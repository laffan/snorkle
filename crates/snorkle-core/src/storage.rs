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
use crate::viewmodes::{self, HeadingRule, HeadingRules};

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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Session {
    pub tabs: Vec<SessionTab>,
    pub active: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
}

#[derive(Default)]
struct Data {
    history: Vec<HistoryEntry>,
    bookmarks: Vec<Bookmark>,
    /// Sites (registrable domains) where the user turned ad blocking off.
    adblock_allowed: BTreeSet<String>,
    user_scripts: Vec<UserScript>,
    heading_rules: HeadingRules,
    ai_usage: Vec<AiUsageEvent>,
    pins: Vec<Bookmark>,
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
    c.url = storable_url(&c.url)?;
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
const HEADING_RULES_FILE: &str = "heading-rules.json";
const AI_USAGE_FILE: &str = "ai-usage.json";
/// View modes each launch starts with (Settings → General).
const LAUNCH_MODES_FILE: &str = "view-modes.json";
/// Settings → Blocked: patterns and the message.
const BLOCKED_FILE: &str = "blocked.json";
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
/// Sites with detected heading rules kept.
pub const MAX_HEADING_RULE_SITES: usize = 500;
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
const ICONS_DIR: &str = "icons-v4";
const OLD_ICONS_DIRS: &[&str] = &["icons", "icons-v2", "icons-v3"];

impl Store {
    /// Opens (or creates) the store. Corrupt files are moved aside to
    /// `<name>.corrupt` and replaced with empty data.
    pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        for old in OLD_ICONS_DIRS {
            let _ = fs::remove_dir_all(dir.join(old));
        }
        let mut history: Vec<HistoryEntry> = load_or_reset(&dir.join(HISTORY_FILE));
        history.retain_mut(|h| match storable_url(&h.url) {
            Some(u) => {
                h.url = u;
                h.title = clean_title(&h.title);
                true
            }
            None => false,
        });
        history.truncate(MAX_HISTORY);
        let mut bookmarks: Vec<Bookmark> = load_or_reset(&dir.join(BOOKMARKS_FILE));
        bookmarks.retain_mut(|b| match storable_url(&b.url) {
            Some(u) => {
                b.url = u;
                b.title = clean_title(&b.title);
                true
            }
            None => false,
        });
        bookmarks.truncate(MAX_BOOKMARKS);
        let adblock_allowed: BTreeSet<String> = load_or_reset::<BTreeSet<String>>(&dir.join(ADBLOCK_ALLOW_FILE))
            .iter()
            .filter_map(|s| crate::blocking::normalise_site(s))
            .take(MAX_BOOKMARKS)
            .collect();
        let mut user_scripts: Vec<UserScript> = load_or_reset::<Vec<UserScript>>(&dir.join(USER_SCRIPTS_FILE))
            .into_iter()
            .filter_map(|s| userscripts::validate(s).ok())
            .collect();
        user_scripts.truncate(userscripts::MAX_SCRIPTS);
        let mut pins: Vec<Bookmark> = load_or_reset(&dir.join(PINS_FILE));
        pins.retain_mut(|b| match storable_url(&b.url) {
            Some(u) => {
                b.url = u;
                b.title = clean_title(&b.title);
                true
            }
            None => false,
        });
        pins.truncate(MAX_PINS);
        let heading_rules = load_or_reset::<HeadingRules>(&dir.join(HEADING_RULES_FILE))
            .into_iter()
            .filter_map(|(site, rules)| Some((crate::blocking::normalise_site(&site)?, valid_rules(&rules))))
            .filter(|(_, rules)| !rules.is_empty())
            .take(MAX_HEADING_RULE_SITES)
            .collect();
        let mut ai_usage: Vec<AiUsageEvent> = load_or_reset::<Vec<AiUsageEvent>>(&dir.join(AI_USAGE_FILE))
            .into_iter()
            .filter(|e| crate::ai::validate_model(&e.model).is_ok())
            .collect();
        let excess = ai_usage.len().saturating_sub(MAX_AI_USAGE_EVENTS);
        ai_usage.drain(..excess);
        let chats = load_chats(&dir.join(CHATS_DIR));
        let launch_modes = load_or_reset(&dir.join(LAUNCH_MODES_FILE));
        let blocked = crate::blocklist::BlockList::from_settings(load_or_reset(&dir.join(BLOCKED_FILE))).settings().clone();
        let mut pin_emoji: BTreeMap<String, String> = load_or_reset(&dir.join(PIN_EMOJI_FILE));
        pin_emoji.retain(|url, e| pins.iter().any(|p| &p.url == url) && clean_emoji(e).as_deref() == Some(e.as_str()));
        let offline = load_offline(&dir.join(OFFLINE_DIR), |u| {
            pins.iter().any(|p| p.url == u) || bookmarks.iter().any(|b| b.url == u)
        });
        let page_texts = load_page_texts(&dir.join(PAGE_TEXT_DIR), |u| history.iter().any(|h| h.url == u));
        let downloads = load_downloads(&dir);
        let next_download = downloads.iter().map(|d| d.id).max().unwrap_or(0) + 1;
        Ok(Store {
            dir,
            inner: Mutex::new(Data {
                history,
                bookmarks,
                adblock_allowed,
                user_scripts,
                heading_rules,
                ai_usage,
                pins,
                chats,
                launch_modes,
                blocked,
                pin_emoji,
                offline,
                page_texts,
                downloads,
                next_download,
            }),
        })
    }

