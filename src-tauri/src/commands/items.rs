//! Pins and editing pins and bookmarks.

use super::*;

#[derive(Serialize)]
pub struct PinEntry {
    url: String,
    title: String,
    display: DisplayUrl,
    /// `data:image/png;base64,…` made by the app from the sandboxed
    /// snapshot, or `None` (the chrome shows a letter tile).
    icon: Option<String>,
    /// Emoji chosen to show instead of the favicon.
    emoji: Option<String>,
    /// The favicon's copy for the dark appearance, if it needs one.
    icon_dark: Option<String>,
}

#[tauri::command]
pub fn list_pins(s: S<'_>) -> Vec<PinEntry> {
    let Some(store) = s.store.as_ref() else { return Vec::new() };
    store
        .pins()
        .into_iter()
        .filter_map(|p| {
            let display = url_display::display(&Url::parse(&p.url).ok()?);
            let host = snorkle_core::storage::favicon_host(&p.url);
            let icon = host.as_ref().and_then(|h| store.icon(h)).and_then(|png| snorkle_core::storage::png_data_url(&png));
            let icon_dark =
                host.as_ref().and_then(|h| store.icon_dark(h)).and_then(|png| snorkle_core::storage::png_data_url(&png));
            let emoji = store.pin_emoji(&p.url);
            Some(PinEntry { url: p.url, title: p.title, display, icon, emoji, icon_dark })
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
    crate::pins::sync(&s);
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
    crate::pins::sync(&s);
    Ok(added)
}

#[tauri::command]
pub async fn move_pin(s: S<'_>, url: String, index: usize) -> Result<(), String> {
    match s.store.clone() {
        Some(store) => store.move_pin(&url, index).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Edits a saved item's URL and optionally its title. `new_url` is what the
/// user typed, read like the address bar but only as an address (a search
/// is refused). Returns the stored URL.
#[tauri::command]
pub async fn edit_item_url(s: S<'_>, url: String, new_url: String, title: Option<String>) -> Result<String, String> {
    let target = match snorkle_core::input::interpret(&new_url, &snorkle_core::input::DEFAULT_SEARCH) {
        Ok(snorkle_core::input::Interpretation::Url(u)) if matches!(u.scheme(), "http" | "https") => u,
        _ => return Err("Enter a web address (https://…).".into()),
    };
    let Some(store) = s.store.clone() else { return Err("Storage is unavailable.".into()) };
    let stored = store
        .edit_item(&url, target.as_str(), title.as_deref())
        .map_err(|e| e.to_string())?
        .ok_or("That address is already pinned or bookmarked, or the item is gone.")?;
    with_browser(&s, |b| b.rekey_launcher(&url, &stored));
    crate::pins::sync(&s);
    Ok(stored)
}

/// Sets (or with `None` removes) the emoji a pin or bookmark shows instead
/// of its favicon (the Edit dialog; validated by `storage::clean_emoji`).
#[tauri::command]
pub async fn set_pin_emoji(s: S<'_>, url: String, emoji: Option<String>) -> Result<bool, String> {
    let Some(store) = s.store.clone() else { return Ok(false) };
    store.set_pin_emoji(&url, emoji.as_deref()).map_err(|e| e.to_string())
}

/// Removes a pin; with `bookmark_index`, moves it into the bookmarks bar
/// at that position instead of dropping it.
#[tauri::command]
pub async fn unpin(s: S<'_>, url: String, bookmark_index: Option<usize>) -> Result<(), String> {
    // Moved into the bookmarks, it keeps its tab; removed, the tab is listed
    // again (unless the address is also bookmarked: then it is that
    // bookmark's tab).
    if bookmark_index.is_none() && !s.store.as_ref().is_some_and(|st| st.is_bookmarked(&url)) {
        with_browser(&s, |b| b.release_launcher(&url));
    }
    let Some(store) = s.store.clone() else { return Ok(()) };
    if let (Some(p), Some(i)) = (store.remove_pin(&url).map_err(|e| e.to_string())?, bookmark_index) {
        store.insert_bookmark(&p.url, &p.title, i, p.created_ms).map_err(|e| e.to_string())?;
    }
    crate::pins::sync(&s);
    store.prune_item_emoji().map_err(|e| e.to_string())
}
