//! Snorkle app entry point: Tauri hosts the trusted chrome UI (bundled
//! `ui/`, webview label `main`); `snorkle-content` hosts untrusted pages in
//! separate WKWebViews; `snorkle-core` makes every decision.
//! See docs/ARCHITECTURE.md.

mod command_list;
pub mod commands;
pub mod state;

use std::sync::Arc;
use std::time::Duration;

use snorkle_core::blocking::Blocklist;
use snorkle_core::browser::Browser;
use snorkle_core::os::{self, OsSupport};
use snorkle_core::storage::Store;
use snorkle_core::tabs::MemoryBudget;
use tauri::{Emitter as _, Manager, Runtime};

pub use command_list::COMMANDS;
pub use state::{ChromeEvent, Emitter, Shared};

/// Label of the only Tauri webview: the browser chrome.
pub const CHROME_LABEL: &str = "main";

const BUNDLED_BLOCKLIST: &str = include_str!("../resources/blocklist.txt");

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
        .run(tauri::generate_context!())
        .expect("error while running snorkle");
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
        };
    });
    let shared = Shared::new(browser, store, emit);
    app.manage(shared.clone());

    let window = app.get_webview_window(CHROME_LABEL).ok_or("main window missing")?;
    attach_content_host(&window, shared.clone())?;
    // Debug desktop builds: open the chrome's Web Inspector (console etc.).
    // On iOS use Safari > Develop > <device or Simulator> instead.
    #[cfg(all(debug_assertions, desktop))]
    window.open_devtools();

    let effects = shared.browser().restore_session(&session, state::now_ms());
    shared.run(effects);

    // Parse the blocklist and build the WebKit rule JSON off the main thread;
    // WebKit compiles it asynchronously as well.
    std::thread::spawn(|| {
        let list = Blocklist::parse(BUNDLED_BLOCKLIST);
        let json = (!list.is_empty()).then(|| list.to_webkit_rules_json());
        snorkle_content::dispatch_main(move || state::with_host(|h| h.install_rules(json)));
    });

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
