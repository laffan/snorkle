//! SECURITY-REVIEW: Save for Later (owner request 2026-11-09): a tab is
//! put aside as its address, title, a thumbnail of what it showed and,
//! since the same day's overhaul, an **archive** of the page that works
//! offline (the method of laffan/thedailyprophet, as a WebKit web archive:
//! `crate::webarchive`). The list lives in `later/index.json`, each
//! thumbnail in `later/<id>.jpg`, each archive in `later/<id>/`.
//!
//! The archive keeps the page's own HTML as the server sent it (fetched
//! again by the capture script, so scripts hydrate against what they
//! expect) and every resource the page loaded under its original URL:
//! what WebKit holds for the page (`createWebArchiveData`) plus what the
//! capture script fetches with the page's session (the resource timeline:
//! data requests, script chunks, fonts). Pages added later ("+", links
//! chosen in the saved page) are stored the same way and share the
//! resources. Reading builds one web archive per page ([`LaterStore::archive`]).
//!
//! Only non-private pages are saved (the caller checks). Everything that
//! comes from the page is checked here before it is written: http(s) URLs
//! without fragments, MIME types and encodings as plain tokens, sizes
//! ([`MAX_RESOURCE_BYTES`], [`MAX_ARCHIVE_BYTES`], [`MAX_RESOURCES`],
//! [`MAX_PAGES`]); files are named by the app (a hash of the URL), never
//! by the page. Thumbnails are JPEG bytes the app drew itself from a
//! snapshot of its own web view; they are checked again (magic bytes,
//! size) and served to the chrome only as a `data:image/jpeg;base64,…`
//! URL.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::storage::{atomic_write, load_or_reset, storable_url};
use crate::text::sanitize_page_text;
use crate::webarchive::Resource;

/// Pages kept; the oldest (and their thumbnails and archives) are dropped
/// first.
pub const MAX_LATER: usize = 200;
/// Largest thumbnail accepted.
pub const MAX_THUMB_BYTES: usize = 400 * 1024;
/// Largest single resource kept in an archive.
pub const MAX_RESOURCE_BYTES: usize = 15 * 1024 * 1024;
/// Largest page HTML kept.
pub const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
/// Most bytes one saved item's archive may hold (pages and resources).
pub const MAX_ARCHIVE_BYTES: u64 = 80 * 1024 * 1024;
/// Most bytes all archives together may hold; the oldest items lose their
/// archive first (they keep their address, title and thumbnail).
pub const MAX_ARCHIVES_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
/// Most resources one archive holds.
pub const MAX_RESOURCES: usize = 1500;
/// Most pages one archive holds (the saved page and pages added to it).
pub const MAX_PAGES: usize = 30;
/// Most URLs the capture script reports for one page.
pub const MAX_CAPTURE_URLS: usize = 1500;
const INDEX_FILE: &str = "index.json";
const RESOURCES_FILE: &str = "resources.json";
const TITLE_CHARS: usize = 300;
const MAX_URL_CHARS: usize = 4096;

/// One saved page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaterItem {
    pub id: String,
    pub url: String,
    pub title: String,
    pub saved_ms: u64,
    /// Whether `<id>.jpg` exists.
    pub thumb: bool,
    /// The archive's pages, the saved page first; empty without an archive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<LaterPage>,
    /// Bytes the archive takes on disk.
    #[serde(default)]
    pub archive_bytes: u64,
}

/// A page in a saved item's archive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaterPage {
    pub url: String,
    pub title: String,
    /// False when the page is WebKit's copy of the live document (the
    /// server's HTML could not be fetched again): it already ran its
    /// scripts, so it is shown with JavaScript off.
    pub scripts: bool,
}

/// A resource as stored: original URL, MIME type, encoding and file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    u: String,
    m: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    e: Option<String>,
    f: String,
    n: u64,
}

/// One page captured from a tab (or a page added to a saved item).
#[derive(Clone, Debug, Default)]
pub struct PageCapture {
    /// The page's URL (the link that leads to it, for added pages).
    pub url: String,
    pub title: String,
    pub html: Vec<u8>,
    /// See [`LaterPage::scripts`].
    pub scripts: bool,
    pub resources: Vec<Resource>,
}

/// A saved page's archive, ready to load.
#[derive(Clone, Debug)]
pub struct OpenArchive {
    /// The page shown (its base URL).
    pub page: Url,
    /// Every page of the archive.
    pub pages: Vec<Url>,
    /// The web archive's bytes.
    pub data: Vec<u8>,
    pub scripts: bool,
    pub saved_ms: u64,
}

