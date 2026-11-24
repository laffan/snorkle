//! Per-page actions: ad blocking, bookmarking a tab, split views and tab
//! order, labels, view modes, find, the image menu, appearance and gestures.

use super::*;

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
/// (dragging a tab into the bar), in bookmark group `group` if given (it
/// was dropped among a group's bookmarks). Private tabs cannot be
/// bookmarked.
#[tauri::command]
pub async fn bookmark_tab(s: S<'_>, tab: u64, index: usize, group: Option<String>) -> Result<bool, String> {
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
    let added = store.insert_bookmark_in(&url, &title, index, group.as_deref(), wall_ms()).map_err(|e| e.to_string())?;
    if added {
        // The dragged tab becomes the bookmark's tab (moved, not duplicated).
        let (_, effects) = s.browser().bind_launcher(TabId(tab), &url);
        s.run(effects);
    }
    Ok(added)
}

/// Reorders the bookmarks bar; the bookmark joins `group` (dropped among
/// its bookmarks) or leaves its group (`None`; 2026-11-24).
#[tauri::command]
pub async fn move_bookmark(s: S<'_>, url: String, index: usize, group: Option<String>) -> Result<(), String> {
    match s.store.clone() {
        Some(store) => store.place_bookmark(&url, index, group.as_deref()).map(|_| ()).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Shows `tab` beside `with` (dragging one tab onto another).
#[tauri::command]
pub fn split_tabs(s: S<'_>, tab: u64, with: u64) {
    with_browser(&s, |b| b.split(TabId(tab), TabId(with), now_ms()));
}

/// Dragging a tab in the sidebar: moves it just before `before`, or to
/// the end of the list, into tab group `group` (dropped next to one of its
/// tabs) or out of any.
#[tauri::command]
pub fn move_tab(s: S<'_>, tab: u64, before: Option<u64>, group: Option<u64>) {
    with_browser(&s, |b| b.move_tab(TabId(tab), before.map(TabId), group));
}

/// The Edit dialog for a tab: its own name (shown instead of the page's
/// title; empty clears it) and an emoji shown instead of its favicon.
#[tauri::command]
pub fn set_tab_label(s: S<'_>, tab: u64, name: Option<String>, emoji: Option<String>) -> Result<(), String> {
    let effects = s.browser().set_tab_label(TabId(tab), name.as_deref(), emoji.as_deref())?;
    s.run(effects);
    Ok(())
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
    /// What each launch starts with (Settings → General).
    launch: viewmodes::LaunchModes,
    /// Tab id → modes that tab is exempted from.
    exempt: std::collections::HashMap<u64, Vec<viewmodes::Kind>>,
}

pub(super) fn view_modes_state(s: &S<'_>) -> ViewModesState {
    let exempt =
        s.exempt().iter().filter(|(_, k)| !k.is_empty()).map(|(t, k)| (t.0, k.iter().copied().collect())).collect();
    let launch = s.store.as_ref().map(|st| st.launch_modes()).unwrap_or_default();
    ViewModesState { modes: s.view_modes(), launch, exempt }
}

/// Applies view-mode changes to every page at once.
pub(super) fn apply_view_modes(s: &S<'_>) {
    crate::scripts::apply(s, true);
}

#[tauri::command]
pub fn get_view_mode(s: S<'_>) -> ViewModesState {
    view_modes_state(&s)
}

/// Turns a view mode on or off app-wide, applied immediately. Dark is
/// remembered for the next launch; the others last for this session.
#[tauri::command]
pub fn set_view_mode(s: S<'_>, mode: viewmodes::Kind, on: bool) -> ViewModesState {
    s.view_modes.lock().unwrap_or_else(|e| e.into_inner()).set(mode, on);
    if mode == viewmodes::Kind::Dark {
        store_launch_mode(&s, mode, on);
    }
    apply_view_modes(&s);
    view_modes_state(&s)
}

/// Settings → General: what a view mode starts as at every launch. Also
/// applied now, app-wide, so the setting takes effect when it is changed.
#[tauri::command]
pub fn set_launch_mode(s: S<'_>, mode: viewmodes::Kind, on: bool) -> ViewModesState {
    store_launch_mode(&s, mode, on);
    s.view_modes.lock().unwrap_or_else(|e| e.into_inner()).set(mode, on);
    apply_view_modes(&s);
    view_modes_state(&s)
}

pub(super) fn store_launch_mode(s: &S<'_>, mode: viewmodes::Kind, on: bool) {
    if let Some(store) = s.store.as_ref() {
        if let Err(e) = store.set_launch_mode(mode, on) {
            s.log(snorkle_core::console::Level::Warn, None, &format!("view modes not saved: {e}"));
        }
    }
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

/// Find in page: highlights the matches of `query` (`step`: start, next or
/// prev) and returns the current match's position, or `None` if the page
/// did not answer (channel C18, `find::parse_position`).
#[tauri::command]
pub async fn find_in_page(
    tab: u64,
    query: String,
    step: snorkle_core::find::FindStep,
) -> Result<Option<snorkle_core::find::FindPosition>, String> {
    let Some(query) = snorkle_core::find::validate_query(&query) else {
        return Ok(Some(snorkle_core::find::FindPosition { index: 0, total: 0 }));
    };
    let (tx, rx) = std::sync::mpsc::channel();
    snorkle_content::dispatch_main(move || {
        with_host(|h| h.find(TabId(tab), query, step, Box::new(move |t| drop(tx.send(t)))))
    });
    let text = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(4)))
        .await
        .map_err(|_| "Find failed.".to_string())?;
    Ok(text.ok().flatten().and_then(|t| snorkle_core::find::parse_position(&t)))
}

/// The split divider was dragged: the first pane's share, in permille.
#[tauri::command]
pub fn set_split_ratio(s: S<'_>, permille: u16) {
    with_browser(&s, |b| b.set_split_ratio(permille));
}

/// ⌘S / ⌘⇧S: split the view with a new empty tab (returned), or change
/// the layout of the split the active tab is in (`None`).
#[tauri::command]
pub fn split_new(s: S<'_>, orientation: snorkle_core::browser::SplitOrientation) -> Option<u64> {
    let (id, effects) = s.browser().split_new(orientation, now_ms());
    s.run(effects);
    id.map(|t| t.0)
}

/// Image menu: Copy, Share or Download the image at `url` (an http(s)
/// URL from the page's report, re-validated here). The image is fetched
/// by the host (`http::fetch_image`), then put on the pasteboard, shared
/// from `x`…`h` (window points), or exported to Files.
#[tauri::command]
pub async fn image_action(
    url: String,
    action: snorkle_core::imagemenu::ImageAction,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<(), String> {
    let parsed = Url::parse(&url).map_err(|_| "Not a web image.".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || url.len() > snorkle_core::imagemenu::MAX_URL_CHARS {
        return Err("Not a web image.".into());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let target = parsed.clone();
    snorkle_content::dispatch_main(move || {
        snorkle_content::http::fetch_image(&target, Box::new(move |r| drop(tx.send(r))));
    });
    let fetched = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(35)))
        .await
        .map_err(|_| "The image could not be loaded.".to_string())?
        .map_err(|_| "The image took too long to load.".to_string())??;
    let (data, mime) = fetched;
    let (tx, rx) = std::sync::mpsc::channel();
    snorkle_content::dispatch_main(move || {
        let mut result = Err("The image could not be used.".to_string());
        with_host(|host| result = host.image_result(action, &parsed, data, &mime, (x, y, w, h)));
        let _ = tx.send(result);
    });
    tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(5)))
        .await
        .map_err(|_| "The image could not be used.".to_string())?
        .unwrap_or(Ok(()))
}

/// The app's appearance (Settings → General; the chrome stores it).
#[tauri::command]
pub fn set_appearance(mode: String) {
    let mode = snorkle_content::Appearance::from_name(&mode);
    snorkle_content::dispatch_main(move || with_host(|h| h.set_appearance(mode)));
}

/// Settings → General → Swipe between pages: WebKit's back/forward swipe
/// on every page (now and later). Sent by the chrome at launch.
#[tauri::command]
pub fn set_swipe_navigation(on: bool) {
    snorkle_content::set_swipe_navigation(on);
    snorkle_content::dispatch_main(move || with_host(|h| h.set_swipe_navigation(on)));
}

/// Whether text is being edited with keyboard focus, in the chrome or in a
/// page (the chrome combines its own focus with the pages' reports, C19):
/// arrow-key shortcuts (⌘← back, …) are left to the text meanwhile. A
/// boolean only.
#[tauri::command]
pub fn set_text_editing(on: bool) {
    if snorkle_content::text_editing() == on {
        return;
    }
    snorkle_content::set_text_editing(on);
    snorkle_content::dispatch_main(|| with_host(|h| h.text_editing_changed()));
}
