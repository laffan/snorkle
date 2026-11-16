//! The Notebook's native half (owner request 2026-11-15; see
//! `snorkle_core::notebook`): its file, clips the app draws from its own
//! web view, and the constant page code for clip mode and the quote
//! button (channel C23). Every command that reads or changes a page
//! refuses private tabs: nothing from them reaches the notebook, which is
//! stored.

use std::sync::{mpsc, Arc};
use std::time::Duration;

use snorkle_core::notebook;
use snorkle_core::tabs::{Life, TabId};

use crate::state::{with_host, Shared};

type S<'a> = tauri::State<'a, Arc<Shared>>;

/// How long a page is shown without its view modes before a clip is
/// drawn (hidden images and colours show at once; a frame or two to
/// paint them).
const PLAIN_MS: u64 = 320;

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
pub async fn load_notebook(s: S<'_>) -> Result<Option<String>, String> {
    let shared = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || shared.store.as_ref().and_then(|st| st.notebook()))
        .await
        .map_err(|_| "Couldn't read the notebook.".to_string())
}

/// Stores the notebook (checked and rebuilt: `notebook::clean`).
#[tauri::command]
pub async fn save_notebook(s: S<'_>, notebook: String) -> Result<(), String> {
    if notebook.len() > notebook::MAX_NOTEBOOK_BYTES {
        return Err("The notebook is too large to save (64 MB). Delete some clips.".into());
    }
    let shared = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let store = shared.store.as_ref().ok_or("Saving is not available.")?;
        match store.set_notebook(&notebook) {
            Ok(true) => Ok(()),
            Ok(false) => Err("The notebook could not be saved.".to_string()),
            Err(e) => Err(format!("The notebook could not be saved: {e}")),
        }
    })
    .await
    .map_err(|_| "The notebook could not be saved.".to_string())?
}

/// A clip of a region of `tab`'s page (a C23 report's rectangle: visual
/// viewport CSS pixels, `vw` wide), as a `data:image/jpeg` URL. The page
/// is shown without its view modes for a moment (black and white, hidden
/// media, dark), so the clip has its colours and images; its appearance
/// is left as it is.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn notebook_clip(s: S<'_>, tab: u64, x: f64, y: f64, w: f64, h: f64, vw: f64) -> Result<String, String> {
    let tab = TabId(tab);
    capturable(&s, tab)?;
    tauri::async_runtime::spawn_blocking(move || {
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
        jpeg.filter(|j| j.starts_with(&[0xFF, 0xD8, 0xFF]) && j.len() <= snorkle_content::CLIP_MAX_BYTES)
            .map(|j| snorkle_core::storage::data_url("image/jpeg", &j))
            .ok_or_else(|| "This part of the page can't be clipped.".to_string())
    })
    .await
    .map_err(|_| "This part of the page can't be clipped.".to_string())?
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
