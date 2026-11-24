//! SECURITY-REVIEW: persistent storage for history, bookmarks and the
//! session (open non-private tabs).
//!
//! Security assumptions:
//! - Files live in the app's private data container. On iOS the container
//!   is protected with `NSFileProtectionComplete` via the
//!   `com.apple.developer.default-data-protection` entitlement (see
//!   `src-tauri/ios/Entitlements.plist`); this module does not encrypt.
//! - Nothing from private tabs is ever passed to this module. That
//!   invariant is enforced by `browser::Browser` (and tested there), not
//!   here.
//! - No credentials, cookies or form data are stored here. Credentials
//!   belong in the iOS Keychain only (docs/ARCHITECTURE.md §Credentials).
//! - Only `http`/`https` URLs are accepted; anything else is dropped on
//!   write and on load, so a tampered file cannot inject `javascript:` etc.
//! - Writes are atomic (temp file + rename) and are performed off the main
//!   thread by the caller.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use url::Url;

use crate::text::{sanitize_page_text, MAX_PAGE_TEXT_CHARS};
use crate::userscripts::{self, UserScript, UserScriptError};
use crate::ai::Provider;

mod files;
mod groups;
mod history;
mod items;
mod settings;

pub use groups::{valid_bookmark_group_id, BookmarkGroup};

pub use files::{data_url, favicon_host, png_data_url, png_summary};
pub(crate) use files::{atomic_write, load_or_reset};
use files::{is_png, load_chats, load_downloads, load_offline, load_page_texts, read_page_text, snippet};

pub const MAX_HISTORY: usize = 5_000;
pub const MAX_BOOKMARKS: usize = 5_000;
pub const MAX_SESSION_TABS: usize = 500;
/// Pinned grid at the top of the sidebar.
pub const MAX_PINS: usize = 12;
/// Largest favicon PNG accepted from the favicon fetcher or from disk.
pub const MAX_ICON_BYTES: usize = 64 * 1024;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
/// Files larger than this are treated as corrupt rather than parsed.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub url: String,
    pub title: String,
    pub visited_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
    pub created_ms: u64,
    /// The bookmark group it is in (`BookmarkGroup::id`; 2026-11-24).
    /// Always `None` for pins (the pinned grid has no groups).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Session {
    pub tabs: Vec<SessionTab>,
    pub active: Option<usize>,
    /// Tab groups (2026-11-24) that have saved tabs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<SessionGroup>,
    /// Research sessions (2026-11-24), kept while they have no saved tabs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub research: Vec<SessionResearch>,
}

/// A tab group as saved (its tabs name it by `id`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGroup {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub collapsed: bool,
}

/// A research session as saved (its tabs name it by `id`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResearch {
    pub id: String,
    pub name: String,
    pub created_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionTab {
    pub url: String,
    pub title: String,
    /// The pin or bookmark (by URL) this tab belongs to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<String>,
    /// View modes turned off for this tab ("Disable for this tab").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exempt: Vec<crate::viewmodes::Kind>,
    /// Page zoom in percent; absent means 100 %.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<u16>,
    #[serde(default)]
    pub url_bar_attached: bool,
    /// The user's own name for the tab (the Edit dialog).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The emoji shown instead of the tab's favicon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    /// The research session the tab belongs to (`SessionResearch::id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
    /// The tab group it is in (`SessionGroup::id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u64>,
}

/// Returns the normalised URL string if `s` is an http(s) URL.
pub fn storable_url(s: &str) -> Option<String> {
    let u = Url::parse(s).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.to_string())
}

fn clean_title(t: &str) -> String {
    sanitize_page_text(t, MAX_PAGE_TEXT_CHARS)
}

/// History/bookmarks/session store backed by JSON files in `dir`.
pub struct Store {
    dir: PathBuf,
    inner: Mutex<Data>,
    /// Save for Later (`later/`).
    later: crate::later::LaterStore,
    /// One Notebook write at a time.
    notebook_lock: Mutex<()>,
}

