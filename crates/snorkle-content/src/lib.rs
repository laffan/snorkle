//! SECURITY-REVIEW: host for untrusted web content.
//!
//! Each live tab is a `WKWebView` created here, in Rust, through objc2. These
//! web views are NOT Tauri webviews: Tauri never sees them, so no Tauri IPC
//! bridge, init script, custom protocol or invoke key is ever injected into
//! them. Their only connection to the app is the WebKit delegate callbacks
//! implemented in `ios::delegates`, which carry WebKit-provided metadata
//! (URLs, navigation types, errors), never page-chosen messages.
//!
//! Every way a page could send data to native code is listed in
//! [`CONTENT_VIEW_SPEC`] and must stay empty unless a new channel has been
//! security-reviewed and signed off (docs/SECURITY_REVIEW.md). Tests in
//! `tests/` fail if code that would create such a channel appears.
//!
//! Why not wry for content views: on iOS, Tauri (`Window::add_child`) and
//! wry (`build_as_child`) do not support more than one webview per window,
//! and wry's iOS delegate surface lacks main-frame information, provisional
//! failure callbacks (needed for HTTPS-first fallback) and popup handling.
//! The browser chrome itself still uses Tauri + wry.

use snorkle_core::browser::{NavErrorKind, NavKind, PopupStyle, Verdict};
use snorkle_core::policy::Frame;
use snorkle_core::tabs::TabId;
use url::Url;

pub use snorkle_core::browser::Effect;

#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "ios")]
pub use ios::{dispatch_main, os_version, physical_memory, ContentHost};

#[cfg(not(target_os = "ios"))]
mod stub;
#[cfg(not(target_os = "ios"))]
pub use stub::{dispatch_main, os_version, physical_memory, ContentHost};

/// Website data the user can delete (Settings). Applies to the default
/// profile's store and the current private store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataKind {
    /// HTTP disk and memory caches, fetch and offline caches.
    Cache,
    /// Cookies (signs out of sites that use them for logins).
    Cookies,
    /// Local and session storage, IndexedDB, WebSQL, service workers, file
    /// system, media keys.
    SiteData,
    /// Everything that keeps you signed in: cookies and site data.
    /// Passwords are not stored by the app (iOS Passwords app).
    Logins,
}

/// Browser keyboard shortcuts handled natively (see `ios::keys`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    NewTab,
    ToggleSidebar,
    /// ⌘1–⌘5: the n-th item in the sidebar (pins, bookmarks, then tabs).
    Item(u8),
}

impl Shortcut {
    /// Name passed to the chrome UI, which performs the action.
    pub fn name(self) -> String {
        match self {
            Shortcut::NewTab => "new_tab".into(),
            Shortcut::ToggleSidebar => "toggle_sidebar".into(),
            Shortcut::Item(n) => format!("item:{n}"),
        }
    }
}

/// Callbacks from content web views into the browser core.
///
/// Called on the main thread from inside WebKit delegate methods.
/// Implementations MUST NOT call back into [`ContentHost`] synchronously;
/// schedule resulting effects with [`dispatch_main`] instead. They must
/// also be fast: `decide_navigation` blocks the navigation.
pub trait ContentEvents: 'static {
    fn decide_navigation(&self, tab: TabId, url: &Url, frame: Frame, kind: NavKind) -> Verdict;
    fn new_window_request(&self, opener: TabId, url: Url);
    /// `window.open` / target=_blank: return the new tab if the platform
    /// should create a popup web view for it (synchronous). `style` is
    /// `Sheet` when the page asked for a window size.
    fn popup_requested(&self, opener: TabId, url: Option<Url>, style: PopupStyle) -> Option<TabId>;
    /// The page called `window.close()`.
    fn close_requested(&self, tab: TabId);
    fn provisional_start(&self, tab: TabId);
    fn commit(&self, tab: TabId, url: Url);
    fn finish(&self, tab: TabId);
    fn fail(&self, tab: TabId, provisional: bool, kind: NavErrorKind, failing_url: Option<Url>, message: String);
    fn url_changed(&self, tab: TabId, url: Url);
    fn title_changed(&self, tab: TabId, title: String);
    fn progress(&self, tab: TabId, fraction: f64);
    fn history_state(&self, tab: TabId, can_go_back: bool, can_go_forward: bool);
    fn secure_content(&self, tab: TabId, only_secure: bool);
    fn process_terminated(&self, tab: TabId);
    /// A JavaScript alert/confirm/prompt was suppressed (no dialog UI yet).
    fn dialog_suppressed(&self, tab: TabId, origin_host: String);
    /// A hardware-keyboard shortcut was pressed while a page had focus.
    fn shortcut(&self, shortcut: Shortcut);
    /// The user tapped inside a content pane (focus it in split view).
    fn focus_requested(&self, tab: TabId);
    /// A page's console output or uncaught error (only while console
    /// capture is on). `body` is page-controlled; `tab`, `origin_host` and
    /// `main_frame` come from WebKit.
    fn console_message(&self, tab: Option<TabId>, origin_host: String, main_frame: bool, body: String);
    fn memory_warning(&self);
    fn did_enter_background(&self);
}

