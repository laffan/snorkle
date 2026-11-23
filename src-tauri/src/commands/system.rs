//! Links to apps, focus, the URL bar, scale, sharing, selection and Define,
//! keyboard shortcuts, the summary, the console, website data, site scripts.

use super::*;

/// Settings → General → Open links in apps (default off): whether WebKit
/// may hand a page to an installed app that claims its address. Sent by
/// the chrome at launch.
#[tauri::command]
pub fn set_open_app_links(on: bool) {
    snorkle_content::set_open_app_links(on);
}

/// ⋯ → Open in App (iPadOS): offers the tab's committed page (http/https,
/// from the core, never from the chrome) to the app that claims its
/// address. Universal links only, so nothing else can be opened this way.
/// Whether an app opened.
#[tauri::command]
pub async fn open_in_app(s: S<'_>, tab: u64) -> Result<bool, String> {
    let url = s
        .browser()
        .tab(TabId(tab))
        .and_then(|t| t.committed_url.clone())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or("Open a web page first.")?;
    let (tx, rx) = std::sync::mpsc::channel();
    snorkle_content::dispatch_main(move || with_host(|h| h.open_in_app(&url, Box::new(move |ok| { let _ = tx.send(ok); }))));
    let opened = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(10)))
        .await
        .map_err(|_| "Request failed.".to_string())?;
    Ok(opened.unwrap_or(false))
}

/// Settings → General → Pull to refresh (iPadOS; no-op on macOS). Sent by
/// the chrome at launch.
#[tauri::command]
pub fn set_pull_to_refresh(on: bool) {
    snorkle_content::set_pull_to_refresh(on);
    snorkle_content::dispatch_main(move || with_host(|h| h.set_pull_to_refresh(on)));
}

/// Gives the chrome's web view keyboard focus (after a shortcut, a page or
/// nothing may have it), so the input the chrome focuses next gets typing.
#[tauri::command]
pub fn focus_chrome() {
    snorkle_content::dispatch_main(|| with_host(|h| h.focus_chrome()));
}

/// The pointer entered (`on`) or left the notebook: keyboard focus follows
/// it there and back (`Host::hover_focus`; macOS). Whether the chrome has
/// focus now.
#[tauri::command]
pub async fn hover_focus(on: bool) -> Result<bool, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    snorkle_content::dispatch_main(move || {
        let mut got = false;
        with_host(|h| got = h.hover_focus(on));
        let _ = tx.send(got);
    });
    tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or(false))
        .await
        .map_err(|_| "Request failed.".to_string())
}

/// Page zoom from the ⋯ menu (`step`: in, out or reset). The core picks
/// the level; the content host applies it to the tab's web view.
#[tauri::command]
pub fn set_url_bar_attached(s: S<'_>, tab: u64, attached: bool) {
    with_browser(&s, |b| b.set_url_bar_attached(TabId(tab), attached));
}

#[tauri::command]
pub fn zoom_page(s: S<'_>, tab: u64, step: snorkle_core::tabs::ZoomStep) {
    with_browser(&s, |b| b.zoom(TabId(tab), step));
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

pub(super) fn shortcut_infos(s: &S<'_>) -> Vec<ShortcutInfo> {
    let current = s.shortcuts.lock().unwrap_or_else(|e| e.into_inner()).clone();
    snorkle_content::SHORTCUTS
        .iter()
        .zip(current)
        .filter(|(d, _)| d.available())
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

/// Summary view: scrolls a tab's page to the blocks a summary point was
/// drawn from and highlights them (channel C24; constant code taking only
/// these integers, `summary::scroll_js`). At most four, each below the
/// most blocks a page is read as.
#[tauri::command]
pub fn scroll_to_summary(tab: u64, blocks: Vec<u32>) {
    let blocks: Vec<u32> = blocks
        .into_iter()
        .filter(|b| (*b as usize) < snorkle_core::summary::MAX_BLOCKS)
        .take(snorkle_core::summary::MAX_POINT_BLOCKS)
        .collect();
    if blocks.is_empty() {
        return;
    }
    snorkle_content::dispatch_main(move || with_host(|h| h.scroll_to_summary(TabId(tab), &blocks)));
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
pub fn clear_website_data(s: S<'_>, kind: String) -> Result<(), String> {
    use snorkle_content::DataKind;
    // "Cache" also deletes the offline copies of pins and bookmarks.
    if kind == "cache" {
        if let Some(store) = s.store.as_ref() {
            store.clear_offline_copies();
        }
    }
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

pub(super) fn wall_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