    fn data(&self) -> std::sync::MutexGuard<'_, Data> {
        // A poisoned lock means a panic mid-update; with panic=abort this is
        // unreachable, but recover the data rather than propagating.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn add_history(&self, url: &str, title: &str, now_ms: u64) -> std::io::Result<()> {
        let Some(url) = storable_url(url) else { return Ok(()) };
        let snapshot = {
            let mut d = self.data();
            if d.history.first().map(|h| h.url == url).unwrap_or(false) {
                let first = &mut d.history[0];
                first.title = clean_title(title);
                first.visited_ms = now_ms;
            } else {
                d.history.insert(0, HistoryEntry { url, title: clean_title(title), visited_ms: now_ms });
                d.history.truncate(MAX_HISTORY);
            }
            serde_json::to_vec(&d.history)?
        };
        atomic_write(&self.dir.join(HISTORY_FILE), &snapshot)
    }

    pub fn history(&self, limit: usize) -> Vec<HistoryEntry> {
        self.data().history.iter().take(limit).cloned().collect()
    }

    pub fn clear_history(&self) -> std::io::Result<()> {
        {
            let mut d = self.data();
            d.history.clear();
            d.page_texts.clear();
        }
        let _ = fs::remove_dir_all(self.dir.join(PAGE_TEXT_DIR));
        atomic_write(&self.dir.join(HISTORY_FILE), b"[]")
    }

    /// Whether `url` (normalised) is in history.
    pub fn in_history(&self, url: &str) -> bool {
        storable_url(url).is_some_and(|u| self.data().history.iter().any(|h| h.url == u))
    }

    /// Removes the visits `(url, visited_ms)` from history; a page with no
    /// visits left also loses its saved text. Returns how many were removed.
    pub fn remove_history(&self, visits: &[(String, u64)]) -> std::io::Result<usize> {
        let (snapshot, index, dropped, removed) = {
            let mut d = self.data();
            let before = d.history.len();
            d.history.retain(|h| !visits.iter().any(|(u, ms)| *u == h.url && *ms == h.visited_ms));
            let removed = before - d.history.len();
            if removed == 0 {
                return Ok(0);
            }
            let Data { history, page_texts, .. } = &mut *d;
            let dropped: Vec<String> = page_texts
                .iter()
                .filter(|e| !history.iter().any(|h| h.url == e.url))
                .map(|e| e.file.clone())
                .collect();
            page_texts.retain(|e| !dropped.contains(&e.file));
            let index = (!dropped.is_empty()).then(|| serde_json::to_vec(&d.page_texts)).transpose()?;
            (serde_json::to_vec(&d.history)?, index, dropped, removed)
        };
        atomic_write(&self.dir.join(HISTORY_FILE), &snapshot)?;
        let dir = self.dir.join(PAGE_TEXT_DIR);
        for f in dropped {
            let _ = fs::remove_file(dir.join(f));
        }
        if let Some(index) = index {
            atomic_write(&dir.join(OFFLINE_INDEX), &index)?;
        }
        Ok(removed)
    }

    /// Saves the text of a page in history (replacing its previous text)
    /// and deletes the oldest beyond [`MAX_PAGE_TEXTS`]. False when `url`
    /// is not in history or there is no text.
    pub fn save_page_text(&self, url: &str, title: &str, text: &str, now_ms: u64) -> std::io::Result<bool> {
        let Some(url) = storable_url(url) else { return Ok(false) };
        let text = crate::ai::clean_multiline(text, MAX_PAGE_TEXT_STORED_CHARS);
        if text.is_empty() {
            return Ok(false);
        }
        let page = SavedPageText { url: url.clone(), title: clean_title(title), text, saved_ms: now_ms };
        let bytes = serde_json::to_vec(&page)?;
        if bytes.len() as u64 > MAX_PAGE_TEXT_FILE_BYTES {
            return Ok(false);
        }
        let dir = self.dir.join(PAGE_TEXT_DIR);
        let file = page_text_file(&url);
        let (index, evicted) = {
            let mut d = self.data();
            if !d.history.iter().any(|h| h.url == url) {
                return Ok(false);
            }
            fs::create_dir_all(&dir)?;
            atomic_write(&dir.join(&file), &bytes)?;
            d.page_texts.retain(|e| e.url != url && e.file != file);
            d.page_texts.insert(0, PageTextEntry { url, file, saved_ms: now_ms, bytes: bytes.len() as u64 });
            let evicted: Vec<String> = d.page_texts.iter().skip(MAX_PAGE_TEXTS).map(|e| e.file.clone()).collect();
            d.page_texts.truncate(MAX_PAGE_TEXTS);
            (serde_json::to_vec(&d.page_texts)?, evicted)
        };
        for f in evicted {
            let _ = fs::remove_file(dir.join(f));
        }
        atomic_write(&dir.join(OFFLINE_INDEX), &index)?;
        Ok(true)
    }

    /// A new download's id and the path its file goes to (its folder is
    /// created). `suggested` is cleaned ([`clean_download_name`]).
    pub fn new_download(&self, suggested: &str) -> std::io::Result<(u64, String, PathBuf)> {
        let id = {
            let mut d = self.data();
            let id = d.next_download;
            d.next_download += 1;
            id
        };
        let name = clean_download_name(suggested);
        let folder = self.dir.join(DOWNLOADS_DIR).join(id.to_string());
        fs::create_dir_all(&folder)?;
        Ok((id, name.clone(), folder.join(name)))
    }

    /// The folder downloads are kept in (created if missing).
    pub fn downloads_root(&self) -> std::io::Result<PathBuf> {
        let root = self.dir.join(DOWNLOADS_DIR);
        fs::create_dir_all(&root)?;
        Ok(root)
    }

    /// Where download `id`'s file named `name` is (whether or not it exists).
    pub fn download_path(&self, id: u64, name: &str) -> Option<PathBuf> {
        (clean_download_name(name) == name).then(|| self.dir.join(DOWNLOADS_DIR).join(id.to_string()).join(name))
    }

    /// Finished downloads of non-private tabs, newest first.
    pub fn downloads(&self) -> Vec<DownloadRecord> {
        self.data().downloads.clone()
    }

    /// Records a finished download (newest first); beyond
    /// [`MAX_DOWNLOAD_RECORDS`] the oldest are deleted with their files.
    pub fn record_download(&self, mut rec: DownloadRecord) -> std::io::Result<()> {
        rec.name = clean_download_name(&rec.name);
        rec.source = storable_url(&rec.source).unwrap_or_default();
        rec.mime = sanitize_page_text(&rec.mime, 100);
        rec.error = sanitize_page_text(&rec.error, 200);
        let (snapshot, evicted) = {
            let mut d = self.data();
            d.downloads.retain(|r| r.id != rec.id);
            d.downloads.insert(0, rec);
            let evicted: Vec<u64> = d.downloads.iter().skip(MAX_DOWNLOAD_RECORDS).map(|r| r.id).collect();
            d.downloads.truncate(MAX_DOWNLOAD_RECORDS);
            (serde_json::to_vec(&d.downloads)?, evicted)
        };
        for id in evicted {
            let _ = fs::remove_dir_all(self.dir.join(DOWNLOADS_DIR).join(id.to_string()));
        }
        atomic_write(&self.dir.join(DOWNLOADS_FILE), &snapshot)
    }

    /// Deletes a download's file (and its record, if it has one).
    pub fn delete_download(&self, id: u64) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            let before = d.downloads.len();
            d.downloads.retain(|r| r.id != id);
            (d.downloads.len() != before).then(|| serde_json::to_vec(&d.downloads)).transpose()?
        };
        let _ = fs::remove_dir_all(self.dir.join(DOWNLOADS_DIR).join(id.to_string()));
        match snapshot {
            Some(s) => atomic_write(&self.dir.join(DOWNLOADS_FILE), &s),
            None => Ok(()),
        }
    }

    /// Size in bytes of each saved page text, by URL.
    pub fn page_text_sizes(&self) -> BTreeMap<String, u64> {
        self.data().page_texts.iter().map(|e| (e.url.clone(), e.bytes)).collect()
    }

    /// The saved text of `url`, if any.
    pub fn page_text(&self, url: &str) -> Option<SavedPageText> {
        let url = storable_url(url)?;
        let file = self.data().page_texts.iter().find(|e| e.url == url)?.file.clone();
        read_page_text(&self.dir.join(PAGE_TEXT_DIR).join(file)).filter(|p| p.url == url)
    }

    /// Saved pages whose text contains `query` (case-insensitive), newest
    /// first, with a snippet around the first match.
    pub fn search_page_texts(&self, query: &str, limit: usize) -> Vec<TextHit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let files: Vec<PageTextEntry> = self.data().page_texts.clone();
        let dir = self.dir.join(PAGE_TEXT_DIR);
        files
            .iter()
            .filter_map(|e| {
                let p = read_page_text(&dir.join(&e.file)).filter(|p| p.url == e.url)?;
                Some(TextHit { url: e.url.clone(), snippet: snippet(&p.text, &q)? })
            })
            .take(limit)
            .collect()
    }

    /// Adds a bookmark; returns false if the URL is not storable or already
    /// bookmarked.
    pub fn add_bookmark(&self, url: &str, title: &str, now_ms: u64) -> std::io::Result<bool> {
        let Some(url) = storable_url(url) else { return Ok(false) };
        let snapshot = {
            let mut d = self.data();
            if d.bookmarks.iter().any(|b| b.url == url) || d.bookmarks.len() >= MAX_BOOKMARKS {
                return Ok(false);
            }
            d.bookmarks.push(Bookmark { url, title: clean_title(title), created_ms: now_ms });
            serde_json::to_vec(&d.bookmarks)?
        };
        atomic_write(&self.dir.join(BOOKMARKS_FILE), &snapshot)?;
        Ok(true)
    }

    /// Bookmarks `url` at position `index` (clamped). Returns false if the
    /// URL is not storable or already bookmarked.
    pub fn insert_bookmark(&self, url: &str, title: &str, index: usize, now_ms: u64) -> std::io::Result<bool> {
        let Some(url) = storable_url(url) else { return Ok(false) };
        let snapshot = {
            let mut d = self.data();
            if d.bookmarks.iter().any(|b| b.url == url) || d.bookmarks.len() >= MAX_BOOKMARKS {
                return Ok(false);
            }
            let at = index.min(d.bookmarks.len());
            d.bookmarks.insert(at, Bookmark { url, title: clean_title(title), created_ms: now_ms });
            serde_json::to_vec(&d.bookmarks)?
        };
        atomic_write(&self.dir.join(BOOKMARKS_FILE), &snapshot)?;
        Ok(true)
    }

    /// Changes the URL of a pin or bookmark ("Edit URL"), keeping its title,
    /// position and emoji; its offline copy is deleted (it was of the old
    /// page). Returns the stored (normalised) new URL, or `None` when `old`
    /// is neither pinned nor bookmarked, `new` is not an http(s) URL, or
    /// it is already pinned or bookmarked.
    pub fn change_item_url(&self, old: &str, new: &str) -> std::io::Result<Option<String>> {
        self.edit_item(old, new, None)
    }

    /// Edits a saved item using the same URL rules, with an optional title
    /// sanitised like other bookmark titles. Title-only edits keep offline data.
    pub fn edit_item(&self, old: &str, new: &str, title: Option<&str>) -> std::io::Result<Option<String>> {
        let Some(new) = storable_url(new) else { return Ok(None) };
        let url_changed = new != old;
        let (pins, bookmarks, emoji, offline, dropped) = {
            let mut d = self.data();
            let taken = url_changed && d.pins.iter().chain(d.bookmarks.iter()).any(|b| b.url == new);
            let in_pins = d.pins.iter().position(|b| b.url == old);
            let in_bookmarks = d.bookmarks.iter().position(|b| b.url == old);
            if taken || (in_pins.is_none() && in_bookmarks.is_none()) {
                return Ok(None);
            }
            if let Some(i) = in_pins {
                d.pins[i].url = new.clone();
                if let Some(title) = title {
                    d.pins[i].title = clean_title(title);
                }
            }
            if let Some(i) = in_bookmarks {
                d.bookmarks[i].url = new.clone();
                if let Some(title) = title {
                    d.bookmarks[i].title = clean_title(title);
                }
            }
            let emoji = match url_changed.then(|| d.pin_emoji.remove(old)).flatten() {
                Some(e) => {
                    d.pin_emoji.insert(new.clone(), e);
                    Some(serde_json::to_vec(&d.pin_emoji)?)
                }
                None => None,
            };
            let dropped: Vec<String> =
                d.offline.iter().filter(|e| url_changed && e.url == old).map(|e| e.file.clone()).collect();
            d.offline.retain(|e| !url_changed || e.url != old);
            let offline = (!dropped.is_empty()).then(|| serde_json::to_vec(&d.offline)).transpose()?;
            (
                in_pins.map(|_| serde_json::to_vec(&d.pins)).transpose()?,
                in_bookmarks.map(|_| serde_json::to_vec(&d.bookmarks)).transpose()?,
                emoji,
                offline,
                dropped,
            )
        };
        if let Some(p) = pins {
            atomic_write(&self.dir.join(PINS_FILE), &p)?;
        }
        if let Some(b) = bookmarks {
            atomic_write(&self.dir.join(BOOKMARKS_FILE), &b)?;
        }
        if let Some(e) = emoji {
            atomic_write(&self.dir.join(PIN_EMOJI_FILE), &e)?;
        }
        if let Some(o) = offline {
            let dir = self.dir.join(OFFLINE_DIR);
            for f in dropped {
                let _ = fs::remove_file(dir.join(f));
            }
            atomic_write(&dir.join(OFFLINE_INDEX), &o)?;
        }
        Ok(Some(new))
    }

    /// Moves the bookmark for `url` to position `index` (clamped).
    pub fn move_bookmark(&self, url: &str, index: usize) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            let Some(from) = d.bookmarks.iter().position(|b| b.url == url) else { return Ok(()) };
            let b = d.bookmarks.remove(from);
            let at = index.min(d.bookmarks.len());
            d.bookmarks.insert(at, b);
            serde_json::to_vec(&d.bookmarks)?
        };
        atomic_write(&self.dir.join(BOOKMARKS_FILE), &snapshot)
    }

    pub fn pins(&self) -> Vec<Bookmark> {
        self.data().pins.clone()
    }

    /// Pins `url` at `index` (clamped). False if not storable, already
    /// pinned, or the grid is full.
    pub fn insert_pin(&self, url: &str, title: &str, index: usize, now_ms: u64) -> std::io::Result<bool> {
        let Some(url) = storable_url(url) else { return Ok(false) };
        let snapshot = {
            let mut d = self.data();
            if d.pins.iter().any(|b| b.url == url) || d.pins.len() >= MAX_PINS {
                return Ok(false);
            }
            let at = index.min(d.pins.len());
            d.pins.insert(at, Bookmark { url, title: clean_title(title), created_ms: now_ms });
            serde_json::to_vec(&d.pins)?
        };
        atomic_write(&self.dir.join(PINS_FILE), &snapshot)?;
        Ok(true)
    }

    pub fn move_pin(&self, url: &str, index: usize) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            let Some(from) = d.pins.iter().position(|b| b.url == url) else { return Ok(()) };
            let b = d.pins.remove(from);
            let at = index.min(d.pins.len());
            d.pins.insert(at, b);
            serde_json::to_vec(&d.pins)?
        };
        atomic_write(&self.dir.join(PINS_FILE), &snapshot)
    }

    /// Removes a pin and returns it.
    pub fn remove_pin(&self, url: &str) -> std::io::Result<Option<Bookmark>> {
        let (removed, snapshot, emoji) = {
            let mut d = self.data();
            let removed = d.pins.iter().position(|b| b.url == url).map(|i| d.pins.remove(i));
            let emoji = d.pin_emoji.remove(url).map(|_| serde_json::to_vec(&d.pin_emoji)).transpose()?;
            (removed, serde_json::to_vec(&d.pins)?, emoji)
        };
        atomic_write(&self.dir.join(PINS_FILE), &snapshot)?;
        if let Some(emoji) = emoji {
            atomic_write(&self.dir.join(PIN_EMOJI_FILE), &emoji)?;
        }
        Ok(removed)
    }

    /// Whether `url` (a committed page URL) is pinned or bookmarked and its
    /// offline copy is missing or older than [`OFFLINE_REFRESH_MS`].
    pub fn offline_copy_wanted(&self, url: &str, now_ms: u64) -> bool {
        let d = self.data();
        let saved = d.pins.iter().any(|p| p.url == url) || d.bookmarks.iter().any(|b| b.url == url);
        saved
            && d.offline
                .iter()
                .find(|e| e.url == url)
                .is_none_or(|e| now_ms.saturating_sub(e.saved_ms) >= OFFLINE_REFRESH_MS)
    }

    /// Stores the offline copy of a pinned or bookmarked page, replacing
    /// its previous one; deletes the oldest copies beyond
    /// [`MAX_OFFLINE_TOTAL_BYTES`]. False when `url` is neither pinned nor
    /// bookmarked or the copy is empty or too large.
    pub fn save_offline_copy(&self, url: &str, data: &[u8], now_ms: u64) -> std::io::Result<bool> {
        if data.is_empty() || data.len() > MAX_OFFLINE_COPY_BYTES {
            return Ok(false);
        }
        let dir = self.dir.join(OFFLINE_DIR);
        let file = offline_file(url);
        let (index, evicted) = {
            let mut d = self.data();
            if !(d.pins.iter().any(|p| p.url == url) || d.bookmarks.iter().any(|b| b.url == url)) {
                return Ok(false);
            }
            fs::create_dir_all(&dir)?;
            atomic_write(&dir.join(&file), data)?;
            d.offline.retain(|e| e.url != url && e.file != file);
            d.offline.push(OfflineEntry { url: url.to_string(), file, saved_ms: now_ms, bytes: data.len() as u64 });
            d.offline.sort_by_key(|e| std::cmp::Reverse(e.saved_ms));
            let mut total = 0u64;
            let mut evicted = Vec::new();
            d.offline.retain(|e| {
                total += e.bytes;
                let keep = total <= MAX_OFFLINE_TOTAL_BYTES;
                if !keep {
                    evicted.push(e.file.clone());
                }
                keep
            });
            (serde_json::to_vec(&d.offline)?, evicted)
        };
        for f in evicted {
            let _ = fs::remove_file(dir.join(f));
        }
        atomic_write(&dir.join(OFFLINE_INDEX), &index)?;
        Ok(true)
    }

    /// The offline copy of `url` and when it was saved, if there is one and
    /// the page is still pinned or bookmarked.
    pub fn offline_copy(&self, url: &str) -> Option<(Vec<u8>, u64)> {
        let entry = {
            let d = self.data();
            let saved = d.pins.iter().any(|p| p.url == url) || d.bookmarks.iter().any(|b| b.url == url);
            d.offline.iter().find(|e| e.url == url && saved).cloned()?
        };
        let path = self.dir.join(OFFLINE_DIR).join(&entry.file);
        if fs::metadata(&path).ok()?.len() > MAX_OFFLINE_COPY_BYTES as u64 {
            return None;
        }
        Some((fs::read(path).ok()?, entry.saved_ms))
    }

    /// Deletes every offline copy (Settings → Cache).
    pub fn clear_offline_copies(&self) {
        self.data().offline.clear();
        let _ = fs::remove_dir_all(self.dir.join(OFFLINE_DIR));
    }

    /// The emoji shown instead of a pin's favicon, if one was chosen.
    pub fn pin_emoji(&self, url: &str) -> Option<String> {
        self.data().pin_emoji.get(url).cloned()
    }

    /// Sets (or with `None` removes) a pin's emoji icon. False when `url`
    /// is not pinned or the emoji is not acceptable ([`clean_emoji`]).
    pub fn set_pin_emoji(&self, url: &str, emoji: Option<&str>) -> std::io::Result<bool> {
        let snapshot = {
            let mut d = self.data();
            if !d.pins.iter().any(|p| p.url == url) {
                return Ok(false);
            }
            match emoji {
                Some(e) => {
                    let Some(e) = clean_emoji(e) else { return Ok(false) };
                    d.pin_emoji.insert(url.to_string(), e);
                }
                None => {
                    d.pin_emoji.remove(url);
                }
            }
            serde_json::to_vec(&d.pin_emoji)?
        };
        atomic_write(&self.dir.join(PIN_EMOJI_FILE), &snapshot)?;
        Ok(true)
    }

    /// Removes a bookmark and returns it (to move it into the pinned grid).
    pub fn take_bookmark(&self, url: &str) -> std::io::Result<Option<Bookmark>> {
        let (removed, snapshot) = {
            let mut d = self.data();
            let removed = d.bookmarks.iter().position(|b| b.url == url).map(|i| d.bookmarks.remove(i));
            (removed, serde_json::to_vec(&d.bookmarks)?)
        };
        atomic_write(&self.dir.join(BOOKMARKS_FILE), &snapshot)?;
        Ok(removed)
    }

    /// Cached favicon PNG for `host` (a validated ASCII host), if any.
    pub fn icon(&self, host: &str) -> Option<Vec<u8>> {
        let host = crate::blocking::normalise_site(host)?;
        let bytes = fs::read(self.dir.join(ICONS_DIR).join(format!("{host}.png"))).ok()?;
        is_png(&bytes).then_some(bytes)
    }

    /// Stores a favicon PNG produced by the favicon fetcher. Rejects
    /// anything that is not a PNG or is too large.
    pub fn save_icon(&self, host: &str, png: &[u8]) -> std::io::Result<bool> {
        let Some(host) = crate::blocking::normalise_site(host) else { return Ok(false) };
        if !is_png(png) {
            return Ok(false);
        }
        let dir = self.dir.join(ICONS_DIR);
        fs::create_dir_all(&dir)?;
        atomic_write(&dir.join(format!("{host}.png")), png)?;
        Ok(true)
    }

    pub fn user_scripts(&self) -> Vec<UserScript> {
        self.data().user_scripts.clone()
    }

    /// Adds or replaces (by domain) a user script after validation.
    pub fn save_user_script(&self, script: UserScript) -> Result<UserScript, UserScriptError> {
        let script = userscripts::validate(script)?;
        let snapshot = {
            let mut d = self.data();
            match d.user_scripts.iter().position(|s| s.domain == script.domain) {
                Some(i) => d.user_scripts[i] = script.clone(),
                None if d.user_scripts.len() >= userscripts::MAX_SCRIPTS => return Err(UserScriptError::TooMany),
                None => d.user_scripts.push(script.clone()),
            }
            d.user_scripts.sort_by(|a, b| a.domain.cmp(&b.domain));
            serde_json::to_vec(&d.user_scripts).map_err(|_| UserScriptError::TooLarge)?
        };
        atomic_write(&self.dir.join(USER_SCRIPTS_FILE), &snapshot).map_err(|_| UserScriptError::TooLarge)?;
        Ok(script)
    }

    /// Archived chats, most recently used first.
    pub fn chats(&self) -> Vec<ChatRecord> {
        let mut c = self.data().chats.clone();
        c.sort_by_key(|c| std::cmp::Reverse(c.updated_ms));
        c
    }

    pub fn chat(&self, id: &str) -> Option<ChatRecord> {
        self.data().chats.iter().find(|c| c.id == id).cloned()
    }

    /// Stores (or replaces) a chat; the least recently used beyond
    /// [`MAX_CHATS`] are deleted. Returns whether it was stored.
    pub fn save_chat(&self, chat: ChatRecord) -> std::io::Result<bool> {
        let Some(chat) = validate_chat(chat) else { return Ok(false) };
        let dir = self.dir.join(CHATS_DIR);
        fs::create_dir_all(&dir)?;
        let bytes = serde_json::to_vec(&chat)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Ok(false);
        }
        let dropped: Vec<String> = {
            let mut d = self.data();
            d.chats.retain(|c| c.id != chat.id);
            d.chats.push(chat.clone());
            d.chats.sort_by_key(|c| std::cmp::Reverse(c.updated_ms));
            let keep = MAX_CHATS.min(d.chats.len());
            d.chats.drain(keep..).map(|c| c.id).collect()
        };
        atomic_write(&dir.join(format!("{}.json", chat.id)), &bytes)?;
        for id in dropped {
            let _ = fs::remove_file(dir.join(format!("{id}.json")));
        }
        Ok(true)
    }

    pub fn delete_chat(&self, id: &str) -> std::io::Result<()> {
        if !valid_chat_id(id) {
            return Ok(());
        }
        self.data().chats.retain(|c| c.id != id);
        match fs::remove_file(self.dir.join(CHATS_DIR).join(format!("{id}.json"))) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    pub fn ai_usage(&self) -> Vec<AiUsageEvent> {
        self.data().ai_usage.clone()
    }

    pub fn record_ai_usage(&self, event: AiUsageEvent) -> std::io::Result<()> {
        if crate::ai::validate_model(&event.model).is_err() {
            return Ok(());
        }
        let snapshot = {
            let mut d = self.data();
            d.ai_usage.push(event);
            let excess = d.ai_usage.len().saturating_sub(MAX_AI_USAGE_EVENTS);
            d.ai_usage.drain(..excess);
            serde_json::to_vec(&d.ai_usage)?
        };
        atomic_write(&self.dir.join(AI_USAGE_FILE), &snapshot)
    }

    pub fn clear_ai_usage(&self) -> std::io::Result<()> {
        self.data().ai_usage.clear();
        atomic_write(&self.dir.join(AI_USAGE_FILE), b"[]")
    }

    /// Detected heading rules per site ("Detect Headers").
    /// The view modes each launch starts with.
    pub fn launch_modes(&self) -> crate::viewmodes::LaunchModes {
        self.data().launch_modes
    }

    /// Changes one launch default and stores them. Returns the result.
    pub fn set_launch_mode(
        &self,
        mode: crate::viewmodes::Kind,
        on: bool,
    ) -> std::io::Result<crate::viewmodes::LaunchModes> {
        let (modes, snapshot) = {
            let mut d = self.data();
            let before = d.launch_modes;
            d.launch_modes.set(mode, on);
            if d.launch_modes == before {
                return Ok(before);
            }
            (d.launch_modes, serde_json::to_vec(&d.launch_modes)?)
        };
        atomic_write(&self.dir.join(LAUNCH_MODES_FILE), &snapshot)?;
        Ok(modes)
    }

    /// The block list as stored (already validated when the store opened).
    pub fn blocked(&self) -> crate::blocklist::BlockSettings {
        self.data().blocked.clone()
    }

    pub fn set_blocked(&self, blocked: &crate::blocklist::BlockSettings) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            d.blocked = blocked.clone();
            serde_json::to_vec(&d.blocked)?
        };
        atomic_write(&self.dir.join(BLOCKED_FILE), &snapshot)
    }

    pub fn heading_rules(&self) -> HeadingRules {
        self.data().heading_rules.clone()
    }

    /// Stores (validated) heading rules for `site`; empty rules remove it.
    pub fn set_heading_rules(&self, site: &str, rules: &[HeadingRule]) -> std::io::Result<bool> {
        let Some(site) = crate::blocking::normalise_site(site) else { return Ok(false) };
        let rules = valid_rules(rules);
        let snapshot = {
            let mut d = self.data();
            if rules.is_empty() {
                d.heading_rules.remove(&site);
            } else if d.heading_rules.len() < MAX_HEADING_RULE_SITES || d.heading_rules.contains_key(&site) {
                d.heading_rules.insert(site, rules);
            } else {
                return Ok(false);
            }
            serde_json::to_vec(&d.heading_rules)?
        };
        atomic_write(&self.dir.join(HEADING_RULES_FILE), &snapshot)?;
        Ok(true)
    }

    pub fn delete_user_script(&self, domain: &str) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            d.user_scripts.retain(|s| s.domain != domain);
            serde_json::to_vec(&d.user_scripts)?
        };
        atomic_write(&self.dir.join(USER_SCRIPTS_FILE), &snapshot)
    }

    pub fn remove_bookmark(&self, url: &str) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            d.bookmarks.retain(|b| b.url != url);
            serde_json::to_vec(&d.bookmarks)?
        };
        atomic_write(&self.dir.join(BOOKMARKS_FILE), &snapshot)
    }

    pub fn bookmarks(&self) -> Vec<Bookmark> {
        self.data().bookmarks.clone()
    }

    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.data().bookmarks.iter().any(|b| b.url == url)
    }

    pub fn save_session(&self, session: &Session) -> std::io::Result<()> {
        let mut s = session.clone();
        s.tabs.retain_mut(|t| match storable_url(&t.url) {
            Some(u) => {
                t.url = u;
                t.title = clean_title(&t.title);
                true
            }
            None => false,
        });
        s.tabs.truncate(MAX_SESSION_TABS);
        if s.active.map(|a| a >= s.tabs.len()).unwrap_or(false) {
            s.active = None;
        }
        atomic_write(&self.dir.join(SESSION_FILE), &serde_json::to_vec(&s)?)
    }

    /// Sites where ad blocking is turned off.
    pub fn adblock_allowed(&self) -> BTreeSet<String> {
        self.data().adblock_allowed.clone()
    }

    /// Turns ad blocking off (`allowed = true`) or back on for `site`.
    /// Returns the updated set. Invalid site keys are ignored.
    pub fn set_adblock_allowed(&self, site: &str, allowed: bool) -> std::io::Result<BTreeSet<String>> {
        let Some(site) = crate::blocking::normalise_site(site) else { return Ok(self.adblock_allowed()) };
        let (set, snapshot) = {
            let mut d = self.data();
            if allowed {
                if d.adblock_allowed.len() < MAX_BOOKMARKS {
                    d.adblock_allowed.insert(site);
                }
            } else {
                d.adblock_allowed.remove(&site);
            }
            (d.adblock_allowed.clone(), serde_json::to_vec(&d.adblock_allowed)?)
        };
        atomic_write(&self.dir.join(ADBLOCK_ALLOW_FILE), &snapshot)?;
        Ok(set)
    }

    pub fn load_session(&self) -> Session {
        let mut s: Session = load_or_reset(&self.dir.join(SESSION_FILE));
        s.tabs.retain(|t| storable_url(&t.url).is_some());
        s.tabs.truncate(MAX_SESSION_TABS);
        if s.active.map(|a| a >= s.tabs.len()).unwrap_or(false) {
            s.active = None;
        }
        s
    }
}

