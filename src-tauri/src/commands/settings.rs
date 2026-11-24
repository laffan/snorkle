//! Settings → Blocked, tab favicons, passkeys.

use super::*;

// ---- Settings → Blocked ----------------------------------------------------

/// The block list for Settings → Blocked, with the passage the chrome
/// asks for before a pattern is changed or removed (kept in the core so
/// there is one copy).
#[derive(Serialize)]
pub struct BlockedState {
    patterns: Vec<String>,
    message: String,
    default_message: &'static str,
    passage: &'static str,
}

pub(super) fn blocked_state(list: &snorkle_core::blocklist::BlockList) -> BlockedState {
    BlockedState {
        patterns: list.patterns().to_vec(),
        message: list.settings().message.clone(),
        default_message: snorkle_core::blocklist::DEFAULT_MESSAGE,
        passage: snorkle_core::blocklist::UNBLOCK_PASSAGE,
    }
}

/// Applies `f` to a copy of the block list, stores it and makes it the
/// browser's (which hides open pages that are now blocked). The browser
/// stays locked throughout, so two changes cannot interleave.
pub(super) fn change_blocked<T>(
    s: &S<'_>,
    f: impl FnOnce(&mut snorkle_core::blocklist::BlockList) -> Result<T, snorkle_core::blocklist::BlockError>,
) -> Result<(T, BlockedState), String> {
    let Some(store) = s.store.clone() else { return Err("Storage unavailable".into()) };
    let (value, state, effects) = {
        let mut b = s.browser();
        let mut list = b.blocklist().clone();
        let value = f(&mut list).map_err(|e| e.to_string())?;
        store.set_blocked(list.settings()).map_err(|e| format!("Not saved: {e}"))?;
        let state = blocked_state(&list);
        let effects = b.set_blocklist(list);
        (value, state, effects)
    };
    s.run(effects);
    Ok((value, state))
}

#[tauri::command]
pub fn get_blocked(s: S<'_>) -> BlockedState {
    blocked_state(s.browser().blocklist())
}

/// Adds an address pattern (a regular expression; see
/// `snorkle_core::blocklist`). Adding needs nothing else.
#[tauri::command]
pub async fn add_blocked(s: S<'_>, pattern: String) -> Result<BlockedState, String> {
    if pattern.len() > 4 * snorkle_core::blocklist::MAX_PATTERN_LEN {
        return Err("That pattern is too long.".into());
    }
    change_blocked(&s, |l| l.add(&pattern)).map(|(_, st)| st)
}

/// "Block This Site" (⋯ menu): blocks the tab's site and its subdomains.
/// The site comes from the core's committed URL, not from the caller.
/// Returns the pattern added.
#[tauri::command]
pub async fn block_site(s: S<'_>, tab: u64) -> Result<String, String> {
    let url = s.browser().tab(TabId(tab)).and_then(|t| t.committed_url.clone()).ok_or("No page to block")?;
    let pattern = snorkle_core::blocklist::site_pattern(&url).ok_or("This address cannot be blocked by site.")?;
    change_blocked(&s, |l| l.add(&pattern)).map(|(p, _)| p)
}

/// Changes (`replacement`) or removes (`None`) a pattern. Refused unless
/// `passage` is the unblock passage, typed out in full.
#[tauri::command]
pub async fn edit_blocked(
    s: S<'_>,
    pattern: String,
    replacement: Option<String>,
    passage: String,
) -> Result<BlockedState, String> {
    if replacement.as_ref().is_some_and(|r| r.len() > 4 * snorkle_core::blocklist::MAX_PATTERN_LEN) {
        return Err("That pattern is too long.".into());
    }
    change_blocked(&s, |l| l.edit(&pattern, replacement.as_deref(), &passage)).map(|(_, st)| st)
}

/// The message shown in a blocked tab (empty: the default).
#[tauri::command]
pub async fn set_blocked_message(s: S<'_>, message: String) -> Result<BlockedState, String> {
    let message: String = message.chars().take(4 * snorkle_core::blocklist::MAX_MESSAGE_CHARS).collect();
    change_blocked(&s, |l| {
        l.set_message(&message);
        Ok(())
    })
    .map(|(_, st)| st)
}

// ---- tab favicons ----------------------------------------------------------

