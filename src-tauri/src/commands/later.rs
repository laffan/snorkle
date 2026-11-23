//! Save for Later: saving tabs and history pages, conversation capture,
//! saved copies and adding pages to them.

use super::*;

/// A page saved for later, as the chrome lists it.
#[derive(Serialize)]
pub struct LaterView {
    id: String,
    url: String,
    title: String,
    host: String,
    saved_ms: u64,
    /// Pages its archive holds (0: no archive; the page opens live).
    pages: usize,
}

/// Save for Later (`saved::save`): an archive that works offline (channel
/// C22), the tab's committed page
/// (http/https, from the core) and title; then the tab closes. Progress
/// goes to the chrome as `later_progress`. Not for private tabs, whose
/// pages are never written, or a saved copy's own tab. A tab without a live
/// page is saved without an archive.
#[tauri::command]
pub async fn save_for_later(s: S<'_>, tab: u64, captured: Option<bool>) -> Result<(), String> {
    let id = TabId(tab);
    let captured = captured.unwrap_or(false);
    let (url, title, live) = {
        let b = s.browser();
        let t = b.tab(id).ok_or("That tab is gone.")?;
        if t.is_private() || t.saved.is_some() {
            return Err("This tab can't be saved.".into());
        }
        let url = t.committed_url.clone().filter(|u| matches!(u.scheme(), "http" | "https")).ok_or("Open a web page first.")?;
        let live = t.life == snorkle_core::tabs::Life::Live && t.notice.is_none() && !b.shows_offline_copy(id);
        (url, t.title.clone(), live)
    };
    let shared = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || crate::saved::save(&shared, id, &url, &title, live, captured))
        .await
        .map_err(|_| "Request failed.".to_string())?
}

/// Save for Later on a history row (owner request 2026-11-23): the page
/// at `url` (http/https without credentials, as history stores it) is
/// loaded in a hidden tab of the default profile, through the navigation
/// policy, and saved there (`saved::save_url`; the tab then closes).
/// Refused when that page is already saved for later.
#[tauri::command]
pub async fn save_page_for_later(s: S<'_>, url: String) -> Result<(), String> {
    let store = s.store.clone().ok_or("Saving is not available.")?;
    let url = snorkle_core::storage::storable_url(&url)
        .and_then(|u| Url::parse(&u).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or("Not a web address.")?;
    if store.later().list().iter().any(|it| it.url == url.as_str()) {
        return Err("Already saved for later.".into());
    }
    let shared = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || crate::saved::save_url(&shared, url))
        .await
        .map_err(|_| "Request failed.".to_string())?
}

/// Save for Later on an AI conversation (chatgpt.com, claude.ai,
/// gemini.google.com; owner request 2026-11-16) begins a capture mode: the
/// reader scrolls through the thread (earlier turns load as they scroll up)
/// while every turn is noted, then saves (`save_for_later` with
/// `captured`). `None`: not such a page (or private, or not loaded); the
/// chrome saves it at once instead.
#[tauri::command]
pub async fn begin_conversation_capture(s: S<'_>, tab: u64) -> Result<Option<u32>, String> {
    let id = TabId(tab);
    {
        let b = s.browser();
        let Some(t) = b.tab(id) else { return Ok(None) };
        let site = t.committed_url.as_ref().and_then(snorkle_core::conversation::site_of);
        if t.is_private() || t.saved.is_some() || site.is_none() || t.life != snorkle_core::tabs::Life::Live || t.notice.is_some() {
            return Ok(None);
        }
    }
    tauri::async_runtime::spawn_blocking(move || crate::saved::begin_capture(id))
        .await
        .map_err(|_| "Request failed.".to_string())
}

/// How many turns the capture holds (`None`: none running).
#[tauri::command]
pub async fn conversation_capture_count(tab: u64) -> Result<Option<u32>, String> {
    tauri::async_runtime::spawn_blocking(move || crate::saved::capture_count(TabId(tab)))
        .await
        .map_err(|_| "Request failed.".to_string())
}

/// Ends the capture without saving.
#[tauri::command]
pub async fn cancel_conversation_capture(tab: u64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || crate::saved::cancel_capture(TabId(tab)))
        .await
        .map_err(|_| "Request failed.".to_string())
}

