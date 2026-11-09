//! Save for Later (owner request 2026-11-09): a tab is put aside as its
//! address, title and a thumbnail of what it showed, and closed. The list
//! lives in `later/index.json`, each thumbnail in `later/<id>.jpg`.
//!
//! Only non-private pages are saved (the caller checks). Thumbnails are
//! JPEG bytes the app drew itself from a snapshot of its own web view
//! (`snorkle_content`); they are checked again here (magic bytes, size)
//! before they are written, and served to the chrome only as a
//! `data:image/jpeg;base64,…` URL.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::storage::{atomic_write, load_or_reset, storable_url};
use crate::text::sanitize_page_text;

/// Pages kept; the oldest (and their thumbnails) are dropped first.
pub const MAX_LATER: usize = 200;
/// Largest thumbnail accepted.
pub const MAX_THUMB_BYTES: usize = 400 * 1024;
const INDEX_FILE: &str = "index.json";
const TITLE_CHARS: usize = 300;

/// One saved page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaterItem {
    pub id: String,
    pub url: String,
    pub title: String,
    pub saved_ms: u64,
    /// Whether `<id>.jpg` exists.
    pub thumb: bool,
}

/// An id: `l` and digits/dashes, short (it is also a file name).
pub fn valid_id(id: &str) -> bool {
    id.len() > 1 && id.len() <= 40 && id.starts_with('l') && id[1..].chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// A JPEG the size of a thumbnail: starts with the SOI marker and fits.
pub fn valid_thumb(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes.len() <= MAX_THUMB_BYTES && bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

/// The saved pages, newest first, in `dir` (`<store>/later`).
pub struct LaterStore {
    dir: PathBuf,
    items: Mutex<Vec<LaterItem>>,
    seq: std::sync::atomic::AtomicU64,
}

impl LaterStore {
    /// Loads the list (dropping invalid entries) and deletes thumbnails
    /// that belong to no entry.
    pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let mut items: Vec<LaterItem> = load_or_reset::<Vec<LaterItem>>(&dir.join(INDEX_FILE))
            .into_iter()
            .filter_map(|mut it| {
                if !valid_id(&it.id) {
                    return None;
                }
                it.url = storable_url(&it.url)?;
                it.title = sanitize_page_text(&it.title, TITLE_CHARS);
                it.thumb = it.thumb && thumb_path(&dir, &it.id).is_file();
                Some(it)
            })
            .collect();
        items.sort_by_key(|it| std::cmp::Reverse(it.saved_ms));
        items.dedup_by(|a, b| a.id == b.id);
        items.truncate(MAX_LATER);
        if let Ok(entries) = fs::read_dir(&dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let keep = name == INDEX_FILE
                    || name.strip_suffix(".jpg").is_some_and(|id| items.iter().any(|it| it.thumb && it.id == id));
                if !keep {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        Ok(Self { dir, items: Mutex::new(items), seq: std::sync::atomic::AtomicU64::new(0) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<LaterItem>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list(&self) -> Vec<LaterItem> {
        self.lock().clone()
    }

    /// Saves a page (http/https only) with an optional thumbnail (dropped
    /// if it is not a valid one). `None` if the URL cannot be stored.
    pub fn add(&self, url: &str, title: &str, thumb: Option<&[u8]>, now_ms: u64) -> std::io::Result<Option<LaterItem>> {
        let Some(url) = storable_url(url) else { return Ok(None) };
        let n = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("l{now_ms}-{n}");
        let thumb = thumb.filter(|t| valid_thumb(t));
        if let Some(bytes) = thumb {
            atomic_write(&thumb_path(&self.dir, &id), bytes)?;
        }
        let item = LaterItem {
            id,
            url,
            title: sanitize_page_text(title, TITLE_CHARS),
            saved_ms: now_ms,
            thumb: thumb.is_some(),
        };
        let (snapshot, dropped) = {
            let mut items = self.lock();
            items.insert(0, item.clone());
            let keep = MAX_LATER.min(items.len());
            let dropped: Vec<LaterItem> = items.drain(keep..).collect();
            (serde_json::to_vec(&*items)?, dropped)
        };
        for d in dropped {
            let _ = fs::remove_file(thumb_path(&self.dir, &d.id));
        }
        atomic_write(&self.dir.join(INDEX_FILE), &snapshot)?;
        Ok(Some(item))
    }

    /// A saved page's thumbnail (JPEG bytes).
    pub fn thumb(&self, id: &str) -> Option<Vec<u8>> {
        if !valid_id(id) || !self.lock().iter().any(|it| it.id == id && it.thumb) {
            return None;
        }
        fs::read(thumb_path(&self.dir, id)).ok().filter(|b| valid_thumb(b))
    }

    /// A saved page's thumbnail as `data:image/jpeg;base64,…`, for the chrome.
    pub fn thumb_url(&self, id: &str) -> Option<String> {
        self.thumb(id).map(|b| crate::storage::data_url("image/jpeg", &b))
    }

    /// Removes a saved page (and its thumbnail) and returns it.
    pub fn take(&self, id: &str) -> std::io::Result<Option<LaterItem>> {
        if !valid_id(id) {
            return Ok(None);
        }
        let (item, snapshot) = {
            let mut items = self.lock();
            let Some(at) = items.iter().position(|it| it.id == id) else { return Ok(None) };
            let item = items.remove(at);
            (item, serde_json::to_vec(&*items)?)
        };
        let _ = fs::remove_file(thumb_path(&self.dir, id));
        atomic_write(&self.dir.join(INDEX_FILE), &snapshot)?;
        Ok(Some(item))
    }
}

fn thumb_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.jpg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("snorkle-later-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];

    #[test]
    fn saves_lists_and_takes_pages() {
        let dir = tmpdir("basic");
        let s = LaterStore::open(&dir).unwrap();
        let a = s.add("https://a.example/x", "A\u{202e}", Some(JPEG), 1).unwrap().unwrap();
        let b = s.add("https://b.example/", "B", Some(b"<svg/>"), 2).unwrap().unwrap();
        assert!(s.add("javascript:alert(1)", "x", None, 3).unwrap().is_none(), "web pages only");
        assert!(a.thumb && !b.thumb, "only a JPEG is kept as a thumbnail");
        assert_eq!(a.title, "A", "titles are cleaned");
        assert_eq!(s.list().iter().map(|i| i.id.clone()).collect::<Vec<_>>(), vec![b.id.clone(), a.id.clone()]);
        assert_eq!(s.thumb(&a.id).as_deref(), Some(JPEG));
        assert_eq!(s.thumb_url(&a.id).as_deref(), Some("data:image/jpeg;base64,/9j/4AECAw=="));
        assert!(s.thumb("../index").is_none());
        drop(s);
        let s = LaterStore::open(&dir).unwrap();
        assert_eq!(s.list().len(), 2, "kept across launches");
        assert_eq!(s.take(&a.id).unwrap().map(|i| i.url), Some("https://a.example/x".to_string()));
        assert!(!dir.join(format!("{}.jpg", a.id)).exists(), "thumbnail deleted with it");
        assert!(s.take(&a.id).unwrap().is_none());
        fs::write(dir.join("stray.jpg"), JPEG).unwrap();
        drop(s);
        let _ = LaterStore::open(&dir).unwrap();
        assert!(!dir.join("stray.jpg").exists(), "files without an entry are removed");
    }

    #[test]
    fn keeps_at_most_max_later() {
        let dir = tmpdir("cap");
        let s = LaterStore::open(&dir).unwrap();
        let first = s.add("https://first.example/", "", Some(JPEG), 0).unwrap().unwrap();
        for i in 1..=MAX_LATER as u64 {
            s.add("https://e.example/", "", None, i).unwrap();
        }
        assert_eq!(s.list().len(), MAX_LATER);
        assert!(s.list().iter().all(|i| i.id != first.id), "oldest dropped");
        assert!(!dir.join(format!("{}.jpg", first.id)).exists());
        assert!(!valid_thumb(&vec![0xFF; MAX_THUMB_BYTES + 1]));
    }
}