/// An id: `l` and digits/dashes, short (it is also a file name).
pub fn valid_id(id: &str) -> bool {
    id.len() > 1 && id.len() <= 40 && id.starts_with('l') && id[1..].chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// A JPEG the size of a thumbnail: starts with the SOI marker and fits.
pub fn valid_thumb(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes.len() <= MAX_THUMB_BYTES && bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

/// An archive URL: http(s), at most [`MAX_URL_CHARS`], without fragment or
/// credentials; normalised.
pub fn clean_url(raw: &str) -> Option<Url> {
    if raw.len() > MAX_URL_CHARS {
        return None;
    }
    let mut u = Url::parse(raw).ok()?;
    if !matches!(u.scheme(), "http" | "https") || !u.username().is_empty() || u.password().is_some() {
        return None;
    }
    u.set_fragment(None);
    Some(u)
}

/// A MIME type as a plain token, lowercase, parameters dropped
/// (`text/css`); `application/octet-stream` when it is not one.
pub fn clean_mime(raw: &str) -> String {
    let m = raw.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let ok = m.len() <= 100
        && m.split_once('/').is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
        && m.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '+' | '-' | '_'));
    if ok { m } else { "application/octet-stream".to_string() }
}

/// The `charset` parameter of a `Content-Type`, as a plain token.
pub fn charset_of(content_type: &str) -> Option<String> {
    let cs = content_type.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("charset").then(|| v.trim().trim_matches('"').to_string())
    })?;
    (!cs.is_empty() && cs.len() <= 40 && cs.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')))
        .then_some(cs)
}

/// A resource fit to be stored ([`clean_url`], [`clean_mime`], size), or
/// `None`.
pub fn clean_resource(r: Resource) -> Option<Resource> {
    let url = clean_url(&r.url)?;
    if r.data.is_empty() || r.data.len() > MAX_RESOURCE_BYTES {
        return None;
    }
    let encoding = r.encoding.as_deref().and_then(|e| charset_of(&format!("x; charset={e}")));
    Some(Resource { url: url.to_string(), mime: clean_mime(&r.mime), encoding, data: r.data })
}

/// The saved pages, newest first, in `dir` (`<store>/later`).
pub struct LaterStore {
    dir: PathBuf,
    items: Mutex<Vec<LaterItem>>,
    seq: std::sync::atomic::AtomicU64,
}