/// Host whose `/favicon.ico` may be fetched for a pinned URL: https only,
/// validated to `[a-z0-9.-]` (safe to embed in HTML and file names).
pub fn favicon_host(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    if u.scheme() != "https" {
        return None;
    }
    match u.host()? {
        url::Host::Domain(d) => crate::blocking::normalise_site(d),
        _ => None,
    }
}

/// A small, plain PNG: signature, then an IHDR of at most 512×512 pixels,
/// 8 bits per channel, RGB or RGBA, not interlaced (what the favicon
/// fetcher produces). Anything else is refused rather than handed to the
/// chrome.
fn valid_rules(rules: &[HeadingRule]) -> Vec<HeadingRule> {
    rules
        .iter()
        .filter_map(|r| viewmodes::validate_rule(&r.selector, r.level))
        .take(viewmodes::MAX_RULES_PER_SITE)
        .collect()
}

fn is_png(bytes: &[u8]) -> bool {
    if bytes.len() > MAX_ICON_BYTES || bytes.len() < 33 || !bytes.starts_with(PNG_SIGNATURE) {
        return false;
    }
    let ihdr = &bytes[8..33];
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let (len, kind, w, h) = (be32(&ihdr[0..4]), &ihdr[4..8], be32(&ihdr[8..12]), be32(&ihdr[12..16]));
    let (depth, color, interlace) = (ihdr[16], ihdr[17], ihdr[20]);
    len == 13 && kind == b"IHDR" && (1..=512).contains(&w) && (1..=512).contains(&h) && depth == 8
        && matches!(color, 2 | 6) && interlace == 0
}

