//! SECURITY-REVIEW: Tauri commands. Reachable only from the bundled chrome
//! UI in the `main` webview (capabilities/chrome.json; enforced by Tauri's
//! ACL and tested in tests/ipc_isolation.rs). Content web views have no IPC
//! at all.
//!
//! Even though the caller is trusted, every argument is treated as data:
//! tab ids that do not exist are ignored, strings are length-limited and
//! parsed by the core, numbers are clamped by the content host.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use snorkle_core::browser::ChromeState;
use snorkle_core::input::InputError;
use snorkle_core::tabs::{Profile, TabId};
use snorkle_core::url_display::{self, DisplayUrl};
use snorkle_core::viewmodes::{self, Heading, ViewModes};
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

/// "Google This" on selected text: a new tab (private if the current one
/// is) with a Google search for `query`. The URL is built here, so the
/// chrome holds no remote addresses.
#[tauri::command]
pub fn search_google(s: S<'_>, query: String, private: bool) -> Result<u64, String> {
    let q = snorkle_core::text::sanitize_page_text(&query, 300);
    if q.is_empty() {
        return Err("Nothing to search for.".into());
    }
    let url = Url::parse_with_params("https://www.google.com/search", [("q", q.as_str())]).map_err(|e| e.to_string())?;
    let profile = if private { Profile::Private } else { Profile::Default };
    let (id, effects) = s.browser().new_tab(profile, now_ms());
    s.run(effects);
    let effects = s.browser().navigate_trusted(id, url, now_ms());
    s.run(effects);
    Ok(id.0)
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

/// A rectangle where the chrome floats over the content area (CSS px =
/// points, window coordinates) and its corner radius.
#[derive(Deserialize)]
pub struct HoleArg {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    r: f64,
    /// Rounded corners (bits: 1 top left, 2 top right, 4 bottom left, 8
    /// bottom right); all when absent.
    c: Option<u8>,
}

/// `top`: bottom of the status bar, `left`: right edge of the docked tab
/// sidebar (0 when collapsed), `right`: width of a docked right panel,
/// `bottom`: height of the docked console, all in CSS px (= points).
/// `overlay`: the chrome is showing UI over the content area (menus, the
/// sidebar in narrow windows), so content must be hidden. `holes`: where
/// the URL pill, its popups and toasts float over the content area; pages
/// are cut out there (validated and bounded by the content host).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn set_chrome_layout(
    s: S<'_>,
    top: f64,
    left: f64,
    right: f64,
    bottom: f64,
    overlay: bool,
    holes: Vec<HoleArg>,
    animate_since: Option<f64>,
) {
    // Seconds since the chrome began its slide (so the host can line up).
    let animate = animate_since.filter(|t| t.is_finite()).map(|since| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(since);
        ((now - since) / 1000.0).clamp(0.0, 0.25)
    });
    let holes: Vec<snorkle_content::Hole> = holes
        .into_iter()
        .take(snorkle_content::MAX_HOLES)
        .map(|h| snorkle_content::Hole {
            x: h.x,
            y: h.y,
            w: h.w,
            h: h.h,
            r: h.r,
            corners: h.c.unwrap_or(snorkle_content::ALL_CORNERS),
        })
        .collect();
    snorkle_content::dispatch_main(move || with_host(|h| h.set_insets(top, left, right, bottom, holes, animate)));
    with_browser(&s, |b| b.set_chrome_overlay(overlay));
}

#[derive(Serialize)]
pub struct Entry {
    url: String,
    title: String,
    display: DisplayUrl,
    /// Wall-clock milliseconds of the visit (history only).
    #[serde(skip_serializing_if = "Option::is_none")]
    visited_ms: Option<u64>,
}

fn entry(url: &str, title: &str) -> Option<Entry> {
    let u = Url::parse(url).ok()?;
    Some(Entry { url: url.to_string(), title: title.to_string(), display: url_display::display(&u), visited_ms: None })
}

#[tauri::command]
pub fn list_bookmarks(s: S<'_>) -> Vec<Entry> {
    s.store
        .as_ref()
        .map(|st| st.bookmarks().iter().filter_map(|b| entry(&b.url, &b.title)).collect())
        .unwrap_or_default()
}