impl LaterStore {
    /// Loads the list (dropping invalid entries) and deletes thumbnails and
    /// archives that belong to no entry.
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
                it.pages.retain(|p| clean_url(&p.url).is_some_and(|u| u.as_str() == p.url));
                it.pages.truncate(MAX_PAGES);
                for p in &mut it.pages {
                    p.title = sanitize_page_text(&p.title, TITLE_CHARS);
                }
                if it.pages.is_empty() || !archive_dir(&dir, &it.id).is_dir() {
                    it.pages.clear();
                    it.archive_bytes = 0;
                }
                Some(it)
            })
            .collect();
        items.sort_by_key(|it| std::cmp::Reverse(it.saved_ms));
        items.dedup_by(|a, b| a.id == b.id);
        items.truncate(MAX_LATER);
        if let Ok(entries) = fs::read_dir(&dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let thumb = name.strip_suffix(".jpg").is_some_and(|id| items.iter().any(|it| it.thumb && it.id == id));
                let archive = items.iter().any(|it| !it.pages.is_empty() && it.id == name);
                if name == INDEX_FILE || thumb {
                    continue;
                }
                if archive && e.path().is_dir() {
                    continue;
                }
                if e.path().is_dir() {
                    let _ = fs::remove_dir_all(e.path());
                } else {
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

    pub fn item(&self, id: &str) -> Option<LaterItem> {
        self.lock().iter().find(|it| it.id == id).cloned()
    }

    /// Saves a page (http/https only) with an optional thumbnail (dropped
    /// if it is not a valid one) and an optional archive of it. `None` if
    /// the URL cannot be stored.
    pub fn add(
        &self,
        url: &str,
        title: &str,
        thumb: Option<&[u8]>,
        capture: Option<PageCapture>,
        now_ms: u64,
    ) -> std::io::Result<Option<LaterItem>> {
        let Some(url) = storable_url(url) else { return Ok(None) };
        let n = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("l{now_ms}-{n}");
        let thumb = thumb.filter(|t| valid_thumb(t));
        if let Some(bytes) = thumb {
            atomic_write(&thumb_path(&self.dir, &id), bytes)?;
        }
        let mut item = LaterItem {
            id,
            url,
            title: sanitize_page_text(title, TITLE_CHARS),
            saved_ms: now_ms,
            thumb: thumb.is_some(),
            pages: Vec::new(),
            archive_bytes: 0,
        };
        if let Some(c) = capture {
            let (pages, bytes) = write_archive(&archive_dir(&self.dir, &item.id), Vec::new(), vec![c])?;
            item.pages = pages;
            item.archive_bytes = bytes;
        }
        let (snapshot, dropped) = {
            let mut items = self.lock();
            items.insert(0, item.clone());
            let keep = MAX_LATER.min(items.len());
            let mut dropped: Vec<String> = items.drain(keep..).map(|d| d.id).collect();
            dropped.extend(enforce_total(&mut items));
            (serde_json::to_vec(&*items)?, dropped)
        };
        for d in dropped {
            self.remove_files(&d, false);
        }
        atomic_write(&self.dir.join(INDEX_FILE), &snapshot)?;
        Ok(Some(item))
    }

    /// Adds pages (chosen links, captured) to a saved item's archive; pages
    /// it already holds are replaced. Returns how many pages it now holds.
    pub fn add_pages(&self, id: &str, captures: Vec<PageCapture>) -> std::io::Result<usize> {
        if !valid_id(id) {
            return Ok(0);
        }
        let Some(current) = self.item(id) else { return Ok(0) };
        let (pages, bytes) = write_archive(&archive_dir(&self.dir, id), current.pages, captures)?;
        let (snapshot, dropped, count) = {
            let mut items = self.lock();
            let Some(it) = items.iter_mut().find(|it| it.id == id) else { return Ok(0) };
            it.pages = pages;
            it.archive_bytes = bytes;
            let count = it.pages.len();
            let dropped = enforce_total(&mut items);
            (serde_json::to_vec(&*items)?, dropped, count)
        };
        for d in dropped {
            self.remove_files(&d, false);
        }
        atomic_write(&self.dir.join(INDEX_FILE), &snapshot)?;
        Ok(count)
    }

    /// The web archive for `page` of a saved item (its first page when
    /// `page` is `None` or not one of its pages).
    pub fn archive(&self, id: &str, page: Option<&str>) -> Option<OpenArchive> {
        if !valid_id(id) {
            return None;
        }
        let item = self.item(id)?;
        let wanted = page.and_then(clean_url).map(|u| u.to_string());
        let p = item.pages.iter().find(|p| Some(&p.url) == wanted.as_ref()).or(item.pages.first())?;
        let dir = archive_dir(&self.dir, id);
        let html = fs::read(dir.join(page_file(&p.url))).ok().filter(|h| h.len() <= MAX_PAGE_BYTES)?;
        let main = Resource { url: p.url.clone(), mime: "text/html".into(), encoding: Some("UTF-8".into()), data: html };
        let mut subs = Vec::new();
        for r in load_or_reset::<Vec<Stored>>(&dir.join(RESOURCES_FILE)).into_iter().take(MAX_RESOURCES) {
            if r.u == p.url || !valid_file(&r.f) || clean_url(&r.u).is_none_or(|u| u.as_str() != r.u) {
                continue;
            }
            let Ok(data) = fs::read(dir.join(&r.f)) else { continue };
            if data.len() > MAX_RESOURCE_BYTES {
                continue;
            }
            subs.push(Resource { url: r.u, mime: clean_mime(&r.m), encoding: r.e, data });
        }
        Some(OpenArchive {
            page: Url::parse(&p.url).ok()?,
            pages: item.pages.iter().filter_map(|p| Url::parse(&p.url).ok()).collect(),
            data: crate::webarchive::write(&main, &subs),
            scripts: p.scripts,
            saved_ms: item.saved_ms,
        })
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

    /// Removes a saved page (and its thumbnail and archive) and returns it.
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
        self.remove_files(id, true);
        atomic_write(&self.dir.join(INDEX_FILE), &snapshot)?;
        Ok(Some(item))
    }

    /// Deletes an item's archive and, with `thumb`, its thumbnail.
    fn remove_files(&self, id: &str, thumb: bool) {
        if !valid_id(id) {
            return;
        }
        if thumb || !self.lock().iter().any(|it| it.id == id) {
            let _ = fs::remove_file(thumb_path(&self.dir, id));
        }
        let _ = fs::remove_dir_all(archive_dir(&self.dir, id));
    }
}

/// Drops the archives of the oldest items while all archives together are
/// over [`MAX_ARCHIVES_TOTAL_BYTES`] (the items stay); returns their ids.
fn enforce_total(items: &mut [LaterItem]) -> Vec<String> {
    let mut total: u64 = items.iter().map(|i| i.archive_bytes).sum();
    let mut dropped = Vec::new();
    for it in items.iter_mut().rev() {
        if total <= MAX_ARCHIVES_TOTAL_BYTES {
            break;
        }
        if it.pages.is_empty() {
            continue;
        }
        total -= it.archive_bytes;
        it.pages.clear();
        it.archive_bytes = 0;
        dropped.push(it.id.clone());
    }
    dropped
}

/// Writes pages and their resources into an archive folder that already
/// holds `pages`; returns the pages it now holds and its size. Resources
/// keep the first copy of each URL; what does not fit
/// ([`MAX_ARCHIVE_BYTES`], [`MAX_RESOURCES`], [`MAX_PAGES`]) is left out.
fn write_archive(dir: &Path, mut pages: Vec<LaterPage>, captures: Vec<PageCapture>) -> std::io::Result<(Vec<LaterPage>, u64)> {
    fs::create_dir_all(dir)?;
    let mut stored: Vec<Stored> = load_or_reset(&dir.join(RESOURCES_FILE));
    stored.retain(|r| valid_file(&r.f) && dir.join(&r.f).is_file());
    let mut have: BTreeSet<String> = stored.iter().map(|r| r.u.clone()).collect();
    let page_bytes = |pages: &[LaterPage]| -> u64 {
        pages.iter().filter_map(|p| fs::metadata(dir.join(page_file(&p.url))).ok()).map(|m| m.len()).sum()
    };
    let mut total: u64 = stored.iter().map(|r| r.n).sum::<u64>() + page_bytes(&pages);
    for c in captures {
        let Some(url) = clean_url(&c.url) else { continue };
        let url = url.to_string();
        if c.html.is_empty() || c.html.len() > MAX_PAGE_BYTES {
            continue;
        }
        let existing = pages.iter().position(|p| p.url == url);
        if existing.is_none() && pages.len() >= MAX_PAGES {
            continue;
        }
        let old = existing.and_then(|i| fs::metadata(dir.join(page_file(&pages[i].url))).ok()).map_or(0, |m| m.len());
        if total - old + c.html.len() as u64 > MAX_ARCHIVE_BYTES {
            continue;
        }
        atomic_write(&dir.join(page_file(&url)), &c.html)?;
        total = total - old + c.html.len() as u64;
        let page = LaterPage { url: url.clone(), title: sanitize_page_text(&c.title, TITLE_CHARS), scripts: c.scripts };
        match existing {
            Some(i) => pages[i] = page,
            None => pages.push(page),
        }
        for r in c.resources.into_iter().filter_map(clean_resource) {
            if have.contains(&r.url) || r.url == url || stored.len() >= MAX_RESOURCES {
                continue;
            }
            if total + r.data.len() as u64 > MAX_ARCHIVE_BYTES {
                continue;
            }
            let f = resource_file(&r.url);
            atomic_write(&dir.join(&f), &r.data)?;
            total += r.data.len() as u64;
            have.insert(r.url.clone());
            stored.push(Stored { u: r.url, m: r.mime, e: r.encoding, f, n: r.data.len() as u64 });
        }
    }
    atomic_write(&dir.join(RESOURCES_FILE), &serde_json::to_vec(&stored)?)?;
    Ok((pages, total))
}

fn hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn resource_file(url: &str) -> String {
    format!("{:016x}.r", hash(url))
}

fn page_file(url: &str) -> String {
    format!("{:016x}.html", hash(url))
}

/// A resource file name this module made: 16 hex digits and `.r`.
fn valid_file(name: &str) -> bool {
    name.len() == 18 && name.ends_with(".r") && name[..16].chars().all(|c| c.is_ascii_hexdigit())
}

fn thumb_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.jpg"))
}

fn archive_dir(dir: &Path, id: &str) -> PathBuf {
    dir.join(id)
}

#[cfg(test)]
mod tests;

mod capture;
pub use capture::*;