/// Describes a stored icon for the console ("128×128 RGBA, 5321 bytes").
pub fn png_summary(bytes: &[u8]) -> String {
    if bytes.len() < 33 || !bytes.starts_with(PNG_SIGNATURE) {
        return format!("not a PNG, {} bytes", bytes.len());
    }
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    format!(
        "{}×{}, depth {}, colour type {}, interlace {}, {} bytes",
        be32(&bytes[16..20]),
        be32(&bytes[20..24]),
        bytes[24],
        bytes[25],
        bytes[28],
        bytes.len()
    )
}

/// `data:image/png;base64,…` for a validated PNG, for the chrome UI.
pub fn png_data_url(png: &[u8]) -> Option<String> {
    if !is_png(png) {
        return None;
    }
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(22 + png.len().div_ceil(3) * 4);
    out.push_str("data:image/png;base64,");
    for chunk in png.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    Some(out)
}

/// Reads the offline index, keeping entries with a valid file name, an
/// existing file and a URL that `keep` accepts (still pinned or
/// bookmarked); deletes every other file in the folder.
fn load_offline(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<OfflineEntry> {
    let mut index: Vec<OfflineEntry> = load_or_reset(&dir.join(OFFLINE_INDEX));
    index.retain(|e| valid_offline_file(&e.file) && keep(&e.url) && dir.join(&e.file).is_file());
    if let Ok(entries) = fs::read_dir(dir) {
        for f in entries.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if name != OFFLINE_INDEX && !index.iter().any(|e| e.file == name) {
                let _ = fs::remove_file(f.path());
            }
        }
    }
    if let Ok(bytes) = serde_json::to_vec(&index) {
        if dir.is_dir() {
            let _ = atomic_write(&dir.join(OFFLINE_INDEX), &bytes);
        }
    }
    index
}

