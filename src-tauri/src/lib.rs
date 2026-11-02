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
        commands::toggle_site_adblock,
        commands::bookmark_tab,
        commands::move_bookmark,
        commands::split_tabs,
        commands::unsplit,
        commands::get_view_mode,
        commands::set_view_mode,
        commands::set_launch_mode,
        commands::set_tab_view_exempt,
        commands::set_tab_headers,
        commands::get_headings,
        commands::scroll_to_heading,
        commands::find_in_page,
        commands::count_in_page,
        commands::end_find,
        commands::focus_chrome,
        commands::share_page,
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
    // Snorkle as the default browser: iOS sends links tapped in other apps.
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
    // Only iOS has a content host; desktop builds run the chrome alone.
    let os_support =
        if cfg!(target_os = "ios") { os::check(snorkle_content::os_version()) } else { OsSupport::PreviewOnly };
    let browser = make_browser(os_support, snorkle_content::physical_memory());

    let store = app.path().app_data_dir().ok().and_then(|d| Store::open(d.join("profile-default")).ok());
    let session = store.as_ref().map(|s| s.load_session()).unwrap_or_default();

    let handle = app.handle().clone();
    let emit: Emitter = Box::new(move |ev| {
        let _ = match ev {
            ChromeEvent::State(s) => handle.emit_to(CHROME_LABEL, "state", s),
            ChromeEvent::Toast(m) => handle.emit_to(CHROME_LABEL, "toast", m),
            ChromeEvent::Shortcut(name) => handle.emit_to(CHROME_LABEL, "shortcut", name),
            ChromeEvent::PinsChanged => handle.emit_to(CHROME_LABEL, "pins", ()),
            ChromeEvent::Console(entry) => handle.emit_to(CHROME_LABEL, "console", entry),
            ChromeEvent::ConsoleReset => handle.emit_to(CHROME_LABEL, "console_reset", ()),
            ChromeEvent::HeadingCurrent(tab, index) => {
                handle.emit_to(CHROME_LABEL, "heading_current", serde_json::json!({ "tab": tab, "index": index }))
            }
            ChromeEvent::Headings(tab, list) => {
                handle.emit_to(CHROME_LABEL, "headings", serde_json::json!({ "tab": tab, "headings": list }))
            }
            ChromeEvent::Selection(tab, rect) => {
                handle.emit_to(CHROME_LABEL, "selection", serde_json::json!({ "tab": tab, "rect": rect }))
            }
        };
    });
    let allowlist = store.as_ref().map(|s| s.adblock_allowed()).unwrap_or_default();
    let shared = Shared::new(browser, store, emit);
    let _ = shared.browser().set_adblock_allowlist(allowlist);
    app.manage(shared.clone());

    let window = app.get_webview_window(CHROME_LABEL).ok_or("main window missing")?;
    attach_content_host(&window, shared.clone())?;
    // The chrome's own WKWebView, so the host can give it keyboard focus.
    #[cfg(target_os = "ios")]
    window.with_webview(|w| {
        let view = w.inner();
        // SAFETY: Tauri runs this on the main thread; `view` is the chrome's
        // WKWebView, alive as long as the window (the host retains it).
        state::with_host(|h| unsafe { h.set_chrome_view(view) });
    })?;
    // Debug desktop builds: open the chrome's Web Inspector (console etc.).
    // On iOS use Safari > Develop > <device or Simulator> instead.
    #[cfg(all(debug_assertions, desktop))]
    window.open_devtools();

    pins::refresh_icons(&shared);
    // Injected scripts first, so restored views are created with them.
    let capture = shared.capturing();
    snorkle_content::dispatch_main(move || state::with_host(|h| h.set_console_capture(capture)));
    let effects = shared.browser().restore_session(&session, state::now_ms());
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
    let pending = std::mem::take(&mut *PENDING_LINKS.lock().unwrap_or_else(|e| e.into_inner()));
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

#[cfg(not(target_os = "ios"))]
fn attach_content_host<R: Runtime>(
    _window: &tauri::WebviewWindow<R>,
    shared: Arc<Shared>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Development stand-in: no web content is rendered off iOS.
    unsafe { state::install_host(std::ptr::null_mut(), shared) };
    Ok(())
}
