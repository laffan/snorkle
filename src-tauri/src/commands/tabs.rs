//! Tabs and navigation: state, typed addresses, closing and activating tabs,
//! back/forward/reload, notices, and the chrome's layout (holes and insets).

use super::*;

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
/// is) with a Google search for `query`; the new-tab page's Search row
/// passes its own tab instead. The URL is built here, so the chrome holds
/// no remote addresses.
#[tauri::command]
pub fn search_google(s: S<'_>, query: String, private: bool, tab: Option<u64>) -> Result<u64, String> {
    let q = snorkle_core::text::sanitize_page_text(&query, 300);
    if q.is_empty() {
        return Err("Nothing to search for.".into());
    }
    let url = Url::parse_with_params("https://www.google.com/search", [("q", q.as_str())]).map_err(|e| e.to_string())?;
    // `tab`: search in that tab (the new-tab page), keeping its profile.
    let existing = tab.map(TabId).filter(|t| s.browser().tab(*t).is_some());
    let id = match existing {
        Some(id) => id,
        None => {
            let profile = if private { Profile::Private } else { Profile::Default };
            let (id, effects) = s.browser().new_tab(profile, now_ms());
            s.run(effects);
            id
        }
    };
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
    with_browser(&s, |b| b.go_back(TabId(tab), now_ms()));
}

#[tauri::command]
pub fn go_forward(s: S<'_>, tab: u64) {
    with_browser(&s, |b| b.go_forward(TabId(tab)));
}

#[tauri::command]
pub fn reload(s: S<'_>, tab: u64) {
    with_browser(&s, |b| b.reload_by_user(TabId(tab), now_ms()));
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