/// Reads the page-text index, keeping entries with a valid file name, an
/// existing file and a URL still in history (`keep`), newest first and at
/// most [`MAX_PAGE_TEXTS`]; deletes every other file in the folder.
fn load_page_texts(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PageTextEntry> {
    let mut index: Vec<PageTextEntry> = load_or_reset(&dir.join(OFFLINE_INDEX));
    index.retain(|e| {
        valid_page_text_file(&e.file)
            && storable_url(&e.url).as_deref() == Some(e.url.as_str())
            && keep(&e.url)
            && dir.join(&e.file).is_file()
    });
    index.sort_by_key(|e| std::cmp::Reverse(e.saved_ms));
    index.truncate(MAX_PAGE_TEXTS);
    if let Ok(entries) = fs::read_dir(dir) {
        for f in entries.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if name != OFFLINE_INDEX && !index.iter().any(|e| e.file == name) {
                let _ = fs::remove_file(f.path());
            }
        }
    }
    if let Ok(bytes) = serde_json::to_vec(&index) {
        if dir.is_dir() {
            let _ = atomic_write(&dir.join(OFFLINE_INDEX), &bytes);
        }
    }
    index
}

/// Reads the download records, keeping those with a clean name, unique id
/// and an existing file; deletes every folder in `downloads/` without one.
fn load_downloads(dir: &Path) -> Vec<DownloadRecord> {
    let root = dir.join(DOWNLOADS_DIR);
    let mut records: Vec<DownloadRecord> = load_or_reset(&dir.join(DOWNLOADS_FILE));
    let mut seen = BTreeSet::new();
    records.retain(|r| {
        clean_download_name(&r.name) == r.name
            && seen.insert(r.id)
            && (r.outcome != DownloadOutcome::Done || root.join(r.id.to_string()).join(&r.name).is_file())
    });
    records.sort_by_key(|r| std::cmp::Reverse(r.finished_ms));
    records.truncate(MAX_DOWNLOAD_RECORDS);
    if let Ok(entries) = fs::read_dir(&root) {
        for f in entries.flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            let kept = records.iter().any(|r| r.id.to_string() == name && r.outcome == DownloadOutcome::Done);
            if !kept {
                let _ = fs::remove_dir_all(f.path());
            }
        }
    }
    records
}

/// Reads one saved page text, re-cleaned (a tampered file cannot carry
/// markup-relevant control characters or more than the cap).
fn read_page_text(path: &Path) -> Option<SavedPageText> {
    if fs::metadata(path).ok()?.len() > MAX_PAGE_TEXT_FILE_BYTES {
        return None;
    }
    let mut p: SavedPageText = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    p.url = storable_url(&p.url)?;
    p.title = clean_title(&p.title);
    p.text = crate::ai::clean_multiline(&p.text, MAX_PAGE_TEXT_STORED_CHARS);
    Some(p)
}