/// ⌘D / the Bookmark row. On a bookmark's own tab: removes the bookmark and
/// the tab becomes an ordinary tab ("removed"). Otherwise bookmarks the
/// tab's committed URL and moves the tab into the bookmark, so opening the
/// bookmark returns to it ("added"; "already" if another tab already is
/// that bookmark's). A pin's tab is left alone ("pinned"). The URL comes
/// from the core, not the caller; private tabs are never bookmarked.
#[tauri::command]
pub async fn toggle_bookmark(s: S<'_>, tab: u64) -> Result<&'static str, String> {
    let Some(store) = s.store.clone() else { return Err("Storage unavailable".into()) };
    let tab = TabId(tab);
    let (launcher, url, title) = {
        let b = s.browser();
        match b.tab(tab) {
            Some(t) if !t.is_private() => {
                (t.launcher.clone(), t.committed_url.as_ref().map(|u| u.to_string()), t.title.clone())
            }
            _ => return Ok("none"),
        }
    };
    // The tab is a bookmark's own tab: un-bookmark it (it becomes a normal
    // tab). A pin's tab is left alone.
    if let Some(key) = launcher {
        if !store.is_bookmarked(&key) {
            return Ok("pinned");
        }
        store.remove_bookmark(&key).map_err(|e| e.to_string())?;
        with_browser(&s, |b| b.release_launcher(&key));
        return Ok("removed");
    }
    let Some(url) = url else { return Ok("none") };
    let added = store.is_bookmarked(&url) || store.add_bookmark(&url, &title, wall_ms()).map_err(|e| e.to_string())?;
    if !added {
        return Ok("none");
    }
    // Move the tab into the bookmark (not a second tab for it).
    let (bound, effects) = s.browser().bind_launcher(tab, &url);
    s.run(effects);
    Ok(if bound { "added" } else { "already" })
}

#[tauri::command]
pub async fn remove_bookmark(s: S<'_>, url: String) -> Result<(), String> {
    with_browser(&s, |b| b.release_launcher(&url));
    match s.store.clone() {
        Some(store) => store.remove_bookmark(&url).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Opens a pin or bookmark in the tab that belongs to it (created on first
/// use; not listed with the other tabs).
#[tauri::command]
pub fn open_launcher(s: S<'_>, url: String) -> Result<u64, String> {
    let parsed = Url::parse(&url).map_err(|_| "Not a web address".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || url.len() > 4096 {
        return Err("Not a web address".into());
    }
    let (id, effects) = s.browser().open_launcher(&url, parsed, now_ms());
    s.run(effects);
    Ok(id.0)
}

#[tauri::command]
pub fn list_history(s: S<'_>, limit: usize) -> Vec<Entry> {
    let limit = limit.min(500);
    s.store
        .as_ref()
        .map(|st| {
            st.history(limit)
                .iter()
                .filter_map(|h| entry(&h.url, &h.title).map(|e| Entry { visited_ms: Some(h.visited_ms), ..e }))
                .collect()
        })
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
    let added = store.insert_bookmark(&url, &title, index, wall_ms()).map_err(|e| e.to_string())?;
    if added {
        // The dragged tab becomes the bookmark's tab (moved, not duplicated).
        let (_, effects) = s.browser().bind_launcher(TabId(tab), &url);
        s.run(effects);
    }
    Ok(added)
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

/// Ends the split pair holding `tab` (dragging a tab out of its pair, or
/// the toolbar's Unsplit button).
#[tauri::command]
pub fn unsplit(s: S<'_>, tab: u64) {
    with_browser(&s, |b| b.unsplit(TabId(tab)));
}

/// App-wide view modes and the tabs exempted from them.
#[derive(Serialize)]
pub struct ViewModesState {
    modes: ViewModes,
    /// Tab id → modes that tab is exempted from.
    exempt: std::collections::HashMap<u64, Vec<viewmodes::Kind>>,
    /// Tabs with the headers view on.
    headers_tabs: Vec<u64>,
}

fn view_modes_state(s: &S<'_>) -> ViewModesState {
    let exempt =
        s.exempt().iter().filter(|(_, k)| !k.is_empty()).map(|(t, k)| (t.0, k.iter().copied().collect())).collect();
    let headers_tabs = s.headers_tabs().iter().map(|t| t.0).collect();
    ViewModesState { modes: s.view_modes(), exempt, headers_tabs }
}

/// Applies view-mode changes to every page at once, and drops outlines of
/// tabs whose headers view is now off.
fn apply_view_modes(s: &S<'_>) {
    let tabs: Vec<TabId> = s.headings_of().keys().copied().collect();
    let cleared: Vec<TabId> = tabs.into_iter().filter(|t| !s.modes_for(*t).headers).collect();
    for t in cleared {
        s.headings_of().remove(&t);
        (s.emit)(crate::state::ChromeEvent::Headings(t, &[]));
    }
    crate::scripts::apply(s, true);
}

#[tauri::command]
pub fn get_view_mode(s: S<'_>) -> ViewModesState {
    view_modes_state(&s)
}

/// Turns a view mode on or off app-wide, applied immediately. Not persisted.
#[tauri::command]
pub fn set_view_mode(s: S<'_>, mode: viewmodes::Kind, on: bool) -> ViewModesState {
    s.view_modes.lock().unwrap_or_else(|e| e.into_inner()).set(mode, on);
    apply_view_modes(&s);
    view_modes_state(&s)
}

/// "Disable for this tab": exempts (or stops exempting) one tab from a
/// view mode. Saved with the session (non-private tabs).
#[tauri::command]
pub fn set_tab_view_exempt(s: S<'_>, tab: u64, mode: viewmodes::Kind, exempt: bool) -> ViewModesState {
    let tab = TabId(tab);
    if s.browser().tab(tab).is_some() {
        let kinds: Vec<viewmodes::Kind> = {
            let mut map = s.exempt();
            let set = map.entry(tab).or_default();
            if exempt {
                set.insert(mode);
            } else {
                set.remove(&mode);
            }
            set.iter().copied().collect()
        };
        with_browser(&s, |b| b.set_view_exempt(tab, &kinds));
    }
    apply_view_modes(&s);
    view_modes_state(&s)
}

/// Turns the headers view on or off for one tab.
#[tauri::command]
pub fn set_tab_headers(s: S<'_>, tab: u64, on: bool) -> ViewModesState {
    let tab = TabId(tab);
    if s.browser().tab(tab).is_some() {
        if on {
            s.headers_tabs().insert(tab);
        } else {
            s.headers_tabs().remove(&tab);
        }
    }
    apply_view_modes(&s);
    view_modes_state(&s)
}

/// Find in page (⌘F): WebKit selects and scrolls to the next (or previous)
/// match, wrapping. Returns whether a match was found.
#[tauri::command]
pub async fn find_in_page(tab: u64, query: String, backwards: bool) -> Result<bool, String> {
    let Some(query) = snorkle_core::find::validate_query(&query) else { return Ok(false) };
    let (tx, rx) = std::sync::mpsc::channel();
    snorkle_content::dispatch_main(move || {
        with_host(|h| {
            h.find(
                TabId(tab),
                query,
                backwards,
                Box::new(move |found| {
                    let _ = tx.send(found);
                }),
            )
        })
    });
    tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(4)).unwrap_or(false))
        .await
        .map_err(|_| "Find failed.".to_string())
}

/// Number of matches of `query` on the tab's page (case-insensitive, in
/// its visible text; channel C18, digits only), or `None` if the page did
/// not answer.
#[tauri::command]
pub async fn count_in_page(tab: u64, query: String) -> Result<Option<u32>, String> {
    let Some(query) = snorkle_core::find::validate_query(&query) else { return Ok(Some(0)) };
    let (tx, rx) = std::sync::mpsc::channel();
    snorkle_content::dispatch_main(move || {
        with_host(|h| h.count_matches(TabId(tab), query, Box::new(move |t| drop(tx.send(t)))))
    });
    let text = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(4)))
        .await
        .map_err(|_| "Find failed.".to_string())?;
    Ok(text.ok().flatten().and_then(|t| snorkle_core::find::parse_count(&t)))
}

/// Gives the chrome's web view keyboard focus (after a shortcut, a page or
/// nothing may have it), so the input the chrome focuses next gets typing.
#[tauri::command]
pub fn focus_chrome() {
    snorkle_content::dispatch_main(|| with_host(|h| h.focus_chrome()));
}

/// The system share sheet for the tab's page (its committed http(s) URL,
/// from the core), pointing at the share button (`x`…`h`, window points).
#[tauri::command]
pub fn share_page(s: S<'_>, tab: u64, x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
    let url = s
        .browser()
        .tab(TabId(tab))
        .and_then(|t| t.committed_url.clone())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or("Open a web page to share it.")?
        .to_string();
    snorkle_content::dispatch_main(move || with_host(|host| host.share(&url, (x, y, w, h))));
    Ok(())
}

/// The text selected on the tab's page (channel C20), cleaned and capped,
/// or `None`.
#[tauri::command]
pub async fn read_selection(tab: u64) -> Result<Option<String>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    snorkle_content::dispatch_main(move || {
        with_host(|h| h.read_selection(TabId(tab), Box::new(move |t| drop(tx.send(t)))))
    });
    let text = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(4)))
        .await
        .map_err(|_| "Couldn't read the selection.".to_string())?;
    Ok(text.ok().flatten().and_then(|t| snorkle_core::selection::clean_text(&t)))
}