/// Configuration applied to every content `WKWebViewConfiguration`.
///
/// The three channel lists are the complete set of page-to-native message
/// paths WebKit offers. They are empty by design; the iOS code installs
/// exactly what is listed here and nothing else.
#[derive(Debug)]
pub struct ContentViewSpec {
    /// `WKUserContentController.addUserScript` sources.
    pub user_scripts: &'static [&'static str],
    /// `WKUserContentController.addScriptMessageHandler` names
    /// (`window.webkit.messageHandlers.<name>`).
    pub script_message_handlers: &'static [&'static str],
    /// `WKWebViewConfiguration.setURLSchemeHandler` schemes.
    pub url_scheme_handlers: &'static [&'static str],
    /// The one message handler installed on demand: page console capture,
    /// page world only, only while the user has capture on (channel C13).
    pub console_capture_handler: &'static str,
    /// Block `window.open` without a user gesture.
    pub javascript_can_open_windows_automatically: bool,
    /// Element fullscreen lets a page hide browser chrome and draw a fake
    /// address bar; disabled. Video still uses the native player.
    pub element_fullscreen: bool,
    pub upgrade_known_hosts_to_https: bool,
    pub limits_navigations_to_app_bound_domains: bool,
    /// WebKit Safe Browsing warnings (lookups are proxied by Apple).
    pub fraudulent_website_warning: bool,
    /// Web Inspector access. Debug builds only.
    pub inspectable: bool,
    pub allows_back_forward_gestures: bool,
    pub allows_link_preview: bool,
    /// Present Safari's iPad (desktop-class) user agent and request desktop
    /// content. WKWebView's default UA lacks Safari's `Version/… Safari/…`
    /// tokens, which sites such as Google sign-in/Gmail treat as an
    /// unsupported embedded browser. Other iOS browsers built on WKWebView
    /// do the same. Trade-off: sites cannot tell Snorkle from Safari (which
    /// also means one less fingerprinting bit).
    pub safari_desktop_user_agent: bool,
}

pub const CONTENT_VIEW_SPEC: ContentViewSpec = ContentViewSpec {
    user_scripts: &[],
    script_message_handlers: &[],
    url_scheme_handlers: &[],
    console_capture_handler: snorkle_core::console::PAGE_HANDLER_NAME,
    javascript_can_open_windows_automatically: false,
    element_fullscreen: false,
    upgrade_known_hosts_to_https: true,
    limits_navigations_to_app_bound_domains: false,
    fraudulent_website_warning: true,
    inspectable: cfg!(debug_assertions),
    allows_back_forward_gestures: true,
    allows_link_preview: false,
    safari_desktop_user_agent: true,
};

/// The user agent Safari sends on iPad in desktop mode (identical to macOS
/// Safari's; the macOS version token is frozen at 10_15_7 by WebKit), with
/// Safari's version taken from the running OS version, as Safari's is.
pub fn safari_desktop_user_agent(os: snorkle_core::os::OsVersion) -> String {
    format!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/{}.{} Safari/605.1.15",
        os.major, os.minor
    )
}

/// Fixed identifier of the default profile's persistent
/// `WKWebsiteDataStore(forIdentifier:)`. Must never be all zeros (WebKit
/// throws). Changing it orphans existing website data.
pub const DEFAULT_PROFILE_STORE_ID: [u8; 16] =
    [0x5e, 0x0b, 0x2c, 0x51, 0x7a, 0x4d, 0x4f, 0x2e, 0x9c, 0x3b, 0x1d, 0x6a, 0x8e, 0x70, 0x42, 0x13];

/// Produces content-blocking rule-list JSON documents (runs off the main
/// thread).
pub type MakeRuleLists = Box<dyn FnOnce() -> Result<Vec<String>, String> + Send>;
/// Completion of a favicon fetch: app-encoded PNG bytes, or `None`.
pub type FaviconDone = Box<dyn FnOnce(Option<Vec<u8>>) + Send>;
/// Completion of a rule-list install, with the number of lists installed.
pub type RulesDone = Box<dyn FnOnce(Result<usize, String>) + Send>;

/// Largest accepted chrome inset (toolbar height, sidebar width) as a
/// fraction of the window extent, so the chrome can never shrink the
/// content area to nothing.
pub const MAX_CHROME_INSET_FRACTION: f64 = 0.5;

/// Clamps a content-area inset (top or left, in points) reported by the
/// chrome to `[0, extent * MAX_CHROME_INSET_FRACTION]`.
pub fn clamp_inset(inset: f64, extent: f64) -> f64 {
    if !inset.is_finite() {
        return 0.0;
    }
    inset.clamp(0.0, (extent * MAX_CHROME_INSET_FRACTION).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp() {
        assert_eq!(clamp_inset(f64::NAN, 1000.0), 0.0);
        assert_eq!(clamp_inset(f64::INFINITY, 1000.0), 0.0);
        assert_eq!(clamp_inset(-5.0, 1000.0), 0.0);
        assert_eq!(clamp_inset(5000.0, 1000.0), 500.0);
        assert_eq!(clamp_inset(64.0, 1000.0), 64.0);
    }

    #[test]
    fn user_agent_matches_safari_desktop_format() {
        let ua = safari_desktop_user_agent(snorkle_core::os::OsVersion::new(26, 6, 1));
        assert_eq!(
            ua,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.6 Safari/605.1.15"
        );
    }

    #[test]
    fn store_id_nonzero() {
        assert!(DEFAULT_PROFILE_STORE_ID.iter().any(|b| *b != 0));
    }
}
