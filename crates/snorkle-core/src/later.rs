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

// ---- capture (constant scripts, client world) -------------------------------

/// Starts a capture (channel C22): run with `callAsyncJavaScript` in the
/// page's client world, main frame, only when the user saves the page (or
/// adds pages to a saved one). Collects the URLs the page loaded (the
/// resource timeline, element sources, stylesheets and the `url()`s inside
/// readable ones), fetches the page's HTML again with its session
/// (`cache: force-cache`, so usually from the HTTP cache), keeps the URL
/// list in the client world for [`fetch_js`], and returns
/// `{"html": string|null, "title": string, "urls": [string]}`.
pub const CAPTURE_START_JS: &str = r#"
const MAX_URLS = 1500;
const here = (() => { const u = new URL(location.href); u.hash = ''; return u.href; })();
const seen = new Set();
const urls = [];
const add = (raw, base) => {
  if (!raw || urls.length >= MAX_URLS) return;
  try {
    const u = new URL(raw, base || document.baseURI);
    if (u.protocol !== 'https:' && u.protocol !== 'http:') return;
    u.hash = '';
    const s = u.href;
    if (s === here || seen.has(s)) return;
    seen.add(s);
    urls.push(s);
  } catch (e) {}
};
const cssUrls = (text, base) => {
  const re = /url\(\s*(['"]?)([^'")]+)\1\s*\)/g;
  let m;
  while ((m = re.exec(text)) && urls.length < MAX_URLS) if (!/^(data|blob|#)/i.test(m[2])) add(m[2], base);
};
try { for (const e of performance.getEntriesByType('resource')) add(e.name); } catch (e) {}
for (const el of document.querySelectorAll('script[src], link[href], img, source, video, audio, track, input[type=image], [style*="url("]')) {
  add(el.getAttribute('src'));
  if (el.tagName === 'LINK') add(el.getAttribute('href'));
  if (el.currentSrc) add(el.currentSrc);
  if (el.getAttribute('poster')) add(el.getAttribute('poster'));
  const set = el.getAttribute('srcset');
  if (set) for (const part of set.split(',')) add(part.trim().split(/\s+/)[0]);
  const style = el.getAttribute('style');
  if (style) cssUrls(style);
}
for (const sheet of document.styleSheets) {
  if (sheet.href) add(sheet.href);
  try { for (const rule of sheet.cssRules) cssUrls(rule.cssText, sheet.href || document.baseURI); } catch (e) {}
}
let html = null;
try {
  const r = await fetch(here, { credentials: 'include', cache: 'force-cache' });
  const type = r.headers.get('content-type') || 'text/html';
  if (r.ok && /html/i.test(type)) html = await r.text();
} catch (e) {}
window.__snorkleArchive = { urls, cache: new Map() };
return JSON.stringify({ html, title: document.title || '', urls });
"#;

/// Bytes a [`DRAIN_JS`] answer may carry (base64 included).
pub const MAX_DRAIN_BYTES: usize = 24 * 1024 * 1024;
/// Bytes a [`CAPTURE_START_JS`] answer may carry.
pub const MAX_START_BYTES: usize = 12 * 1024 * 1024;

/// Fetches the URLs at `indices` of the list [`CAPTURE_START_JS`] kept
/// (same-origin ones with the page's cookies, others without; at most
/// [`MAX_RESOURCE_BYTES`] each, 20 s each) into the client world's cache.
/// Only integers are put into the source. Returns the number fetched.
pub fn fetch_js(indices: &[usize]) -> String {
    let list = indices.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    format!(
        r#"
const S = window.__snorkleArchive;
if (!S) return 0;
const MAX = {max};
const b64 = (buf) => {{
  const u = new Uint8Array(buf);
  let s = '';
  for (let i = 0; i < u.length; i += 0x8000) s += String.fromCharCode.apply(null, u.subarray(i, i + 0x8000));
  return btoa(s);
}};
let n = 0;
await Promise.all([{list}].map(async (i) => {{
  const u = S.urls[i];
  if (!u) return;
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 20000);
  try {{
    const same = new URL(u).origin === location.origin;
    const r = await fetch(u, {{ credentials: same ? 'include' : 'omit', cache: 'force-cache', signal: ctl.signal }});
    if (!r.ok || Number(r.headers.get('content-length') || 0) > MAX) return;
    const buf = await r.arrayBuffer();
    if (!buf.byteLength || buf.byteLength > MAX) return;
    S.cache.set(i, {{ i, t: r.headers.get('content-type') || '', b: b64(buf) }});
    n++;
  }} catch (e) {{}} finally {{ clearTimeout(timer); }}
}}));
return n;
"#,
        max = MAX_RESOURCE_BYTES,
    )
}

/// Hands over fetched resources, at most about 8 MB of base64 per call
/// (always at least one), and forgets them: `{"items": [{"i": index, "t":
/// content type, "b": base64}], "more": bool}`.
pub const DRAIN_JS: &str = r#"
const S = window.__snorkleArchive;
if (!S) return JSON.stringify({ items: [], more: false });
const items = [];
let size = 0;
for (const [k, v] of S.cache) {
  if (items.length && size + v.b.length > 8 * 1024 * 1024) break;
  items.push(v);
  size += v.b.length;
  S.cache.delete(k);
}
return JSON.stringify({ items, more: S.cache.size > 0 });
"#;

/// Ends a capture: drops the client world's list and cache.
pub const END_JS: &str = "delete window.__snorkleArchive; return 0;";

/// What [`CAPTURE_START_JS`] reported, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartReport {
    /// The page's HTML as the server sends it, if it could be fetched.
    pub html: Option<String>,
    pub title: String,
    /// The URLs the page loaded, by the index the script keeps (entries
    /// that are not acceptable URLs are `None`, so indices stay aligned).
    pub urls: Vec<Option<Url>>,
}

/// Parses a [`CAPTURE_START_JS`] answer.
pub fn parse_start(text: &str) -> Option<StartReport> {
    #[derive(Deserialize)]
    struct Raw {
        html: Option<String>,
        #[serde(default)]
        title: String,
        #[serde(default)]
        urls: Vec<String>,
    }
    if text.len() > MAX_START_BYTES {
        return None;
    }
    let raw: Raw = serde_json::from_str(text).ok()?;
    Some(StartReport {
        html: raw.html.filter(|h| !h.is_empty() && h.len() <= MAX_PAGE_BYTES),
        title: sanitize_page_text(&raw.title, TITLE_CHARS),
        urls: raw.urls.iter().take(MAX_CAPTURE_URLS).map(|u| clean_url(u)).collect(),
    })
}

/// Parses a [`DRAIN_JS`] answer into resources (URLs from `urls`, the
/// checked list, never from the answer) and whether more are waiting.
pub fn parse_drain(text: &str, urls: &[Option<Url>]) -> Option<(Vec<Resource>, bool)> {
    #[derive(Deserialize)]
    struct Item {
        i: usize,
        #[serde(default)]
        t: String,
        b: String,
    }
    #[derive(Deserialize)]
    struct Raw {
        items: Vec<Item>,
        #[serde(default)]
        more: bool,
    }
    if text.len() > MAX_DRAIN_BYTES {
        return None;
    }
    let raw: Raw = serde_json::from_str(text).ok()?;
    let out = raw
        .items
        .into_iter()
        .filter_map(|it| {
            let url = urls.get(it.i)?.as_ref()?;
            let data = base64_decode(&it.b).filter(|d| d.len() <= MAX_RESOURCE_BYTES)?;
            clean_resource(Resource { url: url.to_string(), mime: clean_mime(&it.t), encoding: charset_of(&it.t), data })
        })
        .collect();
    Some((out, raw.more))
}

/// Standard base64 (padding optional); `None` on anything else.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes = s.trim_end_matches('=').as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 3);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
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
        let a = s.add("https://a.example/x", "A\u{202e}", Some(JPEG), None, 1).unwrap().unwrap();
        let b = s.add("https://b.example/", "B", Some(b"<svg/>"), None, 2).unwrap().unwrap();
        assert!(s.add("javascript:alert(1)", "x", None, None, 3).unwrap().is_none(), "web pages only");
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
        let first = s.add("https://first.example/", "", Some(JPEG), None, 0).unwrap().unwrap();
        for i in 1..=MAX_LATER as u64 {
            s.add("https://e.example/", "", None, None, i).unwrap();
        }
        assert_eq!(s.list().len(), MAX_LATER);
        assert!(s.list().iter().all(|i| i.id != first.id), "oldest dropped");
        assert!(!dir.join(format!("{}.jpg", first.id)).exists());
        assert!(!valid_thumb(&vec![0xFF; MAX_THUMB_BYTES + 1]));
    }

    fn capture(url: &str, html: &str, resources: Vec<Resource>) -> PageCapture {
        PageCapture { url: url.into(), title: "T".into(), html: html.as_bytes().to_vec(), scripts: true, resources }
    }

    fn res(url: &str, mime: &str, data: &[u8]) -> Resource {
        Resource { url: url.into(), mime: mime.into(), encoding: None, data: data.to_vec() }
    }

    #[test]
    fn archives_pages_and_resources() {
        let dir = tmpdir("archive");
        let s = LaterStore::open(&dir).unwrap();
        let page = "https://a.example/story#top";
        let c = capture(
            page,
            "<p>story</p>",
            vec![
                res("https://a.example/app.js", "text/javascript; charset=UTF-8", b"run()"),
                res("https://cdn.example/x.png#frag", "image/png", &[1, 2, 3]),
                res("https://a.example/app.js", "text/javascript", b"duplicate"),
                res("javascript:alert(1)", "text/html", b"x"),
                res("https://a.example/empty", "text/plain", b""),
                res("https://user:pw@a.example/secret", "text/plain", b"x"),
                res("https://a.example/evil", "text/html\r\nX: y", b"x"),
            ],
        );
        let item = s.add("https://a.example/story", "Story", None, Some(c), 5).unwrap().unwrap();
        assert_eq!(item.pages.len(), 1);
        assert_eq!(item.pages[0].url, "https://a.example/story", "pages are kept without fragments");
        let open = s.archive(&item.id, None).unwrap();
        assert!(open.scripts && open.saved_ms == 5 && open.page.as_str() == "https://a.example/story");
        let main = crate::webarchive::read_main(&open.data).unwrap();
        assert_eq!(main.data, b"<p>story</p>");
        let subs = crate::webarchive::read_subresources(&open.data).unwrap();
        let mut got: Vec<(String, String, Vec<u8>)> = subs.into_iter().map(|r| (r.url, r.mime, r.data)).collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("https://a.example/app.js".into(), "text/javascript".into(), b"run()".to_vec()),
                ("https://a.example/evil".into(), "application/octet-stream".into(), b"x".to_vec()),
                ("https://cdn.example/x.png".into(), "image/png".into(), vec![1, 2, 3]),
            ],
            "first copy kept; bad URLs, empty data dropped; odd MIME types neutralised"
        );

        // Adding a page shares the resources; a page is opened by URL.
        let n = s
            .add_pages(&item.id, vec![capture("https://a.example/part-2", "<p>two</p>", vec![res("https://a.example/two.css", "text/css", b"p{}")])])
            .unwrap();
        assert_eq!(n, 2);
        let two = s.archive(&item.id, Some("https://a.example/part-2#x")).unwrap();
        assert_eq!(crate::webarchive::read_main(&two.data).unwrap().data, b"<p>two</p>");
        assert_eq!(two.pages.len(), 2);
        assert_eq!(crate::webarchive::read_subresources(&two.data).unwrap().len(), 4);

        // Kept across launches; removed with the item.
        drop(s);
        let s = LaterStore::open(&dir).unwrap();
        assert_eq!(s.item(&item.id).unwrap().pages.len(), 2);
        assert!(dir.join(&item.id).is_dir());
        fs::create_dir_all(dir.join("l1-1")).unwrap();
        drop(s);
        let s = LaterStore::open(&dir).unwrap();
        assert!(!dir.join("l1-1").exists(), "archives without an item are removed");
        s.take(&item.id).unwrap();
        assert!(!dir.join(&item.id).exists());
        assert!(s.archive(&item.id, None).is_none());
        assert!(s.archive("../x", None).is_none());
    }

    #[test]
    fn archive_limits_hold() {
        let dir = tmpdir("archive-cap");
        let s = LaterStore::open(&dir).unwrap();
        let big = vec![0u8; MAX_RESOURCE_BYTES];
        let resources: Vec<Resource> = (0..8).map(|i| res(&format!("https://a.example/{i}.bin"), "application/octet-stream", &big)).collect();
        let too_big = res("https://a.example/huge", "a/b", &vec![0u8; MAX_RESOURCE_BYTES + 1]);
        let mut all = resources;
        all.push(too_big);
        let item = s.add("https://a.example/", "", None, Some(capture("https://a.example/", "<p>", all)), 1).unwrap().unwrap();
        assert!(item.archive_bytes <= MAX_ARCHIVE_BYTES);
        let open = s.archive(&item.id, None).unwrap();
        let subs = crate::webarchive::read_subresources(&open.data).unwrap();
        assert_eq!(subs.len() as u64, MAX_ARCHIVE_BYTES / MAX_RESOURCE_BYTES as u64, "what does not fit is left out");
        let pages: Vec<PageCapture> = (0..MAX_PAGES + 5).map(|i| capture(&format!("https://a.example/p{i}"), "<p>", vec![])).collect();
        assert_eq!(s.add_pages(&item.id, pages).unwrap(), MAX_PAGES);
    }

    #[test]
    fn capture_reports_are_checked() {
        let start = r#"{"html":"<p>hi</p>","title":"T\u202e","urls":["https://a.example/x.js","javascript:1","https://a.example/y.css#z"]}"#;
        let r = parse_start(start).unwrap();
        assert_eq!(r.html.as_deref(), Some("<p>hi</p>"));
        assert_eq!(r.title, "T");
        assert_eq!(r.urls.len(), 3, "indices stay aligned");
        assert!(r.urls[1].is_none());
        assert_eq!(r.urls[2].as_ref().unwrap().as_str(), "https://a.example/y.css");
        assert!(parse_start("nope").is_none());
        let drain = r#"{"items":[{"i":0,"t":"text/javascript; charset=utf-8","b":"cnVuKCk="},{"i":1,"t":"x/y","b":"AA=="},{"i":9,"t":"","b":"AA=="},{"i":2,"t":"text/css","b":"!!"}],"more":true}"#;
        let (items, more) = parse_drain(drain, &r.urls).unwrap();
        assert!(more);
        assert_eq!(items.len(), 1, "unknown or invalid indices and bad base64 are dropped");
        assert_eq!((items[0].url.as_str(), items[0].mime.as_str(), items[0].encoding.as_deref()), ("https://a.example/x.js", "text/javascript", Some("utf-8")));
        assert_eq!(items[0].data, b"run()");
        assert_eq!(base64_decode("aGVsbG8"), Some(b"hello".to_vec()));
        assert_eq!(base64_decode("aGVsbG8gd29ybGQ="), Some(b"hello world".to_vec()));
        assert!(fetch_js(&[1, 2, 3]).contains("[1,2,3].map"));
        assert_eq!(clean_mime("TEXT/HTML; charset=x"), "text/html");
        assert_eq!(clean_mime("text/html\"><script>"), "application/octet-stream");
        assert_eq!(charset_of("text/html; charset=\"ISO-8859-1\""), Some("ISO-8859-1".into()));
        assert_eq!(charset_of("text/html; charset=a b"), None);
    }
}
