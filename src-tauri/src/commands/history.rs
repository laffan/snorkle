//! Bookmarks, launchers (pins' and bookmarks' tabs), history and saved page
//! text, reopening closed tabs.

use super::*;

#[derive(Serialize)]
pub struct Entry {
    url: String,
    title: String,
    display: DisplayUrl,
    /// Wall-clock milliseconds of the visit (history only).
    #[serde(skip_serializing_if = "Option::is_none")]
    visited_ms: Option<u64>,
    /// Size of the page's saved text (history only, when saved).
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_bytes: Option<u64>,
    /// Where a history search matched the page's saved text.
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet: Option<String>,
    /// Bookmarks only: the emoji shown instead of the favicon.
    #[serde(skip_serializing_if = "Option::is_none")]
    emoji: Option<String>,
    /// Bookmarks only: the bookmark group it is in (2026-11-24).
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
}

pub(super) fn entry(url: &str, title: &str) -> Option<Entry> {
    let u = Url::parse(url).ok()?;
    Some(Entry {
        url: url.to_string(),
        title: title.to_string(),
        display: url_display::display(&u),
        visited_ms: None,
        cache_bytes: None,
        snippet: None,
        emoji: None,
        group: None,
    })
}

#[tauri::command]
pub fn list_bookmarks(s: S<'_>) -> Vec<Entry> {
    s.store
        .as_ref()
        .map(|st| {
            st.bookmarks()
                .iter()
                .filter_map(|b| Some(Entry { emoji: st.pin_emoji(&b.url), group: b.group.clone(), ..entry(&b.url, &b.title)? }))
                .collect()
        })
        .unwrap_or_default()
}

