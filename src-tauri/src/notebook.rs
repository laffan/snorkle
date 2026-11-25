//! The Notebook's native half (owner request 2026-11-15; see
//! `snorkle_core::notebook`): its file, clips the app draws from its own
//! web view, and the constant page code for clip mode and the quote
//! button (channel C23). Every command that reads or changes a page
//! refuses private tabs: nothing from them reaches the notebook, which is
//! stored.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use snorkle_core::console::Level;
use snorkle_core::notebook;
use snorkle_core::tabs::{Life, TabId};

use crate::state::{with_host, Shared};

type S<'a> = tauri::State<'a, Arc<Shared>>;

/// How long a page is shown without its view modes before a clip is
/// drawn (hidden images and colours show at once; a frame or two to
/// paint them).
const PLAIN_MS: u64 = 320;
/// The longest on-device text recognition may take; past it the clip
/// keeps only the page's text (its answer is dropped).
const RECOGNITION_LIMIT: Duration = Duration::from_secs(20);
/// The page text read for a clip is waited for at most this long.
const REGION_TEXT_LIMIT: Duration = Duration::from_millis(1500);

/// Clips whose pictures are still to be read by text recognition
/// ([`notebook_clip_words`]): a number the chrome holds, the picture and
/// the page's text. The latest few only.
static WORDS_PENDING: Mutex<Vec<(u64, Vec<u8>, String)>> = Mutex::new(Vec::new());
static WORDS_NEXT: AtomicU64 = AtomicU64::new(1);
const WORDS_KEPT: usize = 4;

/// A live, non-private tab with a page, or why not.
fn capturable(s: &Shared, tab: TabId) -> Result<(), String> {
    let b = s.browser();
    let t = b.tab(tab).ok_or("That tab is gone.")?;
    if t.is_private() {
        return Err("Private tabs don't send anything to the notebook.".into());
    }
    if t.life != Life::Live || t.notice.is_some() {
        return Err("Open the page first.".into());
    }
    Ok(())
}

fn wait<T>(rx: mpsc::Receiver<T>, limit: Duration) -> Option<T> {
    rx.recv_timeout(limit).ok()
}

/// The notebook as stored (checked again as it is read), or `None`.
#[tauri::command]
pub async fn load_notebook(s: S<'_>, space: Option<String>) -> Result<Option<String>, String> {
    if !known_space(&s, space.as_deref()) {
        return Ok(None);
    }
    let shared = s.inner().clone();
    // A research session's own notebook (2026-11-24), or everyday browsing's.
    tauri::async_runtime::spawn_blocking(move || shared.store.as_ref().and_then(|st| st.notebook_in(space.as_deref())))
        .await
        .map_err(|_| "Couldn't read the notebook.".to_string())
}

/// Everyday browsing (`None`) or a research session the core has.
fn known_space(s: &S<'_>, space: Option<&str>) -> bool {
    space.is_none_or(|id| s.browser().research().iter().any(|r| r.id == id))
}

/// Stores the notebook (checked and rebuilt: `notebook::clean`).
#[tauri::command]
pub async fn save_notebook(s: S<'_>, notebook: String, space: Option<String>) -> Result<(), String> {
    // A deleted session's last save (it can arrive after the delete) is
    // dropped rather than writing its notebook again.
    if !known_space(&s, space.as_deref()) {
        return Ok(());
    }
    if notebook.len() > notebook::MAX_NOTEBOOK_BYTES {
        return Err("The notebook is too large to save (64 MB). Delete some clips.".into());
    }
    let shared = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let store = shared.store.as_ref().ok_or("Saving is not available.")?;
        match store.set_notebook_in(space.as_deref(), &notebook) {
            Ok(true) => Ok(()),
            Ok(false) => Err("The notebook could not be saved.".to_string()),
            Err(e) => Err(format!("The notebook could not be saved: {e}")),
        }
    })
    .await
    .map_err(|_| "The notebook could not be saved.".to_string())?
}

/// A clip: the picture, and the page's text in it (for the notebook's
/// search; stored with the clip, not shown). `words`: the number to ask
/// [`notebook_clip_words`] for the words text recognition reads in it,
/// which come later (2026-11-16: the clip is not held back for them).
/// `links`: the links laid out in it, shown under it; `at`: its box in the
/// page's document, to go back to ([`notebook_clip_back`]; 2026-11-25).
#[derive(serde::Serialize)]
pub struct Clip {
    image: String,
    text: String,
    words: u64,
    links: Vec<notebook::ClipLink>,
    at: Option<[f64; 4]>,
}

/// What a region of `tab`'s page holds (C23 read-back: its text, its
/// links, its box in the document), or nothing.
fn region_text(tab: TabId, rect: (f64, f64, f64, f64)) -> notebook::Region {
    let js = notebook::region_text_js(rect.0, rect.1, rect.2, rect.3);
    let (tx, rx) = mpsc::channel();
    snorkle_content::dispatch_main(move || {
        let tx2 = tx.clone();
        let mut ran = false;
        with_host(|h| {
            ran = true;
            h.notebook_read(tab, &js, Box::new(move |t| drop(tx.send(t))));
        });
        if !ran {
            let _ = tx2.send(None);
        }
    });
    wait(rx, REGION_TEXT_LIMIT).flatten().and_then(|t| notebook::parse_region(&t)).unwrap_or_default()
}

