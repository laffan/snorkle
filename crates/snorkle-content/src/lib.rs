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

use std::collections::HashMap;

use snorkle_core::browser::{NavErrorKind, NavKind, PopupStyle, Verdict};
use snorkle_core::policy::Frame;
use snorkle_core::tabs::TabId;
use snorkle_core::userscripts::Injection;
use url::Url;

pub use snorkle_core::browser::Effect;

#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "ios")]
pub use ios::{dispatch_main, http, keychain, os_version, physical_memory, ContentHost};

#[cfg(not(target_os = "ios"))]
mod stub;
#[cfg(not(target_os = "ios"))]
pub use stub::{dispatch_main, http, keychain, os_version, physical_memory, ContentHost};

/// Completion of an HTTPS request: status and body, or a short error.
/// Called once, on any thread.
pub type HttpDone = Box<dyn FnOnce(Result<(u16, Vec<u8>), String>) + Send>;

/// Everything injected into content views: the user's scripts (every tab)
/// and the view-mode sources, app-wide with per-tab overrides for tabs
/// exempted from a mode.
#[derive(Clone, Debug, Default)]
pub struct Injections {
    pub user: Vec<Injection>,
    pub view: ViewInjections,
    pub tabs: HashMap<TabId, ViewInjections>,
}

/// View-mode sources for one set of modes.
#[derive(Clone, Debug, Default)]
pub struct ViewInjections {
    pub sources: Vec<Injection>,
    /// Brings an already-loaded page in line with these modes.
    pub apply_js: String,
    /// Install the headings message handler (headers mode on).
    pub headings: bool,
}

impl Injections {
    pub fn view_for(&self, tab: TabId) -> &ViewInjections {
        self.tabs.get(&tab).unwrap_or(&self.view)
    }
}

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

/// Browser keyboard shortcuts handled natively (the app's main menu,
/// `ios::menu`, with the content containers' key commands as a fallback,
/// `ios::keys`); the chrome UI performs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    NewTab,
    CloseTab,
    /// ⌘S or ⌘\: show or hide the tab sidebar.
    ToggleSidebar,
    /// ⌘⇧\: show or hide the right panel (headers, chat).
    ToggleRightPanel,
    /// ⌘⇧D: bookmark the current page.
    Bookmark,
    /// ⌘F: find in page.
    Find,
    /// ⌘1–⌘5: the n-th item in the sidebar (pins, bookmarks, then tabs).
    Item(u8),
}

/// Every native shortcut: menu, title, key, whether Shift is held, action.
pub const SHORTCUTS: &[(ShortcutMenu, &str, &str, bool, Shortcut)] = &[
    (ShortcutMenu::File, "New Tab", "t", false, Shortcut::NewTab),
    (ShortcutMenu::File, "Add Bookmark", "d", true, Shortcut::Bookmark),
    (ShortcutMenu::Close, "Close Tab", "w", false, Shortcut::CloseTab),
    (ShortcutMenu::Find, "Find on Page", "f", false, Shortcut::Find),
    (ShortcutMenu::View, "Show or Hide Sidebar", "s", false, Shortcut::ToggleSidebar),
    (ShortcutMenu::View, "Toggle Sidebar", "\\", false, Shortcut::ToggleSidebar),
    (ShortcutMenu::View, "Toggle Right Panel", "\\", true, Shortcut::ToggleRightPanel),
    (ShortcutMenu::Tabs, "Go to Item 1", "1", false, Shortcut::Item(1)),
    (ShortcutMenu::Tabs, "Go to Item 2", "2", false, Shortcut::Item(2)),
    (ShortcutMenu::Tabs, "Go to Item 3", "3", false, Shortcut::Item(3)),
    (ShortcutMenu::Tabs, "Go to Item 4", "4", false, Shortcut::Item(4)),
    (ShortcutMenu::Tabs, "Go to Item 5", "5", false, Shortcut::Item(5)),
];