#[derive(Default)]
struct Data {
    history: Vec<HistoryEntry>,
    bookmarks: Vec<Bookmark>,
    /// Sites (registrable domains) where the user turned ad blocking off.
    adblock_allowed: BTreeSet<String>,
    user_scripts: Vec<UserScript>,
    ai_usage: Vec<AiUsageEvent>,
    pins: Vec<Bookmark>,
    /// Groups of bookmarks (2026-11-24).
    bookmark_groups: Vec<BookmarkGroup>,
    chats: Vec<ChatRecord>,
    launch_modes: crate::viewmodes::LaunchModes,
    /// Settings → Blocked (patterns as stored; compiled by the caller).
    blocked: crate::blocklist::BlockSettings,
    /// Emoji shown instead of a pin's favicon, by pin URL.
    pin_emoji: BTreeMap<String, String>,
    /// Offline copies of pinned and bookmarked pages (index of `offline/`).
    offline: Vec<OfflineEntry>,
    /// Text of the most recently visited pages (index of `page-text/`).
    page_texts: Vec<PageTextEntry>,
    /// Finished downloads kept in `downloads/` (non-private only).
    downloads: Vec<DownloadRecord>,
    /// The next download id (also in-progress and private ones).
    next_download: u64,
}

/// Downloads (2026-11-03): each file is kept in the app's own folder,
/// `downloads/<id>/<name>`, until the user saves it elsewhere or deletes
/// it; `downloads.json` lists the finished ones of non-private tabs.
/// Folders without a record (interrupted or private downloads) are
/// deleted when the store opens.
const DOWNLOADS_DIR: &str = "downloads";
const DOWNLOADS_FILE: &str = "downloads.json";
/// Finished downloads listed; the oldest records (and files) go first.
pub const MAX_DOWNLOAD_RECORDS: usize = 500;
/// All downloaded files together (finished and running, private too);
/// past this, new downloads are refused and a running one is stopped.
pub const MAX_DOWNLOADS_TOTAL_BYTES: u64 = 500 * 1024 * 1024;
/// Characters of a downloaded file's name kept.
pub const MAX_DOWNLOAD_NAME_CHARS: usize = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadOutcome {
    Done,
    Failed,
    Cancelled,
}

/// A finished (or failed) download of a non-private tab.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub id: u64,
    /// File name, as cleaned by [`clean_download_name`].
    pub name: String,
    /// Where it came from: an http(s) URL, or empty (a file the page made).
    pub source: String,
    pub mime: String,
    pub bytes: u64,
    pub outcome: DownloadOutcome,
    /// Why it failed, for the user (sanitised).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    pub started_ms: u64,
    pub finished_ms: u64,
}

/// A safe file name for a download from the name the server or page
/// suggested: its last path component, without control, bidi or invisible
/// characters, `:` and other characters file systems reject, leading dots
/// and spaces; at most [`MAX_DOWNLOAD_NAME_CHARS`] (the extension kept);
/// `download` if nothing is left. It is never a path.
pub fn clean_download_name(suggested: &str) -> String {
    let last = suggested.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = sanitize_page_text(last, 4 * MAX_DOWNLOAD_NAME_CHARS)
        .chars()
        .map(|c| if matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c })
        .collect();
    let name = cleaned.trim().trim_start_matches(['.', ' ']).trim_end_matches(['.', ' ']).to_string();
    let name = if name.chars().count() > MAX_DOWNLOAD_NAME_CHARS {
        let (stem, ext) = match name.rfind('.') {
            Some(i) if name.len() - i <= 12 && i > 0 => (&name[..i], &name[i..]),
            _ => (name.as_str(), ""),
        };
        let keep = MAX_DOWNLOAD_NAME_CHARS - ext.chars().count();
        format!("{}{ext}", stem.chars().take(keep).collect::<String>().trim_end())
    } else {
        name
    };
    if name.is_empty() {
        "download".to_string()
    } else {
        name
    }
}