/// The page's text in the region, then the lines on-device recognition
/// reads in the picture that the page's text does not already have (text
/// drawn in images, canvases, video), cleaned and capped.
fn clip_text(shared: &Shared, tab: Option<TabId>, page: String, jpeg: &[u8]) -> String {
    let seen = page.to_lowercase();
    let read = recognized(shared, tab, jpeg);
    let extra: Vec<&str> =
        read.lines().map(str::trim).filter(|l| !l.is_empty() && !seen.contains(&l.to_lowercase())).collect();
    let all = if extra.is_empty() {
        page
    } else if page.is_empty() {
        extra.join("\n")
    } else {
        format!("{page}\n{}", extra.join("\n"))
    };
    snorkle_core::ai::clean_multiline(&all, notebook::MAX_REGION_CHARS)
}

/// The text on-device recognition reads in a clip, on a thread of its
/// own: "" when it fails, panics or takes longer than
/// [`RECOGNITION_LIMIT`] (it is then left to finish, and its answer
/// dropped).
fn recognized(shared: &Shared, tab: Option<TabId>, jpeg: &[u8]) -> String {
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    let jpeg = jpeg.to_vec();
    let spawned = std::thread::Builder::new().name("snorkle-clip-text".into()).spawn(move || {
        let _ = tx.send(snorkle_content::ocr::recognize_text(&jpeg, notebook::MAX_REGION_CHARS));
    });
    if spawned.is_err() {
        return String::new();
    }
    match rx.recv_timeout(RECOGNITION_LIMIT) {
        Ok(read) => {
            let read = read.unwrap_or_default();
            let msg = format!(
                "notebook clip: recognition read {} chars in {} ms",
                read.chars().count(),
                started.elapsed().as_millis()
            );
            shared.log(Level::Debug, tab, &msg);
            read
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            shared.log(Level::Warn, tab, "notebook clip: recognition took too long; kept without it");
            String::new()
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            shared.log(Level::Warn, tab, "notebook clip: recognition failed; kept without it");
            String::new()
        }
    }
}

/// A clip of a region of `tab`'s page (a C23 report's rectangle: visual
/// viewport CSS pixels, `vw` wide): a `data:image/jpeg` URL and the text it
/// shows. The page's text in the region is read first; then the page is
/// shown without its view modes for a moment (black and white, hidden
/// media, dark), so the clip has its colours and images (its appearance is
/// left as it is). The words in the picture are read afterwards
/// ([`notebook_clip_words`]).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn notebook_clip(s: S<'_>, tab: u64, x: f64, y: f64, w: f64, h: f64, vw: f64) -> Result<Clip, String> {
    let tab = TabId(tab);
    capturable(&s, tab)?;
    let shared = s.inner().clone();
    // Each step is logged (the console's recording) so a clip that does
    // not arrive can be traced.
    shared.log(Level::Debug, Some(tab), &format!("notebook clip: {w:.0}x{h:.0} at {x:.0},{y:.0} (view {vw:.0})"));
    tauri::async_runtime::spawn_blocking(move || {
        let notebook::Region { text: page, links, at } = region_text(tab, (x, y, w, h));
        let msg = format!("notebook clip: page text {} chars, {} links", page.chars().count(), links.len());
        shared.log(Level::Debug, Some(tab), &msg);
        snorkle_content::dispatch_main(move || with_host(|h| h.plain_page(tab, true)));
        std::thread::sleep(Duration::from_millis(PLAIN_MS));
        let (tx, rx) = mpsc::channel();
        snorkle_content::dispatch_main(move || {
            let tx2 = tx.clone();
            let mut ran = false;
            with_host(|host| {
                ran = true;
                host.clip_region(tab, (x, y, w, h), vw, Box::new(move |jpeg| drop(tx.send(jpeg))));
            });
            if !ran {
                let _ = tx2.send(None);
            }
        });
        let jpeg = wait(rx, Duration::from_secs(5)).flatten();
        snorkle_content::dispatch_main(move || with_host(|h| h.plain_page(tab, false)));
        let Some(jpeg) =
            jpeg.filter(|j| j.starts_with(&[0xFF, 0xD8, 0xFF]) && j.len() <= snorkle_content::CLIP_MAX_BYTES)
        else {
            shared.log(Level::Warn, Some(tab), "notebook clip: the view drew no picture");
            return Err("This part of the page can't be clipped.".to_string());
        };
        shared.log(Level::Debug, Some(tab), &format!("notebook clip: picture {} bytes", jpeg.len()));
        let image = snorkle_core::storage::data_url("image/jpeg", &jpeg);
        let text = snorkle_core::ai::clean_multiline(&page, notebook::MAX_REGION_CHARS);
        let words = WORDS_NEXT.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut q) = WORDS_PENDING.lock() {
            q.push((words, jpeg, page));
            let extra = q.len().saturating_sub(WORDS_KEPT);
            q.drain(..extra);
        }
        Ok(Clip { image, text, words, links, at })
    })
    .await
    .map_err(|_| "This part of the page can't be clipped.".to_string())?
}