/// Where a shortcut appears in the iPadOS menu bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutMenu {
    File,
    /// Replaces the system's Close (⌘W closes a tab, not the window).
    Close,
    /// Replaces the system's Find menu.
    Find,
    View,
    /// A "Tabs" menu after View.
    Tabs,
}

impl Shortcut {
    /// Name passed to the chrome UI, which performs the action.
    pub fn name(self) -> String {
        match self {
            Shortcut::NewTab => "new_tab".into(),
            Shortcut::CloseTab => "close_tab".into(),
            Shortcut::ToggleSidebar => "toggle_sidebar".into(),
            Shortcut::ToggleRightPanel => "toggle_right".into(),
            Shortcut::Bookmark => "bookmark".into(),
            Shortcut::Find => "find".into(),
            Shortcut::Item(n) => format!("item:{n}"),
        }
    }

    /// The shortcut a menu or key command named (its property list).
    pub fn from_name(name: &str) -> Option<Shortcut> {
        SHORTCUTS.iter().map(|s| s.4).find(|s| s.name() == name)
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
    /// The user pulled the page down to refresh it.
    fn reload_requested(&self, tab: TabId);
    /// A page's console output or uncaught error (only while console
    /// capture is on). `body` is page-controlled; `tab`, `origin_host` and
    /// `main_frame` come from WebKit.
    fn console_message(&self, tab: Option<TabId>, origin_host: String, main_frame: bool, body: String);
    /// The headers view's extractor reported the page's headings (a JSON
    /// string, only while headers mode is on for the tab). `body` is
    /// derived from page content; `tab` comes from WebKit.
    fn headings(&self, tab: Option<TabId>, body: String);
    /// The user ⌘-clicked a link to `url` in `opener` (WebKit's modifier
    /// flags on a link activation); the link did not navigate `opener`.
    fn background_tab_requested(&self, opener: TabId, url: Url);
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

/// Completion of a page-text read (AI chat): the script's string result,
/// page-controlled and bounded to [`MAX_PAGE_TEXT_BYTES`], or `None`.
pub type TextDone = Box<dyn FnOnce(Option<String>) + Send>;
/// Completion of a find in page: whether a match was found.
pub type FindDone = Box<dyn FnOnce(bool) + Send>;
/// Largest page-text result read back from a page.
pub const MAX_PAGE_TEXT_BYTES: usize = snorkle_core::ai::MAX_PAGE_REPORT;

/// A region of the window where the chrome floats over the content area
/// (the URL pill, its popups and toasts), in points, window coordinates.
/// Content views are cut out there (not drawn, and touches pass through to
/// the chrome below), so a page can never draw over or under it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hole {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Corner radius.
    pub r: f64,
    /// Which corners are rounded: bits 1 top left, 2 top right, 4 bottom
    /// left, 8 bottom right (UIKit's `UIRectCorner`); 15 for all.
    pub corners: u8,
}

/// All four corners rounded.
pub const ALL_CORNERS: u8 = 15;

impl Hole {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    fn overlaps(&self, o: &Hole) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

/// Most holes the chrome may cut at once.
pub const MAX_HOLES: usize = 8;

/// Validates holes reported by the chrome: finite, inside a `width` ×
/// `height` window, non-empty, at most [`MAX_HOLES`], and merged where
/// they overlap (the mask's even-odd fill would otherwise draw the overlap).
pub fn sanitize_holes(raw: &[Hole], width: f64, height: f64) -> Vec<Hole> {
    let mut out: Vec<Hole> = Vec::new();
    for h in raw.iter().take(MAX_HOLES) {
        if ![h.x, h.y, h.w, h.h, h.r].iter().all(|v| v.is_finite()) {
            continue;
        }
        let x0 = h.x.clamp(0.0, width.max(0.0));
        let y0 = h.y.clamp(0.0, height.max(0.0));
        let x1 = (h.x + h.w).clamp(0.0, width.max(0.0));
        let y1 = (h.y + h.h).clamp(0.0, height.max(0.0));
        if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
            continue;
        }
        let corners = if h.corners & ALL_CORNERS == 0 { ALL_CORNERS } else { h.corners & ALL_CORNERS };
        let mut hole = Hole { x: x0, y: y0, w: x1 - x0, h: y1 - y0, r: 0.0, corners };
        hole.r = h.r.clamp(0.0, hole.w.min(hole.h) / 2.0);
        // Merge with every hole it touches (repeat: a merge can grow into others).
        while let Some(i) = out.iter().position(|o| o.overlaps(&hole)) {
            let o = out.remove(i);
            let (x0, y0) = (o.x.min(hole.x), o.y.min(hole.y));
            let (x1, y1) = ((o.x + o.w).max(hole.x + hole.w), (o.y + o.h).max(hole.y + hole.h));
            hole = Hole { x: x0, y: y0, w: x1 - x0, h: y1 - y0, r: o.r.min(hole.r), corners: ALL_CORNERS };
        }
        out.push(hole);
    }
    out
}

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
    fn holes_are_validated_and_merged() {
        let h = |x, y, w, h, r| Hole { x, y, w, h, r, corners: ALL_CORNERS };
        let out = sanitize_holes(
            &[h(10.0, 900.0, 400.0, 46.0, 23.0), h(f64::NAN, 0.0, 1.0, 1.0, 0.0), h(5.0, 5.0, 0.0, 10.0, 0.0)],
            1000.0,
            1000.0,
        );
        assert_eq!(out, vec![h(10.0, 900.0, 400.0, 46.0, 23.0)]);
        let clipped = sanitize_holes(&[h(-50.0, 980.0, 100.0, 100.0, 99.0)], 1000.0, 1000.0);
        assert_eq!(clipped, vec![h(0.0, 980.0, 50.0, 20.0, 10.0)], "clamped to the window, radius too");
        let merged = sanitize_holes(&[h(0.0, 0.0, 10.0, 10.0, 4.0), h(5.0, 5.0, 10.0, 10.0, 2.0)], 100.0, 100.0);
        assert_eq!(merged, vec![h(0.0, 0.0, 15.0, 15.0, 2.0)]);
        let many: Vec<Hole> = (0..20).map(|i| h(i as f64 * 20.0, 0.0, 10.0, 10.0, 0.0)).collect();
        assert_eq!(sanitize_holes(&many, 1000.0, 1000.0).len(), MAX_HOLES);
        assert!(h(0.0, 0.0, 10.0, 10.0, 0.0).contains(5.0, 5.0) && !h(0.0, 0.0, 10.0, 10.0, 0.0).contains(10.0, 5.0));
        // Touching holes stay separate (the find bar sits on the pill).
        let touching = [h(0.0, 20.0, 10.0, 10.0, 2.0), Hole { corners: 3, ..h(2.0, 14.0, 6.0, 6.0, 2.0) }];
        let out = sanitize_holes(&touching, 100.0, 100.0);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].corners, 3, "top corners only");
        assert_eq!(
            sanitize_holes(&[Hole { corners: 0, ..h(0.0, 0.0, 9.0, 9.0, 1.0) }], 99.0, 99.0)[0].corners,
            ALL_CORNERS
        );
    }

    #[test]
    fn shortcuts_round_trip() {
        for (_, _, key, _, s) in SHORTCUTS {
            assert_eq!(Shortcut::from_name(&s.name()), Some(*s), "{key}");
        }
        assert!(Shortcut::from_name("nope").is_none());
        let keys: Vec<_> = SHORTCUTS.iter().map(|s| (s.2, s.3)).collect();
        let unique: std::collections::BTreeSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len(), "no key is bound twice");
    }

    #[test]
    fn store_id_nonzero() {
        assert!(DEFAULT_PROFILE_STORE_ID.iter().any(|b| *b != 0));
    }
}