#[tauri::command]
pub fn list_later(s: S<'_>) -> Vec<LaterView> {
    let Some(store) = s.store.as_ref() else { return Vec::new() };
    let later = store.later();
    later
        .list()
        .into_iter()
        .map(|it| LaterView {
            host: Url::parse(&it.url).ok().map(|u| url_display::display(&u).registrable).unwrap_or_default(),
            pages: it.pages.len(),
            id: it.id,
            url: it.url,
            title: it.title,
            saved_ms: it.saved_ms,
        })
        .collect()
}

/// Opens a saved page's archive (offline) in a new tab; it stays in the
/// list.
#[tauri::command]
pub async fn open_later(s: S<'_>, id: String) -> Result<(), String> {
    let shared = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || crate::saved::show(&shared, None, &id, None, false).map(|_| ()))
        .await
        .map_err(|_| "Request failed.".to_string())?
}

/// "+" on a saved page: opens its archive in a new tab, with scripts off,
/// where tapping links chooses pages to add (`finish_adding_pages`).
#[tauri::command]
pub async fn add_later_pages(s: S<'_>, id: String) -> Result<(), String> {
    let shared = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || crate::saved::show(&shared, None, &id, None, true).map(|_| ()))
        .await
        .map_err(|_| "Request failed.".to_string())?
}

/// Adds the pages chosen in `tab` to its saved item: each is loaded live in
/// the tab and captured, then the saved copy is shown again. Returns how
/// many pages were added.
#[tauri::command]
pub async fn finish_adding_pages(s: S<'_>, tab: u64) -> Result<usize, String> {
    let tab = TabId(tab);
    let (item, links) = s.browser().take_picks(tab).ok_or("Nothing is being added.")?;
    let shared = s.inner().clone();
    let effects = shared.browser().set_picking(tab, false);
    shared.run(effects);
    tauri::async_runtime::spawn_blocking(move || {
        if links.is_empty() {
            crate::saved::show(&shared, Some(tab), &item, None, false).map(|_| 0)
        } else {
            crate::saved::add_pages(&shared, tab, &item, links)
        }
    })
    .await
    .map_err(|_| "Request failed.".to_string())?
}

/// Stops choosing pages to add and shows the saved copy again.
#[tauri::command]
pub async fn cancel_adding_pages(s: S<'_>, tab: u64) -> Result<(), String> {
    let tab = TabId(tab);
    let (item, page) = {
        let mut b = s.browser();
        let page = b.saved_copy(tab).map(|c| c.page.to_string());
        let Some((item, _)) = b.take_picks(tab) else { return Ok(()) };
        (item, page)
    };
    let shared = s.inner().clone();
    tauri::async_runtime::spawn_blocking(move || crate::saved::show(&shared, Some(tab), &item, page.as_deref(), false).map(|_| ()))
        .await
        .map_err(|_| "Request failed.".to_string())?
}

/// Opens a saved page live in a new tab (and removes it from the list).
#[tauri::command]
pub fn restore_later(s: S<'_>, id: String) -> Result<(), String> {
    let store = s.store.clone().ok_or("Saving is not available.")?;
    let Some(item) = store.later().take(&id).map_err(|e| e.to_string())? else { return Ok(()) };
    with_browser(&s, |b| b.close_saved(&id, now_ms()));
    let url = Url::parse(&item.url).map_err(|e| e.to_string())?;
    let (tab, effects) = s.browser().new_tab(Profile::Default, now_ms());
    s.run(effects);
    let effects = s.browser().navigate_trusted(tab, url, now_ms());
    s.run(effects);
    Ok(())
}

/// Bookmarks a saved page (at the end of the bookmarks) and removes it
/// from the list.
#[tauri::command]
pub fn bookmark_later(s: S<'_>, id: String) -> Result<(), String> {
    let store = s.store.clone().ok_or("Saving is not available.")?;
    let Some(item) = store.later().take(&id).map_err(|e| e.to_string())? else { return Ok(()) };
    with_browser(&s, |b| b.close_saved(&id, now_ms()));
    if !store.is_bookmarked(&item.url) {
        store.add_bookmark(&item.url, &item.title, wall_ms()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn delete_later(s: S<'_>, id: String) -> Result<(), String> {
    with_browser(&s, |b| b.close_saved(&id, now_ms()));
    match s.store.as_ref() {
        Some(store) => store.later().take(&id).map(|_| ()).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}
