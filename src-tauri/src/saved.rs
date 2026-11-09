//! SECURITY-REVIEW: Save for Later's archives (owner request 2026-11-09;
//! the method of laffan/thedailyprophet, as WebKit web archives).
//!
//! Capture (channel C22): when the user saves a page, or adds pages to a
//! saved one, the tab's page is read three ways, all from the app's own
//! web view: WebKit's own archive of it (`createWebArchiveData`: the
//! resources WebKit holds, cross-origin ones included, and the live
//! document as a fallback), then the constant capture script in the
//! page's client world (`snorkle_core::later`): the page's HTML as the
//! server sends it and the URLs it loaded, then those resources WebKit did
//! not have, fetched with the page's session and handed over by index.
//! Everything is checked and bounded in `snorkle_core::later` before it is
//! written. Nothing from the page decides a file name or reaches the
//! chrome other than as text.
//!
//! Reading: the archive of one page is loaded with `loadData:` as a web
//! archive under the page's own URL; WebKit answers the page's requests
//! from it, so the page works offline. Links to other pages of the archive
//! open from the archive (`Browser::show_saved_copy`,
//! `Effect::OpenSavedPage`).

use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use snorkle_core::later::{self, PageCapture};
use snorkle_core::tabs::{Profile, SavedCopy, TabId};
use snorkle_core::webarchive;
use snorkle_core::Url;

use crate::state::{now_ms, with_host, Shared};

/// Largest WebKit archive read (its subresources and the live document).
const MAX_WEBKIT_ARCHIVE: usize = 96 * 1024 * 1024;
/// Resources fetched per round of the capture script.
const FETCH_BATCH: usize = 8;
/// The longest a whole capture runs; resources not fetched by then are
/// left out.
const CAPTURE_BUDGET: Duration = Duration::from_secs(90);

/// Waits on the main thread's answer, at most `limit`.
fn wait<T>(rx: mpsc::Receiver<T>, limit: Duration) -> Option<T> {
    rx.recv_timeout(limit).ok()
}

fn run_js(tab: TabId, body: String, max: usize, limit: Duration) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    snorkle_content::dispatch_main(move || {
        let tx2 = tx.clone();
        let mut ran = false;
        with_host(|h| {
            ran = true;
            h.run_capture(tab, &body, max, Box::new(move |r| drop(tx.send(r))));
        });
        if !ran {
            let _ = tx2.send(None);
        }
    });
    wait(rx, limit).flatten()
}

fn webkit_archive(tab: TabId) -> Option<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    snorkle_content::dispatch_main(move || {
        let tx2 = tx.clone();
        let mut ran = false;
        with_host(|h| {
            ran = true;
            h.web_archive(tab, MAX_WEBKIT_ARCHIVE, Box::new(move |r| drop(tx.send(r))));
        });
        if !ran {
            let _ = tx2.send(None);
        }
    });
    wait(rx, Duration::from_secs(30)).flatten()
}

/// Captures the page `tab` shows, as page `page_url` (blocking: call it
/// off the main thread). `None` when the tab has no live page to read.
pub fn capture(tab: TabId, page_url: &str, fallback_title: &str) -> Option<PageCapture> {
    let started = Instant::now();
    let wk = webkit_archive(tab);
    let (wk_main, mut resources) = match wk.as_deref() {
        Some(bytes) => (webarchive::read_main(bytes), webarchive::read_subresources(bytes).unwrap_or_default()),
        None => (None, Vec::new()),
    };
    drop(wk);
    let report = run_js(tab, later::CAPTURE_START_JS.to_string(), later::MAX_START_BYTES, Duration::from_secs(30))
        .and_then(|t| later::parse_start(&t));
    if let Some(report) = &report {
        let have: std::collections::HashSet<String> =
            resources.iter().filter_map(|r| later::clean_url(&r.url)).map(|u| u.to_string()).collect();
        let wanted: Vec<usize> = report
            .urls
            .iter()
            .enumerate()
            .filter(|(_, u)| u.as_ref().is_some_and(|u| !have.contains(u.as_str())))
            .map(|(i, _)| i)
            .collect();
        'fetch: for chunk in wanted.chunks(FETCH_BATCH) {
            if started.elapsed() > CAPTURE_BUDGET {
                break;
            }
            run_js(tab, later::fetch_js(chunk), 64, Duration::from_secs(45));
            loop {
                let Some(text) =
                    run_js(tab, later::DRAIN_JS.to_string(), later::MAX_DRAIN_BYTES, Duration::from_secs(30))
                else {
                    break 'fetch;
                };
                let Some((items, more)) = later::parse_drain(&text, &report.urls) else { break 'fetch };
                resources.extend(items);
                if !more {
                    break;
                }
            }
        }
        run_js(tab, later::END_JS.to_string(), 64, Duration::from_secs(5));
    }
    // The server's HTML runs its scripts as it did online; WebKit's copy of
    // the live document (when the HTML could not be fetched again) already
    // ran them and is shown with JavaScript off.
    let (html, scripts) = match (report.as_ref().and_then(|r| r.html.clone()), wk_main) {
        (Some(h), _) => (h.into_bytes(), true),
        (None, Some(m)) if m.data.len() <= later::MAX_PAGE_BYTES => (m.data, false),
        _ => return None,
    };
    let title = report
        .as_ref()
        .map(|r| r.title.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| fallback_title.to_string());
    Some(PageCapture { url: page_url.to_string(), title, html, scripts, resources })
}