/// The system dictionary for `term` (selected text), as a popover at `x`…`h`.
#[tauri::command]
pub fn define_term(term: String, x: f64, y: f64, w: f64, h: f64) {
    let term = snorkle_core::text::sanitize_page_text(&term, 100);
    if term.is_empty() {
        return;
    }
    snorkle_content::dispatch_main(move || with_host(|host| host.define(&term, (x, y, w, h))));
}

/// A keyboard shortcut for the settings editor.
#[derive(Serialize)]
pub struct ShortcutInfo {
    id: &'static str,
    title: &'static str,
    /// Name the chrome performs (`Shortcut::name`).
    action: String,
    key: String,
    mods: u8,
    default_key: &'static str,
    default_mods: u8,
}

fn shortcut_infos(s: &S<'_>) -> Vec<ShortcutInfo> {
    let current = s.shortcuts.lock().unwrap_or_else(|e| e.into_inner()).clone();
    snorkle_content::SHORTCUTS
        .iter()
        .zip(current)
        .map(|(d, b)| ShortcutInfo {
            id: d.id,
            title: d.title,
            action: d.action.name(),
            key: b.key,
            mods: b.mods,
            default_key: d.key,
            default_mods: d.mods,
        })
        .collect()
}

#[tauri::command]
pub fn get_shortcuts(s: S<'_>) -> Vec<ShortcutInfo> {
    shortcut_infos(&s)
}