/// The clip's text with the words on-device recognition reads in its
/// picture added (lines the page's text does not already have: text drawn
/// in images, canvases, video), cleaned and capped; `None` when it adds
/// nothing or the clip is no longer kept. Only pictures the app drew
/// itself are read (kept by [`notebook_clip`] under `words`).
#[tauri::command]
pub async fn notebook_clip_words(s: S<'_>, words: u64) -> Result<Option<String>, String> {
    let entry = WORDS_PENDING.lock().ok().and_then(|mut q| q.iter().position(|e| e.0 == words).map(|i| q.remove(i)));
    let Some((_, jpeg, page)) = entry else { return Ok(None) };
    let shared = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let page = snorkle_core::ai::clean_multiline(&page, notebook::MAX_REGION_CHARS);
        let text = clip_text(&shared, None, page.clone(), &jpeg);
        (text != page).then_some(text)
    })
    .await
    .map_err(|_| "Couldn't read the clip.".to_string())
}

/// ⌥-click on a link (C23 `link` report): the linked page's title, from
/// the start of that page fetched without cookies (`http::fetch_page_head`,
/// `notebook::page_title`); `None` when it has none or cannot be loaded.
/// From a non-private tab only; the address is checked again here.
#[tauri::command]
pub async fn notebook_link_title(s: S<'_>, tab: u64, url: String) -> Result<Option<String>, String> {
    let tab = TabId(tab);
    capturable(&s, tab)?;
    let url = notebook::web_url(&url).and_then(|u| snorkle_core::Url::parse(&u).ok()).ok_or("Not a web address.")?;
    let (tx, rx) = mpsc::channel();
    snorkle_content::http::fetch_page_head(&url, Box::new(move |head| drop(tx.send(head))));
    let head = tauri::async_runtime::spawn_blocking(move || wait(rx, Duration::from_secs(12)))
        .await
        .map_err(|_| "Couldn't load the page.".to_string())?;
    Ok(head.flatten().and_then(|h| notebook::page_title(&h)))
}

/// Going back to a clip (2026-11-25): `tab`'s page, the clip's page, is
/// scrolled to the clip's box in its document (`notebook::back_js`, four
/// numbers) and the box is outlined there for a moment. From a live,
/// non-private tab only.
#[tauri::command]
pub fn notebook_clip_back(s: S<'_>, tab: u64, x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
    let tab = TabId(tab);
    capturable(&s, tab)?;
    let js = notebook::back_js(x, y, w, h);
    snorkle_content::dispatch_main(move || with_host(|h| h.notebook_run(tab, &js)));
    Ok(())
}

/// The notebook's scissors: clip mode on or off in `tab`'s page (a drag
/// there then clips, without ⌥).
#[tauri::command]
pub fn notebook_clip_mode(s: S<'_>, tab: u64, on: bool) -> Result<(), String> {
    let tab = TabId(tab);
    if on {
        capturable(&s, tab)?;
    }
    let js = notebook::clip_mode_js(on);
    snorkle_content::dispatch_main(move || with_host(|h| h.notebook_run(tab, &js)));
    Ok(())
}

/// The quote button dragged over `tab`'s page: show where it would land
/// (`fx`, `fy`: fractions of the page's view; `from_y`: where the line
/// leaves the page's right edge). `fx < 0` takes the marker away.
#[tauri::command]
pub fn notebook_link_hover(s: S<'_>, tab: u64, fx: f64, fy: f64, from_y: f64) {
    let tab = TabId(tab);
    if s.browser().tab(tab).is_none_or(|t| t.is_private()) {
        return;
    }
    let js = if fx < 0.0 { notebook::LEAVE_JS.to_string() } else { notebook::hover_js(fx, fy, from_y) };
    snorkle_content::dispatch_main(move || with_host(|h| h.notebook_run(tab, &js)));
}

/// The quote button let go over `tab`'s page at (`fx`, `fy`): the
/// sentence there (cleaned), or `None`.
#[tauri::command]
pub async fn notebook_link_drop(s: S<'_>, tab: u64, fx: f64, fy: f64) -> Result<Option<String>, String> {
    let tab = TabId(tab);
    capturable(&s, tab)?;
    let js = notebook::drop_js(fx, fy);
    let (tx, rx) = mpsc::channel();
    snorkle_content::dispatch_main(move || {
        with_host(|h| h.notebook_read(tab, &js, Box::new(move |t| drop(tx.send(t)))));
    });
    let text = tauri::async_runtime::spawn_blocking(move || wait(rx, Duration::from_secs(4)))
        .await
        .map_err(|_| "Couldn't read the page.".to_string())?;
    Ok(text.flatten().and_then(|t| notebook::parse_drop(&t)))
}