/// Shows page `page` (the first page when `None`) of saved item `item` in
/// `tab`, or in a new tab when `tab` is `None` (blocking: reads the
/// archive). With `picking`, the page is shown with JavaScript off and link
/// taps choose pages to add. Returns the tab.
pub fn show(
    shared: &Arc<Shared>,
    tab: Option<TabId>,
    item: &str,
    page: Option<&str>,
    picking: bool,
) -> Result<TabId, String> {
    let store = shared.store.clone().ok_or("Saving is not available.")?;
    let open = store.later().archive(item, page).ok_or("This page has no saved copy.")?;
    let tab = match tab {
        Some(t) => {
            let b = shared.browser();
            let ok = b.tab(t).is_some_and(|t| !t.is_private());
            if !ok {
                return Err("That tab is gone.".into());
            }
            t
        }
        None => {
            let (t, effects) = shared.browser().new_tab(Profile::Default, now_ms());
            shared.run(effects);
            t
        }
    };
    let copy = SavedCopy {
        item: item.to_string(),
        page: open.page.clone(),
        pages: open.pages.clone(),
        saved_ms: open.saved_ms,
        committed: false,
        picking: picking.then(Vec::new),
    };
    let effects = shared.browser().show_saved_copy(tab, copy);
    shared.run(effects);
    if shared.browser().saved_copy(tab).is_none() {
        return Err("The saved copy could not be shown here.".into());
    }
    let (url, data, scripts) = (open.page, open.data, open.scripts && !picking);
    snorkle_content::dispatch_main(move || with_host(|h| h.load_archive(tab, &url, data, scripts)));
    Ok(tab)
}

/// `Effect::OpenSavedPage`: a link (or Reload) in a saved archive's tab.
pub fn open_page(shared: Arc<Shared>, tab: TabId, item: String, url: Url) {
    let _ = std::thread::Builder::new().name("snorkle-saved".into()).spawn(move || {
        if let Err(e) = show(&shared, Some(tab), &item, Some(url.as_str()), false) {
            shared.toast(&e);
        }
    });
}

/// Adds the chosen links' pages to a saved item (blocking): each is loaded
/// live in `tab`, captured once it has finished loading, and stored; then
/// the item's first page is shown again. Returns how many were added.
pub fn add_pages(shared: &Arc<Shared>, tab: TabId, item: &str, links: Vec<Url>) -> Result<usize, String> {
    let store = shared.store.clone().ok_or("Saving is not available.")?;
    let total = links.len();
    let mut captures = Vec::new();
    for (i, link) in links.into_iter().enumerate() {
        shared.toast(&format!("Adding page {} of {total}…", i + 1));
        let effects = shared.browser().navigate_trusted(tab, link.clone(), now_ms());
        shared.run(effects);
        if !wait_loaded(shared, tab, Duration::from_secs(40)) {
            continue;
        }
        // Lazy content and data requests settle after the load event.
        std::thread::sleep(Duration::from_millis(1500));
        let title = shared.browser().tab(tab).map(|t| t.title.clone()).unwrap_or_default();
        if let Some(c) = capture(tab, link.as_str(), &title) {
            captures.push(c);
        }
    }
    let added = captures.len();
    if added > 0 {
        store.later().add_pages(item, captures).map_err(|e| e.to_string())?;
    }
    show(shared, Some(tab), item, None, false)?;
    Ok(added)
}

/// Waits until `tab` has started and finished a load (or `limit`).
fn wait_loaded(shared: &Arc<Shared>, tab: TabId, limit: Duration) -> bool {
    let start = Instant::now();
    let mut seen_loading = false;
    while start.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(250));
        let b = shared.browser();
        let Some(t) = b.tab(tab) else { return false };
        if t.notice.is_some() {
            return false;
        }
        if t.loading || t.provisional {
            seen_loading = true;
        } else if seen_loading || start.elapsed() > Duration::from_secs(3) {
            return t.committed_url.is_some();
        }
    }
    false
}
