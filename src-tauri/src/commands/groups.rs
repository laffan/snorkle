//! Research sessions, tab groups and bookmark groups (owner request
//! 2026-11-24). Tab ids that do not exist are ignored; names are cleaned
//! by the core; URLs come from the store or the core, except `send_page`'s,
//! which must be a pin or a bookmark.

use super::*;

/// Most tabs or bookmarks one call acts on.
const MAX_IDS: usize = 500;

fn tab_ids(tabs: Vec<u64>) -> Vec<TabId> {
    tabs.into_iter().take(MAX_IDS).map(TabId).collect()
}

fn space_arg(space: Option<String>) -> Result<Option<String>, String> {
    match space {
        Some(id) if !snorkle_core::browser::valid_research_id(&id) => Err("That research session is gone.".into()),
        other => Ok(other),
    }
}

// ---- research sessions -------------------------------------------------------

/// Menu → New Research Session: a new session about `name`, shown at once
/// with an empty tab. Returns its id.
#[tauri::command]
pub fn new_research(s: S<'_>, name: String) -> Result<String, String> {
    let (id, effects) = s.browser().new_research(&name, wall_ms(), now_ms())?;
    s.run(effects);
    Ok(id)
}

/// Shows everyday browsing (`None`) or a research session.
#[tauri::command]
pub fn enter_research(s: S<'_>, space: Option<String>) -> Result<(), String> {
    let space = space_arg(space)?;
    with_browser(&s, |b| b.enter_space(space.as_deref(), now_ms()));
    Ok(())
}

#[tauri::command]
pub fn rename_research(s: S<'_>, id: String, name: String) {
    with_browser(&s, |b| b.rename_research(&id, &name));
}

