//! Snorkle app entry point: Tauri hosts the trusted chrome UI (bundled
//! `ui/`, webview label `main`); `snorkle-content` hosts untrusted pages in
//! separate WKWebViews; `snorkle-core` makes every decision.
//! See docs/ARCHITECTURE.md.

mod adblock;
mod ai;
mod command_list;
pub mod commands;
mod pins;
mod scripts;
pub mod state;

use std::sync::Arc;
use std::time::Duration;

use snorkle_core::browser::Browser;
use snorkle_core::os::{self, OsSupport};
use snorkle_core::storage::Store;
use snorkle_core::tabs::MemoryBudget;
use tauri::{Emitter as _, Manager, Runtime};

pub use command_list::COMMANDS;
pub use state::{ChromeEvent, Emitter, Shared};

/// Label of the only Tauri webview: the browser chrome.
pub const CHROME_LABEL: &str = "main";

/// Registers the app's command handlers. Kept separate from `run` so the
/// IPC isolation tests exercise exactly this handler set.
pub fn register<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.invoke_handler(tauri::generate_handler![
        commands::get_state,
        commands::navigate,
        commands::new_tab,
        commands::close_tab,
        commands::activate_tab,
        commands::go_back,
        commands::go_forward,
        commands::reload,
        commands::stop,
        commands::proceed_http,
        commands::confirm_external,
        commands::dismiss_notice,
        commands::set_chrome_layout,
        commands::list_bookmarks,
        commands::toggle_bookmark,
        commands::remove_bookmark,
        commands::list_history,
        commands::clear_history,
        commands::remove_history,
        commands::search_history,
        commands::get_page_text,
        commands::reopen_closed_tab,
        commands::list_downloads,
        commands::cancel_download,
        commands::delete_download,
        commands::clear_downloads,
        commands::export_download,
        commands::share_download,
        commands::answer_download_prompt,
        commands::toggle_site_adblock,
        commands::bookmark_tab,
        commands::move_bookmark,
        commands::split_tabs,
        commands::unsplit,
        commands::split_new,
        commands::set_split_ratio,
        commands::get_view_mode,
        commands::set_view_mode,
        commands::set_launch_mode,
        commands::set_tab_view_exempt,
        commands::set_tab_headers,
        commands::get_headings,
        commands::scroll_to_heading,
        commands::find_in_page,
        commands::end_find,
        commands::focus_chrome,
        commands::set_appearance,
        commands::share_page,
        commands::image_action,
        commands::zoom_page,
        commands::set_url_bar_attached,
        commands::read_selection,
        commands::define_term,
        commands::search_google,
        commands::get_shortcuts,
        commands::set_shortcuts,
        commands::clear_website_data,
        ai::ai_status,
        ai::ai_set_key,
        ai::ai_delete_key,
        ai::ai_test,
        ai::ai_write_script,
        ai::detect_headings,
        ai::ai_chat,
        ai::ai_efforts,
        ai::list_chats,
        ai::get_chat,
        ai::delete_chat,
        ai::ai_usage,
        ai::ai_reset_usage,
        commands::open_launcher,
        commands::get_console,
        commands::set_console_capture,
        commands::clear_console,
        commands::list_user_scripts,
        commands::save_user_script,
        commands::delete_user_script,
        commands::list_pins,
        commands::pin_tab,
        commands::pin_bookmark,
        commands::move_pin,
        commands::unpin,
        commands::set_pin_emoji,
        commands::edit_item_url,
        commands::get_blocked,
        commands::add_blocked,
        commands::block_site,
        commands::edit_blocked,
        commands::set_blocked_message,
        commands::tab_icons,
        commands::set_swipe_navigation,
        commands::set_pull_to_refresh,
    ])
}