/// ⌘D / the Bookmark row. On a bookmark's own tab: removes the bookmark and
/// the tab becomes an ordinary tab ("removed"). Otherwise bookmarks the
/// tab's committed URL and moves the tab into the bookmark, so opening the
/// bookmark returns to it ("added"; "already" if another tab already is
/// that bookmark's). On a pin's tab (2026-11-24; before, "pinned" and
/// nothing): bookmarks the pin's address and title, the pin keeps its tab
/// and the bookmark gets its own when opened ("from_pin"; "already" if it
/// is bookmarked). The URL comes from the core and the store, not the
/// caller; private tabs are never bookmarked.
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
        // A pin's tab: a bookmark of the pin (the same address, so opening
        // the bookmark comes back to this tab).
        if let Some(pin) = store.pins().into_iter().find(|p| p.url == key) {
            if store.is_bookmarked(&key) {
                return Ok("already");
            }
            let added = store.add_bookmark(&pin.url, &pin.title, wall_ms()).map_err(|e| e.to_string())?;
            return Ok(if added { "from_pin" } else { "none" });
        }
        if !store.is_bookmarked(&key) {
            return Ok("none");
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
    // Still a pin (a bookmark made from it): the tab stays the pin's.
    if !s.store.as_ref().is_some_and(|st| st.pins().iter().any(|p| p.url == url)) {
        with_browser(&s, |b| b.release_launcher(&url));
    }
    match s.store.clone() {
        Some(store) => store.remove_bookmark(&url).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

/// Opens a pin or bookmark in the tab that belongs to it (created on first
/// use; not listed with the other tabs).
#[tauri::command]
pub fn open_launcher(s: S<'_>, url: String, reset: Option<bool>) -> Result<u64, String> {
    let parsed = Url::parse(&url).map_err(|_| "Not a web address".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || url.len() > 4096 {
        return Err("Not a web address".into());
    }
    let (id, effects) = s.browser().open_launcher(&url, parsed, reset.unwrap_or(false), now_ms());
    s.run(effects);
    Ok(id.0)
}

#[tauri::command]
pub fn list_history(s: S<'_>, limit: usize) -> Vec<Entry> {
    let limit = limit.min(500);
    s.store
        .as_ref()
        .map(|st| {
            let sizes = st.page_text_sizes();
            st.history(limit)
                .iter()
                .filter_map(|h| {
                    entry(&h.url, &h.title).map(|e| Entry {
                        visited_ms: Some(h.visited_ms),
                        cache_bytes: sizes.get(&h.url).copied(),
                        ..e
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// History matching `query` (case-insensitive) in the title, the address
/// or the page's saved text, newest first, at most 500 visits. Text hits
/// carry a snippet.
#[tauri::command]
pub async fn search_history(s: S<'_>, query: String) -> Result<Vec<Entry>, String> {
    let Some(store) = s.store.clone() else { return Ok(Vec::new()) };
    let q: String = query.trim().chars().take(200).collect::<String>().to_lowercase();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let hits = tauri::async_runtime::spawn_blocking(move || {
        let texts: std::collections::BTreeMap<String, String> =
            store.search_page_texts(&q, snorkle_core::storage::MAX_PAGE_TEXTS).into_iter().map(|h| (h.url, h.snippet)).collect();
        let sizes = store.page_text_sizes();
        store
            .history(snorkle_core::storage::MAX_HISTORY)
            .into_iter()
            .filter_map(|h| {
                let snippet = texts.get(&h.url).cloned();
                let matched = snippet.is_some() || h.title.to_lowercase().contains(&q) || h.url.to_lowercase().contains(&q);
                if !matched {
                    return None;
                }
                entry(&h.url, &h.title).map(|e| Entry {
                    visited_ms: Some(h.visited_ms),
                    cache_bytes: sizes.get(&h.url).copied(),
                    snippet,
                    ..e
                })
            })
            .take(500)
            .collect::<Vec<Entry>>()
    })
    .await
    .map_err(|_| "Search failed.".to_string())?;
    Ok(hits)
}

#[derive(Deserialize)]
pub struct VisitArg {
    url: String,
    visited_ms: u64,
}

/// Removes visits from history (one, or a selection); pages left with no
/// visit lose their saved text. Returns how many were removed.
#[tauri::command]
pub async fn remove_history(s: S<'_>, visits: Vec<VisitArg>) -> Result<usize, String> {
    let Some(store) = s.store.clone() else { return Ok(0) };
    let visits: Vec<(String, u64)> = visits
        .into_iter()
        .take(snorkle_core::storage::MAX_HISTORY)
        .filter_map(|v| Some((snorkle_core::storage::storable_url(&v.url)?, v.visited_ms)))
        .collect();
    store.remove_history(&visits).map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct PageCopy {
    url: String,
    title: String,
    display: DisplayUrl,
    text: String,
    saved_ms: u64,
}

/// The saved text of a page in history ("cached version"), if any.
/// Page-derived (C17), cleaned on save and on read; the chrome shows it
/// with `textContent`.
#[tauri::command]
pub fn get_page_text(s: S<'_>, url: String) -> Option<PageCopy> {
    let p = s.store.as_ref()?.page_text(&url)?;
    let display = url_display::display(&Url::parse(&p.url).ok()?);
    Some(PageCopy { url: p.url, title: p.title, display, text: p.text, saved_ms: p.saved_ms })
}

/// ⌘⇧T: reopens the most recently closed tab whose page is still in
/// history (pages removed from history are skipped). False when there is
/// none.
#[tauri::command]
pub fn reopen_closed_tab(s: S<'_>) -> bool {
    let store = s.store.clone();
    loop {
        let Some(url) = s.browser().pop_closed() else { return false };
        if store.as_ref().is_some_and(|st| st.in_history(url.as_str())) {
            let (tab, effects) = s.browser().reopen_closed(url, now_ms());
            s.run(effects);
            return tab.is_some();
        }
    }
}

#[tauri::command]
pub async fn clear_history(s: S<'_>) -> Result<(), String> {
    match s.store.clone() {
        Some(store) => store.clear_history().map_err(|e| e.to_string()),
        None => Ok(()),
    }
}