#[derive(Deserialize)]
pub struct BindingArg {
    id: String,
    key: String,
    mods: u8,
}

/// The user's bindings (applied over the defaults; a clash is refused) and
/// whether shortcuts are suspended while the editor records a key. The
/// menu bar is rebuilt.
#[tauri::command]
pub fn set_shortcuts(s: S<'_>, bindings: Vec<BindingArg>, suspend: bool) -> Result<Vec<ShortcutInfo>, String> {
    let overrides: Vec<(String, String, u8)> = bindings.into_iter().take(64).map(|b| (b.id, b.key, b.mods)).collect();
    let effective = snorkle_content::effective_bindings(&overrides)?;
    *s.shortcuts.lock().unwrap_or_else(|e| e.into_inner()) = effective.clone();
    snorkle_content::dispatch_main(move || with_host(|h| h.set_shortcuts(effective, suspend)));
    Ok(shortcut_infos(&s))
}

/// Closing the find bar: clears the match selection on the page.
#[tauri::command]
pub fn end_find(tab: u64) {
    snorkle_content::dispatch_main(move || with_host(|h| h.end_find(TabId(tab))));
}

/// The latest outline of a tab's page (headers view).
#[tauri::command]
pub fn get_headings(s: S<'_>, tab: u64) -> Vec<Heading> {
    s.headings_of().get(&TabId(tab)).cloned().unwrap_or_default()
}

/// Scrolls a tab's page to one of its headings (headers view).
#[tauri::command]
pub fn scroll_to_heading(tab: u64, index: u32) {
    snorkle_content::dispatch_main(move || with_host(|h| h.scroll_to_heading(TabId(tab), index)));
}

#[derive(Serialize)]
pub struct ConsoleState {
    capture: bool,
    entries: Vec<snorkle_core::console::Entry>,
}

#[tauri::command]
pub fn get_console(s: S<'_>) -> ConsoleState {
    ConsoleState { capture: s.capturing(), entries: s.console_log().entries() }
}

/// Starts or stops recording, and installs or removes the page console
/// hook (channel C13) in every content view, from their next load.
#[tauri::command]
pub fn set_console_capture(s: S<'_>, on: bool) {
    s.console_capture.store(on, std::sync::atomic::Ordering::Relaxed);
    snorkle_content::dispatch_main(move || with_host(|h| h.set_console_capture(on)));
}

#[tauri::command]
pub fn clear_console(s: S<'_>) {
    s.console_log().clear();
}

/// Deletes one kind of website data (Settings): "cache", "cookies",
/// "site_data" or "logins".
#[tauri::command]
pub fn clear_website_data(kind: String) -> Result<(), String> {
    use snorkle_content::DataKind;
    let kind = match kind.as_str() {
        "cache" => DataKind::Cache,
        "cookies" => DataKind::Cookies,
        "site_data" => DataKind::SiteData,
        "logins" => DataKind::Logins,
        _ => return Err("Unknown data kind".into()),
    };
    snorkle_content::dispatch_main(move || with_host(|h| h.clear_data(kind)));
    Ok(())
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
    crate::scripts::apply(&s, false);
    Ok(saved)
}

#[tauri::command]
pub async fn delete_user_script(s: S<'_>, domain: String) -> Result<(), String> {
    let Some(store) = s.store.clone() else { return Err("Storage unavailable".into()) };
    store.delete_user_script(&domain).map_err(|e| e.to_string())?;
    crate::scripts::apply(&s, false);
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
    // Moved into the bookmarks, it keeps its tab; removed, the tab is listed again.
    if bookmark_index.is_none() {
        with_browser(&s, |b| b.release_launcher(&url));
    }
    let Some(store) = s.store.clone() else { return Ok(()) };
    if let (Some(p), Some(i)) = (store.remove_pin(&url).map_err(|e| e.to_string())?, bookmark_index) {
        store.insert_bookmark(&p.url, &p.title, i, p.created_ms).map_err(|e| e.to_string())?;
    }
    Ok(())
}
