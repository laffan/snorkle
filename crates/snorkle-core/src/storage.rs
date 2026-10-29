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

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use url::Url;

use crate::text::{sanitize_page_text, MAX_PAGE_TEXT_CHARS};

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
}

const HISTORY_FILE: &str = "history.json";
const BOOKMARKS_FILE: &str = "bookmarks.json";
const SESSION_FILE: &str = "session.json";

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
        Ok(Store { dir, inner: Mutex::new(Data { history, bookmarks }) })
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
