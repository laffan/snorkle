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

fn with<F>(s: &S<'_>, f: F)
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
    with(&s, |b| b.close(TabId(tab), now_ms()));
}

#[tauri::command]
pub fn activate_tab(s: S<'_>, tab: u64) {
    with(&s, |b| b.activate(TabId(tab), now_ms()));
}

#[tauri::command]
pub fn go_back(s: S<'_>, tab: u64) {
    with(&s, |b| b.go_back(TabId(tab)));
}

#[tauri::command]
pub fn go_forward(s: S<'_>, tab: u64) {
    with(&s, |b| b.go_forward(TabId(tab)));
}

#[tauri::command]
pub fn reload(s: S<'_>, tab: u64) {
    with(&s, |b| b.reload(TabId(tab)));
}

#[tauri::command]
pub fn stop(s: S<'_>, tab: u64) {
    with(&s, |b| b.stop(TabId(tab)));
}

/// The user clicked "Continue to HTTP" on the HTTPS-first interstitial.
#[tauri::command]
pub fn proceed_http(s: S<'_>, tab: u64) {
    with(&s, |b| b.proceed_http(TabId(tab), now_ms()));
}

/// The user confirmed opening another app.
#[tauri::command]
pub fn confirm_external(s: S<'_>, tab: u64) {
    with(&s, |b| b.confirm_external(TabId(tab)));
}

#[tauri::command]
pub fn dismiss_notice(s: S<'_>, tab: u64) {
    with(&s, |b| b.dismiss_notice(TabId(tab)));
}

/// `top`: bottom edge of the toolbar, `left`: right edge of the docked tab
/// sidebar (0 when collapsed), both in CSS px (= points). `overlay`: the
/// chrome is showing UI over the content area (menus, the sidebar in narrow
/// windows), so content must be hidden.
#[tauri::command]
pub fn set_chrome_layout(s: S<'_>, top: f64, left: f64, overlay: bool) {
    snorkle_content::dispatch_main(move || with_host(|h| h.set_insets(top, left)));
    with(&s, |b| b.set_chrome_overlay(overlay));
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