/// About 160 characters of `text` around the first case-insensitive match
/// of `q` (already lowercase), on one line, with "…" where cut.
fn snippet(text: &str, q: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let qc: Vec<char> = q.chars().collect();
    let at = (0..=lower.len().checked_sub(qc.len())?).find(|&i| lower[i..i + qc.len()] == qc[..])?;
    let start = at.saturating_sub(60);
    let end = (at + qc.len() + 100).min(chars.len());
    let body: String = chars[start..end].iter().map(|c| if c.is_whitespace() { ' ' } else { *c }).collect();
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(body.trim());
    if end < chars.len() {
        out.push('…');
    }
    Some(out)
}

fn load_or_reset<T: DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(meta) = fs::metadata(path) else { return T::default() };
    let parsed = if meta.len() > MAX_FILE_BYTES {
        None
    } else {
        fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<T>(&bytes).ok())
    };
    match parsed {
        Some(v) => v,
        None => {
            let _ = fs::rename(path, path.with_extension("corrupt"));
            T::default()
        }
    }
}

/// Loads every valid chat file (others are deleted), newest first, capped.
fn load_chats(dir: &Path) -> Vec<ChatRecord> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut chats: Vec<ChatRecord> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let ok = path.extension().is_some_and(|x| x == "json")
            && e.metadata().is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE_BYTES);
        let chat = ok
            .then(|| fs::read(&path).ok())
            .flatten()
            .and_then(|b| serde_json::from_slice::<ChatRecord>(&b).ok())
            .filter(|c| path.file_stem().and_then(|s| s.to_str()) == Some(c.id.as_str()))
            .and_then(validate_chat);
        match chat {
            Some(c) => chats.push(c),
            None => {
                let _ = fs::remove_file(&path);
            }
        }
    }
    chats.sort_by_key(|c| std::cmp::Reverse(c.updated_ms));
    let keep = MAX_CHATS.min(chats.len());
    for c in chats.drain(keep..) {
        let _ = fs::remove_file(dir.join(format!("{}.json", c.id)));
    }
    chats
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("snorkle-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn chats_roundtrip_validate_and_cap() {
        use crate::ai::ChatRole;
        let dir = tmpdir("chats");
        let s = Store::open(&dir).unwrap();
        let chat = |id: &str, t: u64| ChatRecord {
            id: id.into(),
            created_ms: t,
            updated_ms: t,
            url: "https://a.example/x".into(),
            title: "A\u{202e}".into(),
            page_text: "Body".into(),
            selection: String::new(),
            messages: vec![
                ChatEntry { role: ChatRole::User, text: "Hi".into() },
                ChatEntry { role: ChatRole::Assistant, text: "Hello".into() },
            ],
            provider: Provider::Anthropic,
            model: "claude-opus-5-5".into(),
            input: 10,
            output: 2,
            cost_micros: 80,
            unpriced: false,
        };
        assert!(s.save_chat(chat("c1", 1)).unwrap());
        assert!(!s.save_chat(chat("../evil", 2)).unwrap(), "ids are file names: validated");
        assert!(!s.save_chat(ChatRecord { url: "javascript:x".into(), ..chat("c3", 3) }).unwrap());
        drop(s);
        let s = Store::open(&dir).unwrap();
        let got = s.chat("c1").unwrap();
        assert_eq!(got.title, "A");
        assert_eq!(got.messages.len(), 2);
        for i in 0..(MAX_CHATS as u64 + 5) {
            s.save_chat(chat(&format!("c{}", 100 + i), 10 + i)).unwrap();
        }
        assert_eq!(s.chats().len(), MAX_CHATS);
        assert!(s.chat("c1").is_none(), "least recently used dropped");
        s.delete_chat("c110").unwrap();
        assert!(s.chat("c110").is_none());
        fs::write(dir.join(CHATS_DIR).join("c9.json"), b"{not json").unwrap();
        drop(s);
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.chats().len(), MAX_CHATS - 1);
        assert!(!dir.join(CHATS_DIR).join("c9.json").exists(), "corrupt chat files are removed");
    }

    #[test]
    fn download_names_are_safe() {
        assert_eq!(clean_download_name("report.pdf"), "report.pdf");
        assert_eq!(clean_download_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_download_name("C:\\Users\\x\\evil.exe"), "evil.exe");
        assert_eq!(clean_download_name(".hidden"), "hidden");
        assert_eq!(clean_download_name("..."), "download");
        assert_eq!(clean_download_name(""), "download");
        assert_eq!(clean_download_name("a\u{202E}gpj.exe"), "agpj.exe", "bidi override removed");
        assert_eq!(clean_download_name("x:y?.txt"), "x-y-.txt");
        let long = format!("{}.tar.gz", "n".repeat(300));
        let c = clean_download_name(&long);
        assert_eq!(c.chars().count(), MAX_DOWNLOAD_NAME_CHARS);
        assert!(c.ends_with(".gz"));
    }

    #[test]
    fn downloads_are_recorded_and_swept() {
        let dir = tmpdir("downloads");
        let _ = fs::remove_dir_all(&dir);
        let s = Store::open(&dir).unwrap();
        let (a, name, path) = s.new_download("../a.pdf").unwrap();
        assert_eq!(name, "a.pdf");
        assert!(path.starts_with(dir.join(DOWNLOADS_DIR)));
        fs::write(&path, b"pdf").unwrap();
        let (b, _, bpath) = s.new_download("partial.zip").unwrap();
        assert!(b > a);
        fs::write(&bpath, b"half").unwrap();
        let rec = DownloadRecord {
            id: a,
            name: name.clone(),
            source: "https://example.com/a.pdf".into(),
            mime: "application/pdf".into(),
            bytes: 3,
            outcome: DownloadOutcome::Done,
            error: String::new(),
            started_ms: 1,
            finished_ms: 2,
        };
        s.record_download(rec.clone()).unwrap();
        assert_eq!(s.download_path(a, &name), Some(path.clone()));
        assert_eq!(s.download_path(a, "../x"), None);
        drop(s);
        // Reopened: the recorded file stays; the unrecorded (interrupted) one goes.
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.downloads(), vec![rec]);
        assert!(path.is_file() && !bpath.exists());
        let (c, _, _) = s.new_download("next").unwrap();
        assert!(c > a, "recorded ids are not reused (an unrecorded one may be: its folder is gone)");
        s.delete_download(a).unwrap();
        assert!(s.downloads().is_empty() && !path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_removal_and_page_text_cache() {
        let dir = tmpdir("pagetext");
        let _ = fs::remove_dir_all(&dir);
        let s = Store::open(&dir).unwrap();
        s.add_history("https://a.example/", "A", 1).unwrap();
        s.add_history("https://b.example/", "B", 2).unwrap();
        s.add_history("https://a.example/", "A", 3).unwrap();
        assert!(!s.save_page_text("https://nothere.example/", "X", "text", 4).unwrap(), "only pages in history");
        assert!(s.save_page_text("https://a.example/", "A", "Hello\n\nNeedle in a haystack", 4).unwrap());
        assert!(s.save_page_text("https://b.example/", "B", "Other text", 5).unwrap());
        assert_eq!(s.page_text("https://a.example/").unwrap().text, "Hello\n\nNeedle in a haystack");
        let hits = s.search_page_texts("NEEDLE", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://a.example/");
        assert!(hits[0].snippet.contains("Needle"));
        assert!(s.page_text_sizes()["https://a.example/"] > 0);
        // One of a's two visits goes: its text stays; the other goes too: gone.
        assert_eq!(s.remove_history(&[("https://a.example/".into(), 1)]).unwrap(), 1);
        assert!(s.page_text("https://a.example/").is_some());
        assert_eq!(s.remove_history(&[("https://a.example/".into(), 3)]).unwrap(), 1);
        assert!(s.page_text("https://a.example/").is_none());
        assert!(!s.in_history("https://a.example/") && s.in_history("https://b.example/"));
        // Survives a reopen; clearing history clears the texts.
        drop(s);
        let s = Store::open(&dir).unwrap();
        assert!(s.page_text("https://b.example/").is_some());
        assert!(s.page_text("https://a.example/").is_none());
        s.clear_history().unwrap();
        assert!(s.page_text("https://b.example/").is_none() && s.page_text_sizes().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn page_text_cache_keeps_the_newest_hundred() {
        let dir = tmpdir("pagetext-cap");
        let _ = fs::remove_dir_all(&dir);
        let s = Store::open(&dir).unwrap();
        for i in 0..(MAX_PAGE_TEXTS as u64 + 5) {
            let url = format!("https://p{i}.example/");
            s.add_history(&url, "t", i).unwrap();
            assert!(s.save_page_text(&url, "t", "words", i).unwrap());
        }
        assert_eq!(s.page_text_sizes().len(), MAX_PAGE_TEXTS);
        assert!(s.page_text("https://p0.example/").is_none());
        assert!(s.page_text(&format!("https://p{}.example/", MAX_PAGE_TEXTS + 4)).is_some());
        let files = fs::read_dir(dir.join(PAGE_TEXT_DIR)).unwrap().count();
        assert_eq!(files, MAX_PAGE_TEXTS + 1, "evicted files deleted (plus the index)");
        assert_eq!(snippet("abc", "abcd"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_roundtrip_and_filtering() {
        let dir = tmpdir("hist");
        let s = Store::open(&dir).unwrap();
        s.add_history("https://a.example/", "A\u{202E}", 1).unwrap();
        s.add_history("javascript:alert(1)", "x", 2).unwrap();
        s.add_history("data:text/html,x", "x", 3).unwrap();
        s.add_history("https://a.example/", "A2", 4).unwrap();
        let h = s.history(10);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].title, "A2");
        drop(s);
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.history(10).len(), 1);
    }

    #[test]
    fn tampered_file_is_sanitised() {
        let dir = tmpdir("tamper");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(BOOKMARKS_FILE),
            r#"[{"url":"javascript:alert(1)","title":"x","created_ms":1},{"url":"https://ok.example","title":"ok","created_ms":1}]"#,
        )
        .unwrap();
        let s = Store::open(&dir).unwrap();
        let b = s.bookmarks();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].url, "https://ok.example/");
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let dir = tmpdir("corrupt");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(HISTORY_FILE), b"{not json").unwrap();
        let s = Store::open(&dir).unwrap();
        assert!(s.history(10).is_empty());
        assert!(dir.join("history.corrupt").exists());
    }

    #[test]
    fn adblock_allowlist_roundtrip_and_validation() {
        let dir = tmpdir("adblock");
        let s = Store::open(&dir).unwrap();
        s.set_adblock_allowed("News.Example", true).unwrap();
        s.set_adblock_allowed("bad\"]}(", true).unwrap();
        assert_eq!(s.adblock_allowed().into_iter().collect::<Vec<_>>(), vec!["news.example".to_string()]);
        drop(s);
        fs::write(dir.join(ADBLOCK_ALLOW_FILE), r#"["ok.example","x(y"]"#).unwrap();
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.adblock_allowed().into_iter().collect::<Vec<_>>(), vec!["ok.example".to_string()]);
        s.set_adblock_allowed("ok.example", false).unwrap();
        assert!(s.adblock_allowed().is_empty());
    }

    #[test]
    fn bookmark_insert_and_move() {
        let dir = tmpdir("bm-order");
        let s = Store::open(&dir).unwrap();
        for (i, u) in ["https://a.example/", "https://b.example/", "https://c.example/"].iter().enumerate() {
            assert!(s.insert_bookmark(u, "t", 99, i as u64).unwrap());
        }
        assert!(s.insert_bookmark("https://z.example/", "t", 0, 9).unwrap());
        assert!(!s.insert_bookmark("https://a.example/", "dup", 0, 9).unwrap());
        s.move_bookmark("https://z.example/", 99).unwrap();
        s.move_bookmark("https://c.example/", 0).unwrap();
        let order: Vec<String> = s.bookmarks().into_iter().map(|b| b.url).collect();
        assert_eq!(order, ["https://c.example/", "https://a.example/", "https://b.example/", "https://z.example/"]);
        drop(s);
        assert_eq!(Store::open(&dir).unwrap().bookmarks().len(), 4);
    }

    #[test]
    fn user_scripts_roundtrip() {
        let dir = tmpdir("userscripts");
        let s = Store::open(&dir).unwrap();
        let us = |d: &str, js: &str| UserScript { domain: d.into(), css: String::new(), js: js.into(), enabled: true };
        assert_eq!(s.save_user_script(us("Example.com", "a()")).unwrap().domain, "example.com");
        s.save_user_script(us("example.com", "b()")).unwrap();
        assert!(s.save_user_script(us("not a domain", "c()")).is_err());
        assert_eq!(s.user_scripts().len(), 1);
        assert_eq!(s.user_scripts()[0].js, "b()");
        drop(s);
        fs::write(
            dir.join(USER_SCRIPTS_FILE),
            r#"[{"domain":"ok.example","css":"","js":"x()","enabled":true},{"domain":"bad\"]","css":"","js":"","enabled":true}]"#,
        )
        .unwrap();
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.user_scripts().len(), 1, "tampered entries are dropped on load");
        s.delete_user_script("ok.example").unwrap();
        assert!(s.user_scripts().is_empty());
    }

    #[test]
    fn block_list_is_stored_and_cleaned() {
        use crate::blocklist::{BlockList, BlockSettings};
        let dir = tmpdir("blocked");
        {
            let s = Store::open(&dir).unwrap();
            assert_eq!(s.blocked(), BlockSettings::default());
            let mut l = BlockList::from_settings(s.blocked());
            l.add("reddit\\.com").unwrap();
            l.set_message("Back to work.");
            s.set_blocked(l.settings()).unwrap();
        }
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.blocked().patterns, ["reddit\\.com"]);
        assert_eq!(s.blocked().message, "Back to work.");
        // A hand-edited file: invalid patterns are dropped on load.
        fs::write(dir.join(BLOCKED_FILE), br#"{"patterns":["(","a\\.com"],"message":""}"#).unwrap();
        assert_eq!(Store::open(&dir).unwrap().blocked().patterns, ["a\\.com"]);
    }

    #[test]
    fn launch_modes_are_stored() {
        use crate::viewmodes::{Kind, LaunchModes};
        let dir = tmpdir("launch-modes");
        {
            let s = Store::open(&dir).unwrap();
            assert_eq!(s.launch_modes(), LaunchModes::default());
            s.set_launch_mode(Kind::Dark, true).unwrap();
            s.set_launch_mode(Kind::HideMedia, false).unwrap();
            // The headers view has no launch default.
            assert_eq!(s.set_launch_mode(Kind::Headers, true).unwrap().modes().headers, false);
        }
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.launch_modes(), LaunchModes { desaturate: true, hide_media: false, dark: true });
        assert!(!s.launch_modes().modes().headers);
        // A partial file keeps the defaults for what it lacks.
        fs::write(dir.join(LAUNCH_MODES_FILE), br#"{"dark":true}"#).unwrap();
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.launch_modes(), LaunchModes { desaturate: true, hide_media: true, dark: true });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn offline_copies() {
        let dir = tmpdir("offline");
        let url = "https://news.example/";
        {
            let s = Store::open(&dir).unwrap();
            assert!(!s.offline_copy_wanted(url, 0), "not pinned or bookmarked");
            assert!(!s.save_offline_copy(url, b"archive", 0).unwrap());
            s.insert_bookmark(url, "News", 0, 0).unwrap();
            assert!(s.offline_copy_wanted(url, 0));
            assert!(!s.save_offline_copy(url, &[], 0).unwrap(), "empty");
            assert!(s.save_offline_copy(url, b"archive", 1_000).unwrap());
            assert!(!s.offline_copy_wanted(url, 1_000 + OFFLINE_REFRESH_MS - 1), "fresh");
            assert!(s.offline_copy_wanted(url, 1_000 + OFFLINE_REFRESH_MS));
            assert_eq!(s.offline_copy(url), Some((b"archive".to_vec(), 1_000)));
            fs::write(dir.join(OFFLINE_DIR).join("stray.txt"), b"x").unwrap();
        }
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.offline_copy(url).map(|c| c.1), Some(1_000), "kept across launches");
        assert!(!dir.join(OFFLINE_DIR).join("stray.txt").exists(), "unlisted files are deleted");
        s.take_bookmark(url).unwrap();
        assert!(s.offline_copy(url).is_none(), "only while bookmarked");
        let s = Store::open(&dir).unwrap();
        assert!(fs::read_dir(dir.join(OFFLINE_DIR)).unwrap().count() == 1, "only the index is left");
        s.insert_pin(url, "News", 0, 0).unwrap();
        s.save_offline_copy(url, b"a2", 5).unwrap();
        s.clear_offline_copies();
        assert!(s.offline_copy(url).is_none() && !dir.join(OFFLINE_DIR).exists());
        assert!(valid_offline_file(&offline_file(url)) && !valid_offline_file("../x.webarchive"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn item_urls_can_be_changed() {
        let dir = tmpdir("edit-url");
        let s = Store::open(&dir).unwrap();
        s.insert_pin("https://p.example/", "P", 0, 0).unwrap();
        s.set_pin_emoji("https://p.example/", Some("📌")).unwrap();
        s.insert_bookmark("https://b.example/", "B", 0, 0).unwrap();
        s.save_offline_copy("https://b.example/", b"x", 1).unwrap();
        assert_eq!(
            s.change_item_url("https://p.example/", "https://p.example/home").unwrap().as_deref(),
            Some("https://p.example/home")
        );
        assert_eq!(s.pins()[0].url, "https://p.example/home");
        assert_eq!(s.pins()[0].title, "P", "title kept");
        assert_eq!(s.pin_emoji("https://p.example/home").as_deref(), Some("📌"), "emoji follows");
        assert!(s.change_item_url("https://b.example/", "https://p.example/home").unwrap().is_none(), "already pinned");
        assert!(s.change_item_url("https://b.example/", "javascript:alert(1)").unwrap().is_none());
        assert!(s.change_item_url("https://none.example/", "https://x.example/").unwrap().is_none());
        assert!(s.change_item_url("https://b.example/", "https://b.example/new").unwrap().is_some());
        assert!(
            s.offline_copy("https://b.example/new").is_none() && s.offline_copy("https://b.example/").is_none(),
            "old copy dropped"
        );
        drop(s);
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.bookmarks()[0].url, "https://b.example/new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pin_emoji() {
        assert_eq!(clean_emoji(" 📰 ").as_deref(), Some("📰"));
        assert_eq!(
            clean_emoji("👨\u{200D}👩\u{200D}👧").as_deref(),
            Some("👨\u{200D}👩\u{200D}👧"),
            "joined sequences survive"
        );
        for bad in ["", "  ", "a b", "\u{202E}x", "x\u{7}y", &"😀".repeat(MAX_EMOJI_CHARS + 1)] {
            assert!(clean_emoji(bad).is_none(), "{bad:?}");
        }
        let dir = tmpdir("pin-emoji");
        {
            let s = Store::open(&dir).unwrap();
            s.insert_pin("https://n.example/", "n", 0, 0).unwrap();
            assert!(!s.set_pin_emoji("https://other.example/", Some("📰")).unwrap(), "only for pins");
            assert!(!s.set_pin_emoji("https://n.example/", Some("not one")).unwrap());
            assert!(s.set_pin_emoji("https://n.example/", Some("📰")).unwrap());
        }
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.pin_emoji("https://n.example/").as_deref(), Some("📰"));
        s.remove_pin("https://n.example/").unwrap();
        assert!(s.pin_emoji("https://n.example/").is_none(), "removed with the pin");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pins_are_capped_and_movable() {
        let dir = tmpdir("pins");
        let s = Store::open(&dir).unwrap();
        for i in 0..MAX_PINS + 2 {
            s.insert_pin(&format!("https://p{i}.example/"), "p", 99, 0).unwrap();
        }
        assert_eq!(s.pins().len(), MAX_PINS);
        assert!(!s.insert_pin("https://p0.example/", "dup", 0, 0).unwrap());
        s.move_pin("https://p3.example/", 0).unwrap();
        assert_eq!(s.pins()[0].url, "https://p3.example/");
        assert!(s.remove_pin("https://p3.example/").unwrap().is_some());
        assert_eq!(s.pins().len(), MAX_PINS - 1);
        assert!(s.insert_bookmark("https://b.example/", "b", 0, 0).unwrap());
        assert_eq!(s.take_bookmark("https://b.example/").unwrap().unwrap().title, "b");
        assert!(s.bookmarks().is_empty());
    }

    #[test]
    fn icons_must_be_small_pngs() {
        let dir = tmpdir("icons");
        let s = Store::open(&dir).unwrap();
        let png = test_png(64, 8, 6);
        assert!(s.save_icon("www.example.com", &png).unwrap());
        // Unusual encodings are refused: 16-bit, palette, too large.
        assert!(!s.save_icon("www.example.com", &test_png(64, 16, 6)).unwrap());
        assert!(!s.save_icon("www.example.com", &test_png(64, 8, 3)).unwrap());
        assert!(!s.save_icon("www.example.com", &test_png(4096, 8, 6)).unwrap());
        assert!(!s.save_icon("www.example.com", &[PNG_SIGNATURE, b"rest"].concat()).unwrap());
        assert_eq!(s.icon("www.example.com").unwrap(), png);
        assert!(!s.save_icon("www.example.com", b"<svg onload=alert(1)>").unwrap());
        assert!(!s.save_icon("../../etc/passwd", &png).unwrap());
        assert!(!s.save_icon("x.example", &[PNG_SIGNATURE, &vec![0u8; MAX_ICON_BYTES]].concat()).unwrap());
        assert!(s.icon("nope.example").is_none());
    }

    /// Signature + IHDR (+ a stand-in for the rest of the file).
    fn test_png(size: u32, depth: u8, color: u8) -> Vec<u8> {
        let mut v = PNG_SIGNATURE.to_vec();
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&[depth, color, 0, 0, 0]);
        v.extend_from_slice(&[0, 0, 0, 0]); // CRC (not checked)
        v
    }

    #[test]
    fn ai_usage_roundtrip_and_reset() {
        let dir = tmpdir("aiusage");
        let s = Store::open(&dir).unwrap();
        let e = AiUsageEvent { at_ms: 1, provider: Provider::Anthropic, model: "claude-opus-5-5".into(), input: 10, output: 5, cost_micros: Some(140) };
        s.record_ai_usage(e.clone()).unwrap();
        s.record_ai_usage(AiUsageEvent { model: "bad model\"".into(), ..e.clone() }).unwrap();
        drop(s);
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.ai_usage(), vec![e]);
        s.clear_ai_usage().unwrap();
        assert!(Store::open(&dir).unwrap().ai_usage().is_empty());
    }

    #[test]
    fn heading_rules_roundtrip_and_are_revalidated() {
        let dir = tmpdir("hrules");
        let s = Store::open(&dir).unwrap();
        let ok = HeadingRule { selector: "h2.hed".into(), level: 2 };
        assert!(s.set_heading_rules("www.NYTimes.com", std::slice::from_ref(&ok)).unwrap());
        assert!(!s.set_heading_rules("not a site", std::slice::from_ref(&ok)).unwrap());
        drop(s);
        fs::write(
            dir.join(HEADING_RULES_FILE),
            r#"{"nytimes.com":[{"selector":"h2.hed","level":2},{"selector":"x{y}","level":1}],"bad site":[{"selector":"h1","level":1}]}"#,
        )
        .unwrap();
        let s = Store::open(&dir).unwrap();
        let rules = s.heading_rules();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules["nytimes.com"], vec![ok]);
        assert!(s.set_heading_rules("nytimes.com", &[]).unwrap());
        assert!(s.heading_rules().is_empty());
    }

    #[test]
    fn old_icon_cache_is_removed() {
        let dir = tmpdir("oldicons");
        fs::create_dir_all(dir.join("icons")).unwrap();
        fs::write(dir.join("icons/www.example.com.png"), test_png(64, 8, 6)).unwrap();
        let s = Store::open(&dir).unwrap();
        assert!(s.icon("www.example.com").is_none(), "refetched in the new format");
        assert!(!dir.join("icons").exists());
    }

    #[test]
    fn favicon_hosts() {
        assert_eq!(favicon_host("https://www.Pinterest.com/x").as_deref(), Some("www.pinterest.com"));
        assert_eq!(favicon_host("http://plain.example/"), None);
        assert_eq!(favicon_host("https://192.168.0.1/"), None);
        assert_eq!(favicon_host("javascript:alert(1)"), None);
    }

    #[test]
    fn data_url_encoding() {
        let png = test_png(16, 8, 6);
        let url = png_data_url(&png).unwrap();
        assert!(url.starts_with("data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAAAAAA"));
        assert!(png_data_url(b"GIF89a").is_none());
    }

    #[test]
    fn session_roundtrip() {
        let dir = tmpdir("session");
        let s = Store::open(&dir).unwrap();
        let sess = Session {
            tabs: vec![
                SessionTab {
                    url: "https://a.example/".into(),
                    title: "a".into(),
                    launcher: None,
                    exempt: vec![],
                    zoom: None,
                    url_bar_attached: false,
                },
                SessionTab { url: "file:///x".into(), title: "f".into(), launcher: None, exempt: vec![], zoom: None, url_bar_attached: false },
            ],
            active: Some(1),
        };
        s.save_session(&sess).unwrap();
        let loaded = s.load_session();
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.active, None);
    }
}
