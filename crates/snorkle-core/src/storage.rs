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
    settings: Settings,
    user_scripts: Vec<UserScript>,
}

/// User preferences that native code needs (UI-only preferences such as the
/// sidebar width stay in the chrome's localStorage).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Render pages in black and white.
    pub grayscale: bool,
}

const HISTORY_FILE: &str = "history.json";
const BOOKMARKS_FILE: &str = "bookmarks.json";
const SESSION_FILE: &str = "session.json";
const ADBLOCK_ALLOW_FILE: &str = "adblock-allow.json";
const SETTINGS_FILE: &str = "settings.json";
const USER_SCRIPTS_FILE: &str = "user-scripts.json";

impl Store {
    /// Opens (or creates) the store. Corrupt files are moved aside to
    /// `<name>.corrupt` and replaced with empty data.
    pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
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
        let settings: Settings = load_or_reset(&dir.join(SETTINGS_FILE));
        let mut user_scripts: Vec<UserScript> = load_or_reset::<Vec<UserScript>>(&dir.join(USER_SCRIPTS_FILE))
            .into_iter()
            .filter_map(|s| userscripts::validate(s).ok())
            .collect();
        user_scripts.truncate(userscripts::MAX_SCRIPTS);
        Ok(Store { dir, inner: Mutex::new(Data { history, bookmarks, adblock_allowed, settings, user_scripts }) })
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

    pub fn settings(&self) -> Settings {
        self.data().settings.clone()
    }

    pub fn set_settings(&self, settings: Settings) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            d.settings = settings;
            serde_json::to_vec(&d.settings)?
        };
        atomic_write(&self.dir.join(SETTINGS_FILE), &snapshot)
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
    fn user_scripts_and_settings_roundtrip() {
        let dir = tmpdir("userscripts");
        let s = Store::open(&dir).unwrap();
        let us = |d: &str, js: &str| UserScript { domain: d.into(), css: String::new(), js: js.into(), enabled: true };
        assert_eq!(s.save_user_script(us("Example.com", "a()")).unwrap().domain, "example.com");
        s.save_user_script(us("example.com", "b()")).unwrap();
        assert!(s.save_user_script(us("not a domain", "c()")).is_err());
        assert_eq!(s.user_scripts().len(), 1);
        assert_eq!(s.user_scripts()[0].js, "b()");
        s.set_settings(Settings { grayscale: true }).unwrap();
        drop(s);
        fs::write(
            dir.join(USER_SCRIPTS_FILE),
            r#"[{"domain":"ok.example","css":"","js":"x()","enabled":true},{"domain":"bad\"]","css":"","js":"","enabled":true}]"#,
        )
        .unwrap();
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.user_scripts().len(), 1, "tampered entries are dropped on load");
        assert!(s.settings().grayscale);
        s.delete_user_script("ok.example").unwrap();
        assert!(s.user_scripts().is_empty());
    }

    #[test]
    fn session_roundtrip() {
        let dir = tmpdir("session");
        let s = Store::open(&dir).unwrap();
        let sess = Session {
            tabs: vec![
                SessionTab { url: "https://a.example/".into(), title: "a".into() },
                SessionTab { url: "file:///x".into(), title: "f".into() },
            ],
            active: Some(1),
        };
        s.save_session(&sess).unwrap();
        let loaded = s.load_session();
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.active, None);
    }
}