/// Text copies of recently visited pages (history's "cached version"), in
/// `page-text/`, one JSON file per URL, listed in `page-text/index.json`.
/// Only pages that are in history are kept: removing a page from history
/// (or clearing it) deletes its text. Never written for private tabs (the
/// caller only saves pages of non-private tabs, and history has none).
const PAGE_TEXT_DIR: &str = "page-text";
/// Pages whose text is kept; the oldest are deleted first.
pub const MAX_PAGE_TEXTS: usize = 100;
/// Largest text file accepted (the text itself is capped by the caller).
pub const MAX_PAGE_TEXT_FILE_BYTES: u64 = 512 * 1024;
/// Characters of a page's text kept.
pub const MAX_PAGE_TEXT_STORED_CHARS: usize = crate::ai::MAX_CHAT_PAGE_CHARS;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageTextEntry {
    pub url: String,
    /// File name in `page-text/`: 16 hex digits + `.json`.
    pub file: String,
    pub saved_ms: u64,
    pub bytes: u64,
}

/// A page's saved text, as written by [`Store::save_page_text`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPageText {
    pub url: String,
    pub title: String,
    pub text: String,
    pub saved_ms: u64,
}

/// A history search hit in a page's saved text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextHit {
    pub url: String,
    /// Up to about 160 characters around the first match.
    pub snippet: String,
}

fn page_text_file(url: &str) -> String {
    format!("{}.json", &offline_file(url)[..16])
}

fn valid_page_text_file(name: &str) -> bool {
    name.len() == 16 + ".json".len()
        && name.ends_with(".json")
        && name[..16].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Offline copies (web archives) of pinned and bookmarked pages, in
/// `offline/`, one file per URL, listed in `offline/index.json`.
const OFFLINE_DIR: &str = "offline";
const OFFLINE_INDEX: &str = "index.json";
/// Largest offline copy kept, in bytes.
pub const MAX_OFFLINE_COPY_BYTES: usize = 25 * 1024 * 1024;
/// All offline copies together; the oldest are deleted first.
pub const MAX_OFFLINE_TOTAL_BYTES: u64 = 400 * 1024 * 1024;
/// A page's copy is refreshed at most this often.
pub const OFFLINE_REFRESH_MS: u64 = 10 * 60 * 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfflineEntry {
    pub url: String,
    /// File name in `offline/`: 16 hex digits + `.webarchive`.
    pub file: String,
    pub saved_ms: u64,
    pub bytes: u64,
}

/// File name of a URL's offline copy (FNV-1a of the URL; the index maps
/// the URL, so a collision only replaces the other copy).
fn offline_file(url: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}.webarchive")
}

fn valid_offline_file(name: &str) -> bool {
    name.len() == 16 + ".webarchive".len()
        && name.ends_with(".webarchive")
        && name[..16].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Chat archive: one file per chat in this directory.
const CHATS_DIR: &str = "chats";
/// Chats kept (the least recently used are deleted first).
pub const MAX_CHATS: usize = 60;
pub const MAX_CHAT_MESSAGES: usize = 200;

/// A page chat (Settings never shows it; the chat panel's Archive does).
/// Only chats begun in non-private tabs are stored. It keeps what the
/// first message was asked about: the page's address, title, the visible
/// text sent and the selection at that moment, so the chat can be
/// continued later with the same context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRecord {
    pub id: String,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub url: String,
    pub title: String,
    pub page_text: String,
    pub selection: String,
    pub messages: Vec<ChatEntry>,
    pub provider: Provider,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cost_micros: u64,
    /// Some reply came from a model without a known price.
    pub unpriced: bool,
    /// A Define chat (the new-tab page): the word it is about. Such a chat
    /// has no page: `url`, `page_text` and `selection` are empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub word: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatEntry {
    pub role: crate::ai::ChatRole,
    pub text: String,
}

/// A chat id: `c` and digits/dashes, short (it is also a file name).
pub fn valid_chat_id(id: &str) -> bool {
    id.len() <= 40 && id.starts_with('c') && id[1..].chars().all(|c| c.is_ascii_digit() || c == '-') && id.len() > 1
}

