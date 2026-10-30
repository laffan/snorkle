//! SECURITY-REVIEW: Tauri commands. Reachable only from the bundled chrome
//! UI in the `main` webview (capabilities/chrome.json; enforced by Tauri's
//! ACL and tested in tests/ipc_isolation.rs). Content web views have no IPC
//! at all.
//!
//! Even though the caller is trusted, every argument is treated as data:
//! tab ids that do not exist are ignored, strings are length-limited and
//! parsed by the core, numbers are clamped by the content host.

use std::sync::Arc;

use serde::Serialize;
use snorkle_core::browser::ChromeState;
use snorkle_core::input::InputError;
use snorkle_core::tabs::{Profile, TabId};
use snorkle_core::url_display::{self, DisplayUrl};
use snorkle_core::Url;

use crate::state::{now_ms, with_host, Shared};

type S<'a> = tauri::State<'a, Arc<Shared>>;

fn with_browser<F>(s: &S<'_>, f: F)
where
    F: FnOnce(&mut snorkle_core::browser::Browser) -> Vec<snorkle_core::browser::Effect>,
{
    let effects = f(&mut s.browser());
    s.run(effects);
}

#[tauri::command]
pub fn get_state(s: S<'_>) -> ChromeState {
    s.view()
}

#[tauri::command]
pub fn navigate(s: S<'_>, tab: u64, input: String) -> Result<(), String> {
    let result = s.browser().navigate_input(TabId(tab), &input, now_ms());
    match result {
        Ok(effects) => {
            s.run(effects);
            Ok(())
        }
        Err(InputError::Empty) => Ok(()),
        Err(InputError::TooLong) => Err("That address is too long.".into()),
    }
}

#[tauri::command]
pub fn new_tab(s: S<'_>, private: bool) -> u64 {
    let profile = if private { Profile::Private } else { Profile::Default };
    let (id, effects) = s.browser().new_tab(profile, now_ms());
    s.run(effects);
    id.0
}

#[tauri::command]
pub fn close_tab(s: S<'_>, tab: u64) {
    with_browser(&s, |b| b.close(TabId(tab), now_ms()));
}

#[tauri::command]
pub fn activate_tab(s: S<'_>, tab: u64) {
    with_browser(&s, |b| b.activate(TabId(tab), now_ms()));
}

#[tauri::command]
pub fn go_back(s: S<'_>, tab: u64) {
    with_browser(&s, |b| b.go_back(TabId(tab)));
}

#[tauri::command]
pub fn go_forward(s: S<'_>, tab: u64) {
    with_browser(&s, |b| b.go_forward(TabId(tab)));
}

#[tauri::command]
pub fn reload(s: S<'_>, tab: u64) {
    with_browser(&s, |b| b.reload(TabId(tab)));
}

#[tauri::command]
pub fn stop(s: S<'_>, tab: u64) {
    with_browser(&s, |b| b.stop(TabId(tab)));
}

/// The user clicked "Continue to HTTP" on the HTTPS-first interstitial.
#[tauri::command]
pub fn proceed_http(s: S<'_>, tab: u64) {
    with_browser(&s, |b| b.proceed_http(TabId(tab), now_ms()));
}

/// The user confirmed opening another app.
#[tauri::command]
pub fn confirm_external(s: S<'_>, tab: u64) {
    with_browser(&s, |b| b.confirm_external(TabId(tab)));
}

#[tauri::command]
pub fn dismiss_notice(s: S<'_>, tab: u64) {
    with_browser(&s, |b| b.dismiss_notice(TabId(tab)));
}

/// `top`: bottom edge of the toolbar, `left`: right edge of the docked tab
/// sidebar (0 when collapsed), both in CSS px (= points). `overlay`: the
/// chrome is showing UI over the content area (menus, the sidebar in narrow
/// windows), so content must be hidden.
#[tauri::command]
pub fn set_chrome_layout(s: S<'_>, top: f64, left: f64, overlay: bool) {
    snorkle_content::dispatch_main(move || with_host(|h| h.set_insets(top, left)));
    with_browser(&s, |b| b.set_chrome_overlay(overlay));
}

#[derive(Serialize)]
pub struct Entry {
    url: String,
    title: String,
    display: DisplayUrl,
}

fn entry(url: &str, title: &str) -> Option<Entry> {
    let u = Url::parse(url).ok()?;
    Some(Entry { url: url.to_string(), title: title.to_string(), display: url_display::display(&u) })
}

#[tauri::command]
pub fn list_bookmarks(s: S<'_>) -> Vec<Entry> {
    s.store
        .as_ref()
        .map(|st| st.bookmarks().iter().filter_map(|b| entry(&b.url, &b.title)).collect())
        .unwrap_or_default()
}

/// Bookmarks or un-bookmarks the tab's committed URL. The URL is taken from
/// the core, not from the caller. Private tabs cannot be bookmarked (that
/// would persist private browsing data).
#[tauri::command]
pub async fn toggle_bookmark(s: S<'_>, tab: u64) -> Result<bool, String> {
    let (url, title) = {
        let b = s.browser();
        match b.tab(TabId(tab)) {
            Some(t) if !t.is_private() => match &t.committed_url {
                Some(u) => (u.to_string(), t.title.clone()),
                None => return Ok(false),
            },
            _ => return Ok(false),
        }
    };
    let Some(store) = s.store.clone() else { return Err("Storage unavailable".into()) };
    if store.is_bookmarked(&url) {
        store.remove_bookmark(&url).map_err(|e| e.to_string())?;
        Ok(false)
    } else {
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        store.add_bookmark(&url, &title, wall).map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub async fn remove_bookmark(s: S<'_>, url: String) -> Result<(), String> {
    match s.store.clone() {
        Some(store) => store.remove_bookmark(&url).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

#[tauri::command]
pub fn list_history(s: S<'_>, limit: usize) -> Vec<Entry> {
    let limit = limit.min(500);
    s.store
        .as_ref()
        .map(|st| st.history(limit).iter().filter_map(|h| entry(&h.url, &h.title)).collect())
        .unwrap_or_default()
}

#[tauri::command]
pub async fn clear_history(s: S<'_>) -> Result<(), String> {
    match s.store.clone() {
        Some(store) => store.clear_history().map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Turns ad blocking off for the tab's site, or back on. The site is derived
/// from the core's committed URL, not supplied by the caller. Not available
/// in private tabs, because the setting is persisted. Returns whether
/// blocking is now on for the site.
#[tauri::command]
pub async fn toggle_site_adblock(s: S<'_>, tab: u64) -> Result<bool, String> {
    let site = {
        let b = s.browser();
        let t = b.tab(TabId(tab)).ok_or("No such tab")?;
        if t.is_private() {
            return Err("Ad-blocking exceptions can't be changed in a private tab.".into());
        }
        t.committed_url.as_ref().and_then(snorkle_core::blocking::site_key).ok_or("No site to change")?
    };
    let Some(store) = s.store.clone() else { return Err("Storage unavailable".into()) };
    let currently_allowed = store.adblock_allowed().contains(&site);
    let set = store.set_adblock_allowed(&site, !currently_allowed).map_err(|e| e.to_string())?;
    let effects = s.browser().set_adblock_allowlist(set);
    s.run(effects);
    crate::adblock::rebuild(&s, Some(TabId(tab)));
    Ok(currently_allowed)
}

/// Bookmarks the tab's committed URL at `index` in the bookmarks bar
/// (dragging a tab into the bar). Private tabs cannot be bookmarked.
#[tauri::command]
pub async fn bookmark_tab(s: S<'_>, tab: u64, index: usize) -> Result<bool, String> {
    let (url, title) = {
        let b = s.browser();
        match b.tab(TabId(tab)) {
            Some(t) if !t.is_private() => match &t.committed_url {
                Some(u) => (u.to_string(), t.title.clone()),
                None => return Ok(false),
            },
            _ => return Ok(false),
        }
    };
    let Some(store) = s.store.clone() else { return Err("Storage unavailable".into()) };
    store.insert_bookmark(&url, &title, index, wall_ms()).map_err(|e| e.to_string())
}

/// Reorders the bookmarks bar.
#[tauri::command]
pub async fn move_bookmark(s: S<'_>, url: String, index: usize) -> Result<(), String> {
    match s.store.clone() {
        Some(store) => store.move_bookmark(&url, index).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Shows `tab` beside `with` (dragging one tab onto another).
#[tauri::command]
pub fn split_tabs(s: S<'_>, tab: u64, with: u64) {
    with_browser(&s, |b| b.split(TabId(tab), TabId(with), now_ms()));
}

#[tauri::command]
pub fn unsplit(s: S<'_>) {
    with_browser(&s, |b| b.unsplit());
}

#[tauri::command]
pub fn get_settings(s: S<'_>) -> snorkle_core::storage::Settings {
    s.store.as_ref().map(|st| st.settings()).unwrap_or_default()
}

/// Black-and-white rendering for all pages, applied immediately.
#[tauri::command]
pub async fn set_grayscale(s: S<'_>, on: bool) -> Result<(), String> {
    let Some(store) = s.store.clone() else { return Err("Storage unavailable".into()) };
    let mut settings = store.settings();
    settings.grayscale = on;
    store.set_settings(settings).map_err(|e| e.to_string())?;
    crate::scripts::apply(&s, Some(snorkle_core::userscripts::grayscale_toggle_js(on)));
    Ok(())
}

/// Empties the HTTP caches (not cookies or site data).
#[tauri::command]
pub fn empty_cache() {
    snorkle_content::dispatch_main(|| with_host(|h| h.clear_cache()));
}

#[tauri::command]
pub fn list_user_scripts(s: S<'_>) -> Vec<snorkle_core::userscripts::UserScript> {
    s.store.as_ref().map(|st| st.user_scripts()).unwrap_or_default()
}

/// Saves (adds or replaces by domain) a user script. Returns the stored,
/// normalised entry. Applies from the next load of matching pages.
#[tauri::command]
pub async fn save_user_script(
    s: S<'_>,
    domain: String,
    css: String,
    js: String,
    enabled: bool,
) -> Result<snorkle_core::userscripts::UserScript, String> {
    let Some(store) = s.store.clone() else { return Err("Storage unavailable".into()) };
    let saved = store
        .save_user_script(snorkle_core::userscripts::UserScript { domain, css, js, enabled })
        .map_err(|e| e.to_string())?;
    crate::scripts::apply(&s, None);
    Ok(saved)
}

#[tauri::command]
pub async fn delete_user_script(s: S<'_>, domain: String) -> Result<(), String> {
    let Some(store) = s.store.clone() else { return Err("Storage unavailable".into()) };
    store.delete_user_script(&domain).map_err(|e| e.to_string())?;
    crate::scripts::apply(&s, None);
    Ok(())
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[derive(Serialize)]
pub struct PinEntry {
    url: String,
    title: String,
    display: DisplayUrl,
    /// `data:image/png;base64,…` made by the app from the sandboxed
    /// snapshot, or `None` (the chrome shows a letter tile).
    icon: Option<String>,
}

#[tauri::command]
pub fn list_pins(s: S<'_>) -> Vec<PinEntry> {
    let Some(store) = s.store.as_ref() else { return Vec::new() };
    store
        .pins()
        .into_iter()
        .filter_map(|p| {
            let display = url_display::display(&Url::parse(&p.url).ok()?);
            let icon = snorkle_core::storage::favicon_host(&p.url)
                .and_then(|h| store.icon(&h))
                .and_then(|png| snorkle_core::storage::png_data_url(&png));
            Some(PinEntry { url: p.url, title: p.title, display, icon })
        })
        .collect()
}

/// Pins the tab's committed URL (dragging a tab into the grid). Not for
/// private tabs. False when the grid is full or the URL is already pinned.
#[tauri::command]
pub async fn pin_tab(s: S<'_>, tab: u64, index: usize) -> Result<bool, String> {
    let (url, title) = {
        let b = s.browser();
        match b.tab(TabId(tab)) {
            Some(t) if !t.is_private() => match &t.committed_url {
                Some(u) => (u.to_string(), t.title.clone()),
                None => return Ok(false),
            },
            _ => return Ok(false),
        }
    };
    let Some(store) = s.store.clone() else { return Err("Storage unavailable".into()) };
    let added = store.insert_pin(&url, &title, index, wall_ms()).map_err(|e| e.to_string())?;
    crate::pins::refresh_icons(&s);
    Ok(added)
}

/// Moves a bookmark into the pinned grid.
#[tauri::command]
pub async fn pin_bookmark(s: S<'_>, url: String, index: usize) -> Result<bool, String> {
    let Some(store) = s.store.clone() else { return Err("Storage unavailable".into()) };
    if store.pins().len() >= snorkle_core::storage::MAX_PINS {
        return Ok(false);
    }
    let Some(b) = store.take_bookmark(&url).map_err(|e| e.to_string())? else { return Ok(false) };
    let added = store.insert_pin(&b.url, &b.title, index, b.created_ms).map_err(|e| e.to_string())?;
    if !added {
        // Already pinned: keep the bookmark where it was not lost.
        store.insert_bookmark(&b.url, &b.title, usize::MAX, b.created_ms).map_err(|e| e.to_string())?;
    }
    crate::pins::refresh_icons(&s);
    Ok(added)
}

#[tauri::command]
pub async fn move_pin(s: S<'_>, url: String, index: usize) -> Result<(), String> {
    match s.store.clone() {
        Some(store) => store.move_pin(&url, index).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Removes a pin; with `bookmark_index`, moves it into the bookmarks bar
/// at that position instead of dropping it.
#[tauri::command]
pub async fn unpin(s: S<'_>, url: String, bookmark_index: Option<usize>) -> Result<(), String> {
    let Some(store) = s.store.clone() else { return Ok(()) };
    if let (Some(p), Some(i)) = (store.remove_pin(&url).map_err(|e| e.to_string())?, bookmark_index) {
        store.insert_bookmark(&p.url, &p.title, i, p.created_ms).map_err(|e| e.to_string())?;
    }
    Ok(())
}