/// Deletes a research session: its tabs are closed and its notebook
/// deleted.
#[tauri::command]
pub async fn delete_research(s: S<'_>, id: String) -> Result<(), String> {
    if !snorkle_core::browser::valid_research_id(&id) {
        return Ok(());
    }
    with_browser(&s, |b| b.delete_research(&id, now_ms()));
    if let Some(store) = s.store.clone() {
        store.delete_notebook_in(&id).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Sends tabs to everyday browsing (`to: None`) or a research session.
#[tauri::command]
pub fn send_tabs(s: S<'_>, tabs: Vec<u64>, to: Option<String>) -> Result<(), String> {
    let to = space_arg(to)?;
    with_browser(&s, |b| b.send_tabs(&tab_ids(tabs), to.as_deref(), now_ms()));
    Ok(())
}

/// Sends a copy of a pin's or bookmark's page (`url`, as stored) to
/// everyday browsing or a research session: a new tab there, loaded when
/// it is chosen. Returns whether one was made.
#[tauri::command]
pub fn send_page(s: S<'_>, url: String, to: Option<String>) -> Result<bool, String> {
    let to = space_arg(to)?;
    let Some(store) = s.store.clone() else { return Ok(false) };
    let item = store.pins().into_iter().chain(store.bookmarks()).find(|b| b.url == url);
    let Some(item) = item else { return Err("That pin or bookmark is gone.".into()) };
    let page = Url::parse(&item.url).map_err(|_| "Not a web address".to_string())?;
    let (tab, effects) = s.browser().send_page(page, &item.title, to.as_deref(), now_ms());
    s.run(effects);
    Ok(tab.is_some())
}

// ---- tab groups ----------------------------------------------------------------

/// Groups the tabs (those that can be: listed, of the first one's profile
/// and session) under `name`. Returns the group, or `None`.
#[tauri::command]
pub fn group_tabs(s: S<'_>, tabs: Vec<u64>, name: String) -> Option<u64> {
    let (group, effects) = s.browser().group_tabs(&tab_ids(tabs), &name)?;
    s.run(effects);
    Some(group)
}

/// Takes the tabs out of their groups.
#[tauri::command]
pub fn ungroup_tabs(s: S<'_>, tabs: Vec<u64>) {
    with_browser(&s, |b| b.ungroup_tabs(&tab_ids(tabs)));
}

/// Renames, folds or unfolds a tab group, or ungroups it (its tabs stay).
#[tauri::command]
pub fn edit_tab_group(s: S<'_>, group: u64, name: Option<String>, collapsed: Option<bool>, ungroup: Option<bool>) {
    with_browser(&s, |b| {
        if ungroup == Some(true) {
            return b.ungroup(group);
        }
        let mut out = Vec::new();
        if let Some(n) = name {
            out.extend(b.rename_group(group, &n));
        }
        if let Some(c) = collapsed {
            out.extend(b.set_group_collapsed(group, c));
        }
        out
    });
}

/// Closes several tabs at once (a group, a selection).
#[tauri::command]
pub fn close_tabs(s: S<'_>, tabs: Vec<u64>) {
    with_browser(&s, |b| {
        let mut out = Vec::new();
        for t in tab_ids(tabs) {
            out.extend(b.close(t, now_ms()));
        }
        out
    });
}

/// A tab group goes into the bookmarks bar (its tab menu's Move to
/// Bookmarks): each tab with a page is bookmarked at the end of the bar
/// and becomes that bookmark's tab, all in one new bookmark group of the
/// same name; the tab group ends. Private tabs are never bookmarked.
/// Returns the bookmark group's id, or `None` when nothing was bookmarked.
#[tauri::command]
pub async fn bookmark_tab_group(s: S<'_>, group: u64) -> Result<Option<String>, String> {
    let Some(store) = s.store.clone() else { return Err("Storage unavailable".into()) };
    let (name, pages): (String, Vec<(TabId, String, String)>) = {
        let b = s.browser();
        let Some(g) = b.groups().iter().find(|g| g.id == group) else { return Ok(None) };
        let pages = b
            .group_members(group)
            .into_iter()
            .filter_map(|id| {
                let t = b.tab(id).filter(|t| !t.is_private())?;
                let url = t.committed_url.as_ref().filter(|u| matches!(u.scheme(), "http" | "https"))?;
                Some((id, url.to_string(), t.title.clone()))
            })
            .collect();
        (g.name.clone(), pages)
    };
    let mut urls = Vec::new();
    for (id, url, title) in pages {
        let added = store.add_bookmark(&url, &title, wall_ms()).map_err(|e| e.to_string())?;
        if added {
            let (_, effects) = s.browser().bind_launcher(id, &url);
            s.run(effects);
            urls.push(url);
        }
    }
    if urls.is_empty() {
        return Ok(None);
    }
    with_browser(&s, |b| b.ungroup(group));
    store.group_bookmarks(&urls, &name, wall_ms()).map_err(|e| e.to_string())
}

// ---- bookmark groups -------------------------------------------------------------

#[tauri::command]
pub fn list_bookmark_groups(s: S<'_>) -> Vec<snorkle_core::storage::BookmarkGroup> {
    s.store.as_ref().map(|st| st.bookmark_groups()).unwrap_or_default()
}

/// Groups the bookmarks of `urls` under `name`. Returns the group's id.
#[tauri::command]
pub async fn group_bookmarks(s: S<'_>, urls: Vec<String>, name: String) -> Result<Option<String>, String> {
    let Some(store) = s.store.clone() else { return Ok(None) };
    let urls: Vec<String> = urls.into_iter().take(MAX_IDS).collect();
    store.group_bookmarks(&urls, &name, wall_ms()).map_err(|e| e.to_string())
}

/// Renames, folds or unfolds a bookmark group, or ungroups it (its
/// bookmarks stay).
#[tauri::command]
pub async fn edit_bookmark_group(
    s: S<'_>,
    id: String,
    name: Option<String>,
    collapsed: Option<bool>,
    ungroup: Option<bool>,
) -> Result<(), String> {
    let Some(store) = s.store.clone() else { return Ok(()) };
    if !snorkle_core::storage::valid_bookmark_group_id(&id) {
        return Ok(());
    }
    if ungroup == Some(true) {
        return store.ungroup_bookmarks(&id).map(|_| ()).map_err(|e| e.to_string());
    }
    if let Some(n) = name {
        store.rename_bookmark_group(&id, &n).map_err(|e| e.to_string())?;
    }
    if let Some(c) = collapsed {
        store.set_bookmark_group_collapsed(&id, c).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Removes several bookmarks (a selection, a group); their tabs become
/// ordinary tabs again, unless the address is also a pin.
#[tauri::command]
pub async fn remove_bookmarks(s: S<'_>, urls: Vec<String>) -> Result<(), String> {
    let Some(store) = s.store.clone() else { return Ok(()) };
    let urls: Vec<String> = urls.into_iter().take(MAX_IDS).collect();
    let pins: Vec<String> = store.pins().into_iter().map(|p| p.url).collect();
    for url in store.remove_bookmarks(&urls).map_err(|e| e.to_string())? {
        if !pins.contains(&url) {
            with_browser(&s, |b| b.release_launcher(&url));
        }
    }
    Ok(())
}
