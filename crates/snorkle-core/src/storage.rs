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

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use url::Url;

use crate::text::{sanitize_page_text, MAX_PAGE_TEXT_CHARS};
use crate::userscripts::{self, UserScript, UserScriptError};

pub const MAX_HISTORY: usize = 5_000;
pub const MAX_BOOKMARKS: usize = 5_000;
pub const MAX_SESSION_TABS: usize = 500;
/// Pinned grid at the top of the sidebar.
pub const MAX_PINS: usize = 4;
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
    pins: Vec<Bookmark>,
}

const HISTORY_FILE: &str = "history.json";
const BOOKMARKS_FILE: &str = "bookmarks.json";
const SESSION_FILE: &str = "session.json";
const ADBLOCK_ALLOW_FILE: &str = "adblock-allow.json";
const USER_SCRIPTS_FILE: &str = "user-scripts.json";
const PINS_FILE: &str = "pins.json";
/// Versioned: older caches are deleted and icons fetched again (v1: raw
/// snapshot encoding the chrome could fail to display; v2: icons offset
/// by the snapshot view's desktop layout).
const ICONS_DIR: &str = "icons-v3";
const OLD_ICONS_DIRS: &[&str] = &["icons", "icons-v2"];

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
        Ok(Store { dir, inner: Mutex::new(Data { history, bookmarks, adblock_allowed, user_scripts, pins }) })
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
        self.data().history.clear();
        atomic_write(&self.dir.join(HISTORY_FILE), b"[]")
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
        let (removed, snapshot) = {
            let mut d = self.data();
            let removed = d.pins.iter().position(|b| b.url == url).map(|i| d.pins.remove(i));
            (removed, serde_json::to_vec(&d.pins)?)
        };
        atomic_write(&self.dir.join(PINS_FILE), &snapshot)?;
        Ok(removed)
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
    fn pins_are_capped_and_movable() {
        let dir = tmpdir("pins");
        let s = Store::open(&dir).unwrap();
        for i in 0..6 {
            s.insert_pin(&format!("https://p{i}.example/"), "p", 99, 0).unwrap();
        }
        assert_eq!(s.pins().len(), MAX_PINS);
        assert!(!s.insert_pin("https://p0.example/", "dup", 0, 0).unwrap());
        s.move_pin("https://p3.example/", 0).unwrap();
        assert_eq!(s.pins()[0].url, "https://p3.example/");
        assert!(s.remove_pin("https://p3.example/").unwrap().is_some());
        assert_eq!(s.pins().len(), 3);
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
                SessionTab { url: "https://a.example/".into(), title: "a".into(), launcher: None },
                SessionTab { url: "file:///x".into(), title: "f".into(), launcher: None },
            ],
            active: Some(1),
        };
        s.save_session(&sess).unwrap();
        let loaded = s.load_session();
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.active, None);
    }
}