/// Favicons for the tab list and the bookmarks: which host each tab's page
/// and each bookmark is on, and the stored icons of those hosts the chrome
/// does not have yet (`have`).
#[derive(Serialize)]
pub struct TabIcons {
    /// Tab id → favicon host (tabs on an https page only).
    tabs: std::collections::HashMap<u64, String>,
    /// Bookmark URL → favicon host (https bookmarks only).
    bookmarks: std::collections::HashMap<String, String>,
    /// Host → `data:image/png;base64,…`.
    icons: std::collections::HashMap<String, String>,
    /// Host → the icon's copy for the dark appearance (2026-11-24), for
    /// icons too dark to see there (sent with the icon).
    dark: std::collections::HashMap<String, String>,
}

/// Hosts come from the core's committed URLs, not from the caller. Missing
/// icons are fetched (once per session) for non-private tabs only, so a
/// private visit is never fetched or written to disk; a private tab still
/// shows an icon already stored for its host.
#[tauri::command]
pub async fn tab_icons(s: S<'_>, have: Vec<String>) -> Result<TabIcons, String> {
    let have: std::collections::HashSet<String> = have.into_iter().take(1000).collect();
    let pages: Vec<(u64, String, bool)> = {
        let b = s.browser();
        b.tab_ids()
            .into_iter()
            .filter_map(|id| {
                let t = b.tab(id)?;
                let host = snorkle_core::storage::favicon_host(t.committed_url.as_ref()?.as_str())?;
                Some((id.0, host, t.is_private()))
            })
            .collect()
    };
    let mut out = TabIcons {
        tabs: Default::default(),
        bookmarks: Default::default(),
        icons: Default::default(),
        dark: Default::default(),
    };
    let Some(store) = s.store.clone() else { return Ok(out) };
    let want = |host: &String, private: bool, out: &mut TabIcons| {
        if have.contains(host) || out.icons.contains_key(host) {
            return;
        }
        match store.icon(host).and_then(|png| snorkle_core::storage::png_data_url(&png)) {
            Some(url) => {
                out.icons.insert(host.clone(), url);
                if let Some(dark) = store.icon_dark(host).and_then(|png| snorkle_core::storage::png_data_url(&png)) {
                    out.dark.insert(host.clone(), dark);
                }
            }
            None if !private => crate::pins::fetch_icon(&s, host.clone()),
            None => {}
        }
    };
    for (id, host, private) in pages {
        want(&host, private, &mut out);
        out.tabs.insert(id, host);
    }
    // Bookmarks are never private (private tabs cannot be bookmarked).
    for b in store.bookmarks() {
        if let Some(host) = snorkle_core::storage::favicon_host(&b.url) {
            want(&host, false, &mut out);
            out.bookmarks.insert(b.url, host);
        }
    }
    Ok(out)
}

/// Passkeys (Settings → General; owner request 2026-11-23): whether this
/// build is signed with Apple's passkey entitlement for browsers (macOS;
/// `None` on iPadOS, where the default-browser entitlement counts) and the
/// person's permission for Snorkle to use their passkeys.
#[derive(Serialize)]
pub struct PasskeyStatus {
    entitled: Option<bool>,
    permission: &'static str,
}

#[tauri::command]
pub async fn passkey_status() -> Result<PasskeyStatus, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    snorkle_content::dispatch_main(move || {
        use snorkle_content::passkeys;
        let _ = tx.send(PasskeyStatus { entitled: passkeys::entitled(), permission: passkeys::permission().name() });
    });
    tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(5)).ok())
        .await
        .ok()
        .flatten()
        .ok_or_else(|| "Passkey status unavailable.".to_string())
}

/// Asks the person, with the system's prompt, whether Snorkle may use their
/// passkeys; returns their answer (`PasskeyPermission::name`). Refused on
/// macOS in a build without the entitlement, where pages cannot use
/// passkeys whatever the answer.
#[tauri::command]
pub async fn request_passkeys() -> Result<&'static str, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    snorkle_content::dispatch_main(move || {
        use snorkle_content::passkeys;
        if passkeys::entitled() == Some(false) {
            let _ = tx.send(None);
            return;
        }
        passkeys::request_permission(Box::new(move |p| {
            let _ = tx.send(Some(p.name()));
        }));
    });
    tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(std::time::Duration::from_secs(600)).ok())
        .await
        .ok()
        .flatten()
        .ok_or_else(|| "No answer.".to_string())?
        .ok_or_else(|| "This build is not signed with Apple's passkey entitlement.".to_string())
}