/// Checks and bounds a chat before it is stored or after it is loaded.
pub fn validate_chat(mut c: ChatRecord) -> Option<ChatRecord> {
    if !valid_chat_id(&c.id) || crate::ai::validate_model(&c.model).is_err() {
        return None;
    }
    if c.word.is_empty() {
        c.url = storable_url(&c.url)?;
    } else {
        c.word = crate::ai::clean_word(&c.word)?;
        c.url.clear();
        c.page_text.clear();
        c.selection.clear();
    }
    c.title = clean_title(&c.title);
    c.page_text = crate::ai::clean_multiline(&c.page_text, crate::ai::MAX_CHAT_PAGE_CHARS);
    c.selection = crate::ai::clean_multiline(&c.selection, crate::ai::MAX_CHAT_SELECTION_CHARS);
    let skip = c.messages.len().saturating_sub(MAX_CHAT_MESSAGES);
    c.messages.drain(..skip);
    for m in &mut c.messages {
        m.text = crate::ai::clean_multiline(&m.text, crate::ai::MAX_CHAT_REPLY_CHARS);
    }
    c.messages.retain(|m| !m.text.is_empty());
    Some(c)
}

const HISTORY_FILE: &str = "history.json";
const BOOKMARKS_FILE: &str = "bookmarks.json";
const SESSION_FILE: &str = "session.json";
const ADBLOCK_ALLOW_FILE: &str = "adblock-allow.json";
const USER_SCRIPTS_FILE: &str = "user-scripts.json";
/// Detect Headers' per-site rules (the headers view, replaced by the
/// summary view on 2026-11-16): deleted when the store opens.
const OLD_HEADING_RULES_FILE: &str = "heading-rules.json";
const AI_USAGE_FILE: &str = "ai-usage.json";
/// View modes each launch starts with (Settings → General).
const LAUNCH_MODES_FILE: &str = "view-modes.json";
/// Settings → Blocked: patterns and the message.
const BLOCKED_FILE: &str = "blocked.json";
const NOTEBOOK_FILE: &str = "notebook.json";
/// AI requests remembered for the usage table (oldest dropped first).
pub const MAX_AI_USAGE_EVENTS: usize = 20_000;

/// One AI request's token use (Settings → AI → Usage). No content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiUsageEvent {
    /// Wall-clock milliseconds.
    pub at_ms: u64,
    pub provider: Provider,
    pub model: String,
    pub input: u64,
    pub output: u64,
    /// Estimated cost in millionths of a dollar; `None` if the model's price
    /// is unknown.
    pub cost_micros: Option<u64>,
}
const PINS_FILE: &str = "pins.json";
/// Emoji chosen for pins ("Use Emoji for Icon"), by pin URL.
const PIN_EMOJI_FILE: &str = "pin-emoji.json";
/// Characters (code points) an icon emoji may have: enough for joined
/// sequences (families, flags, skin tones).
pub const MAX_EMOJI_CHARS: usize = 12;

/// An emoji (or a short symbol) to show instead of a pin's favicon:
/// trimmed, at most [`MAX_EMOJI_CHARS`] code points and 64 bytes, no
/// whitespace, control or bidi-override characters. Zero-width joiners and
/// variation selectors are kept (emoji sequences need them). The chrome
/// shows it with `textContent`.
pub fn clean_emoji(s: &str) -> Option<String> {
    let t = s.trim();
    let bad = |c: char| {
        c.is_control()
            || c.is_whitespace()
            || matches!(c, '\u{061C}' | '\u{200B}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
    };
    (!t.is_empty() && t.len() <= 64 && t.chars().count() <= MAX_EMOJI_CHARS && !t.chars().any(bad))
        .then(|| t.to_string())
}
/// Versioned: older caches are deleted and icons fetched again (v1: raw
/// snapshot encoding the chrome could fail to display; v2: icons offset
/// by the snapshot view's desktop layout; v3: root-only lookup and blank snapshots).
/// v4: one copy per icon; v5 (2026-11-24) adds a dark-appearance copy of
/// icons too dark to see there, so icons are fetched once more.
const ICONS_DIR: &str = "icons-v5";
const OLD_ICONS_DIRS: &[&str] = &["icons", "icons-v2", "icons-v3", "icons-v4"];


#[cfg(test)]
mod tests;
#[cfg(test)]
mod groups_tests;