/// Builds the core with the device's memory budget and OS policy.
pub fn make_browser(os_support: OsSupport, physical_memory: u64) -> Browser {
    Browser::new(MemoryBudget::for_physical_memory(physical_memory), os_support)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    register(tauri::Builder::default())
        .setup(|app| {
            setup(app)?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building snorkle")
        .run(on_run_event);
}

/// Links opened from other apps before setup finished (cold launch).
static PENDING_LINKS: std::sync::Mutex<Vec<snorkle_core::Url>> = std::sync::Mutex::new(Vec::new());

#[allow(unused_variables)]
fn on_run_event<R: Runtime>(app: &tauri::AppHandle<R>, event: tauri::RunEvent) {
    // Quitting (⌘Q on macOS): the session waits up to 400 ms to be written;
    // write it now.
    if let tauri::RunEvent::Exit = event {
        if let Some(shared) = app.try_state::<Arc<Shared>>() {
            shared.flush_blocking(Duration::from_secs(2));
        }
        return;
    }
    // Snorkle as the default browser: iOS and macOS send links opened in
    // other apps.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    if let tauri::RunEvent::Opened { urls } = event {
        match app.try_state::<Arc<Shared>>() {
            Some(shared) => open_links(&shared, urls),
            None => PENDING_LINKS.lock().unwrap_or_else(|e| e.into_inner()).extend(urls),
        }
    }
}

/// Opens links from other apps in new tabs (http/https only, policy-checked).
fn open_links(shared: &Arc<Shared>, urls: Vec<snorkle_core::Url>) {
    for url in urls {
        let shown = url.to_string();
        let (tab, effects) = shared.browser().open_from_other_app(url, state::now_ms());
        let result = if tab.is_some() { "opened" } else { "refused" };
        shared.log(snorkle_core::console::Level::Info, tab, &format!("link from another app {shown}: {result}"));
        shared.run(effects);
    }
}

fn setup<R: Runtime>(app: &mut tauri::App<R>) -> Result<(), Box<dyn std::error::Error>> {
    // iPadOS and macOS have a content host; other desktop builds run the
    // chrome alone. On macOS the web engine's version is Safari's.
    let os_support = if cfg!(target_os = "ios") {
        os::check(snorkle_content::os_version())
    } else if cfg!(target_os = "macos") {
        os::check_macos_safari(snorkle_content::webkit_version())
    } else {
        OsSupport::PreviewOnly
    };
    let browser = make_browser(os_support, snorkle_content::physical_memory());

    let store = app.path().app_data_dir().ok().and_then(|d| Store::open(d.join("profile-default")).ok());
    let session = store.as_ref().map(|s| s.load_session()).unwrap_or_default();
    // Downloads are kept out of device backups (owner decision 2026-11-04).
    if let Some(root) = store.as_ref().and_then(|s| s.downloads_root().ok()) {
        snorkle_content::exclude_from_backup(&root);
    }

    let handle = app.handle().clone();
    let emit: Emitter = Box::new(move |ev| {
        let _ = match ev {
            ChromeEvent::State(s) => handle.emit_to(CHROME_LABEL, "state", s),
            ChromeEvent::Toast(m) => handle.emit_to(CHROME_LABEL, "toast", m),
            ChromeEvent::Shortcut(name) => handle.emit_to(CHROME_LABEL, "shortcut", name),
            ChromeEvent::PinsChanged => handle.emit_to(CHROME_LABEL, "pins", ()),
            ChromeEvent::IconsChanged => handle.emit_to(CHROME_LABEL, "icons", ()),
            ChromeEvent::Console(entry) => handle.emit_to(CHROME_LABEL, "console", entry),
            ChromeEvent::ConsoleReset => handle.emit_to(CHROME_LABEL, "console_reset", ()),
            ChromeEvent::Downloads(list) => handle.emit_to(CHROME_LABEL, "downloads", list),
            ChromeEvent::DownloadPrompt { ask, tab, from } => handle.emit_to(
                CHROME_LABEL,
                "download_prompt",
                serde_json::json!({ "ask": ask, "tab": tab, "from": from }),
            ),
            ChromeEvent::HeadingCurrent(tab, index) => {
                handle.emit_to(CHROME_LABEL, "heading_current", serde_json::json!({ "tab": tab, "index": index }))
            }
            ChromeEvent::Headings(tab, list) => {
                handle.emit_to(CHROME_LABEL, "headings", serde_json::json!({ "tab": tab, "headings": list }))
            }
            ChromeEvent::Selection(tab, rect) => {
                handle.emit_to(CHROME_LABEL, "selection", serde_json::json!({ "tab": tab, "rect": rect }))
            }
            ChromeEvent::EdgePointer(at) => handle.emit_to(
                CHROME_LABEL,
                "edge_pointer",
                at.map(|(x, y)| serde_json::json!({ "x": x, "y": y })).unwrap_or(serde_json::Value::Null),
            ),
            ChromeEvent::ImageMenu(tab, menu) => {
                let (url, rect) = menu.map_or((None, None), |(u, r)| (Some(u), Some(r)));
                handle.emit_to(CHROME_LABEL, "image_menu", serde_json::json!({ "tab": tab, "url": url, "rect": rect }))
            }
        };
    });
    let allowlist = store.as_ref().map(|s| s.adblock_allowed()).unwrap_or_default();
    let blocked = store.as_ref().map(|s| s.blocked()).unwrap_or_default();
    let shared = Shared::new(browser, store, emit);
    let _ = shared.browser().set_adblock_allowlist(allowlist);
    // Before the session is restored, so no blocked page is ever shown.
    let _ = shared.browser().set_blocklist(snorkle_core::blocklist::BlockList::from_settings(blocked));
    app.manage(shared.clone());

    let window = app.get_webview_window(CHROME_LABEL).ok_or("main window missing")?;
    attach_content_host(&window, shared.clone())?;
    // The chrome's own WKWebView, so the host can give it keyboard focus.
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    window.with_webview(|w| {
        let view = w.inner();
        // SAFETY: Tauri runs this on the main thread; `view` is the chrome's
        // WKWebView, alive as long as the window (the host retains it).
        state::with_host(|h| unsafe { h.set_chrome_view(view) });
    })?;
    // Debug desktop preview builds: open the chrome's Web Inspector. On iOS
    // and macOS use Safari > Develop instead (both the chrome and the pages
    // are inspectable in debug builds); on macOS a docked inspector would
    // sit under the page views.
    #[cfg(all(debug_assertions, desktop, not(target_os = "macos")))]
    window.open_devtools();

    pins::refresh_icons(&shared);
    // Injected scripts first, so restored views are created with them.
    let capture = shared.capturing();
    snorkle_content::dispatch_main(move || state::with_host(|h| h.set_console_capture(capture)));
    // Each launch greets the user with a new tab (the chrome's start page)
    // after the restored, unloaded tabs; links that opened the app (cold
    // launch from another app) are shown instead. Returning from the
    // background is not a launch: nothing is added then.
    let pending = std::mem::take(&mut *PENDING_LINKS.lock().unwrap_or_else(|e| e.into_inner()));
    let effects = if pending.is_empty() {
        shared.browser().restore_session_with_start_tab(&session, state::now_ms()).1
    } else {
        shared.browser().restore_session(&session, state::now_ms())
    };
    // Per-tab view-mode exemptions come back with the session; scripts are
    // set (with them) before the restored views are created.
    let exemptions = shared.browser().view_exemptions();
    let restored: Vec<String> = exemptions.iter().map(|(t, k)| format!("tab {} {k:?}", t.0)).collect();
    for (tab, kinds) in exemptions {
        shared.exempt().insert(tab, kinds.into_iter().collect());
    }
    let launch = shared.view_modes();
    shared.log(
        snorkle_core::console::Level::Info,
        None,
        &format!(
            "session: {} tabs restored; view modes {launch:?}; off per tab: {}",
            session.tabs.len(),
            if restored.is_empty() { "none".to_string() } else { restored.join(", ") }
        ),
    );
    scripts::apply(&shared, false);
    shared.run(effects);
    open_links(&shared, pending);

    // Content blocking: reuses WebKit's compiled lists when unchanged,
    // otherwise converts off the main thread.
    if os_support.allows_browsing() {
        adblock::rebuild(&shared, None);
    }

    // Background-tab TTL discards.
    let ticker = shared.clone();
    std::thread::Builder::new().name("snorkle-tick".into()).spawn(move || loop {
        std::thread::sleep(Duration::from_secs(30));
        let effects = ticker.browser().tick(state::now_ms());
        ticker.run(effects);
    })?;
    Ok(())
}

#[cfg(target_os = "ios")]
fn attach_content_host<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    shared: Arc<Shared>,
) -> Result<(), Box<dyn std::error::Error>> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle()?;
    match handle.as_raw() {
        // SAFETY: `ui_view` is the root UIView of the main window, valid for
        // the window's lifetime; `setup` runs on the main thread.
        RawWindowHandle::UiKit(h) => unsafe { state::install_host(h.ui_view.as_ptr(), shared) },
        _ => return Err("unexpected window handle".into()),
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn attach_content_host<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    shared: Arc<Shared>,
) -> Result<(), Box<dyn std::error::Error>> {
    let ns_window = window.ns_window()?;
    if ns_window.is_null() {
        return Err("main window has no NSWindow".into());
    }
    // SAFETY: the NSWindow of the main window, valid for the window's
    // lifetime, whose content view holds the chrome's web view; `setup`
    // runs on the main thread.
    unsafe { state::install_host(ns_window, shared) };
    Ok(())
}

#[cfg(not(any(target_os = "ios", target_os = "macos")))]
fn attach_content_host<R: Runtime>(
    _window: &tauri::WebviewWindow<R>,
    shared: Arc<Shared>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Development stand-in: no web content is rendered off iOS and macOS.
    unsafe { state::install_host(std::ptr::null_mut(), shared) };
    Ok(())
}
