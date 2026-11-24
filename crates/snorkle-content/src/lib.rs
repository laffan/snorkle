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

// iPadOS (UIKit) and macOS (AppKit) hosts; the WebKit code they share
// (delegates, injection, content blocking, HTTPS requests, Keychain) is in
// `webkit`, which reaches its platform through `crate::platform`.
#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "ios")]
use ios as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(any(target_os = "ios", target_os = "macos"))]
mod webkit;
#[cfg(any(target_os = "ios", target_os = "macos"))]
pub use platform::{dispatch_main, os_version, physical_memory, webkit_version, ContentHost};
#[cfg(any(target_os = "ios", target_os = "macos"))]
pub use webkit::exclude_from_backup;
#[cfg(any(target_os = "ios", target_os = "macos"))]
pub use webkit::{http, keychain, ocr, passkeys};

#[cfg(not(any(target_os = "ios", target_os = "macos")))]
mod stub;
#[cfg(not(any(target_os = "ios", target_os = "macos")))]
pub use stub::{
    dispatch_main, exclude_from_backup, http, keychain, ocr, os_version, passkeys, physical_memory, webkit_version,
    ContentHost,
};

mod shortcuts;
pub use shortcuts::*;

/// The person's permission for the app to use their passkeys in web pages
/// (AuthenticationServices' authorization state for browsers;
/// `passkeys::permission`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasskeyPermission {
    Authorized,
    Denied,
    NotDetermined,
    /// The system has no such permission (older OS, no AuthenticationServices).
    Unavailable,
}

impl PasskeyPermission {
    /// From `ASAuthorizationWebBrowserPublicKeyCredentialManagerAuthorizationState`
    /// (authorized 0, denied 1, not determined 2).
    pub fn from_raw(v: isize) -> Self {
        match v {
            0 => PasskeyPermission::Authorized,
            1 => PasskeyPermission::Denied,
            2 => PasskeyPermission::NotDetermined,
            _ => PasskeyPermission::Unavailable,
        }
    }

    /// Name passed to the chrome.
    pub fn name(self) -> &'static str {
        match self {
            PasskeyPermission::Authorized => "authorized",
            PasskeyPermission::Denied => "denied",
            PasskeyPermission::NotDetermined => "not_determined",
            PasskeyPermission::Unavailable => "unavailable",
        }
    }
}

/// Width of the strip along the window's left edge where the pointer is
/// reported (the edge tab is 14 pt wide and shows within 100 pt of it).
pub const EDGE_ZONE: f64 = 160.0;

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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewInjections {
    pub sources: Vec<Injection>,
    /// Brings an already-loaded page in line with these modes.
    pub apply_js: String,
    /// Dark mode: the web view prefers a dark appearance, so pages with
    /// their own dark theme (`prefers-color-scheme`) use it.
    pub dark: bool,
    /// Night vision: the web view's own canvas (under the page, and past
    /// its edges when it is pulled) is drawn as night vision draws it —
    /// black stays black, white becomes pure red — since the page's
    /// layers (`viewmodes::NIGHT_CSS`) cannot reach it.
    pub night: bool,
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

/// The app's appearance (Settings → General): the window's interface style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    System,
    Light,
    Dark,
}

impl Appearance {
    /// From the chrome's setting ("light", "dark"; anything else: system).
    pub fn from_name(name: &str) -> Self {
        match name {
            "light" => Appearance::Light,
            "dark" => Appearance::Dark,
            _ => Appearance::System,
        }
    }
}

/// Callbacks from content web views into the browser core.
///
/// Called on the main thread from inside WebKit delegate methods.
/// Implementations MUST NOT call back into [`ContentHost`] synchronously;
/// schedule resulting effects with [`dispatch_main`] instead. They must
/// also be fast: `decide_navigation` blocks the navigation.
/// Settings → General → Swipe between pages: whether new content views get
/// WebKit's back/forward swipe (`CONTENT_VIEW_SPEC` must allow it too).
static SWIPE_NAVIGATION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn swipe_navigation() -> bool {
    SWIPE_NAVIGATION.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_swipe_navigation(on: bool) {
    SWIPE_NAVIGATION.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Settings → General → Open links in apps (2026-11-09; default off):
/// whether WebKit may hand a page to an installed app that claims its
/// address (universal links: Google Docs, Sheets, …) when the user follows
/// a link to another site. Off, every page loads in the browser and the
/// app is one tap away (⋯ → Open in App, iPadOS).
static OPEN_APP_LINKS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn open_app_links() -> bool {
    OPEN_APP_LINKS.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_open_app_links(on: bool) {
    OPEN_APP_LINKS.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Called with whether an app opened (⋯ → Open in App).
pub type OpenedDone = Box<dyn FnOnce(bool) + Send>;

/// Called with a JPEG thumbnail, or `None` (Save for Later).
pub type ThumbDone = Box<dyn FnOnce(Option<Vec<u8>>) + Send>;
/// Width of a Save for Later thumbnail, in points (the snapshot is drawn
/// at the screen's scale).
pub const THUMB_WIDTH: f64 = 320.0;
/// JPEG quality of a thumbnail.
pub const THUMB_QUALITY: f64 = 0.6;

/// Notebook clips (2026-11-15): the widest a clip is drawn, in points (at
/// the screen's scale), its JPEG quality, and the largest kept.
pub const CLIP_MAX_WIDTH: f64 = 900.0;
pub const CLIP_QUALITY: f64 = 0.82;
pub const CLIP_MAX_BYTES: usize = 2 * 1024 * 1024;

/// A region a page reported (`notebook` C23: visual-viewport CSS pixels,
/// `vw` that viewport's width) in its web view's coordinates (`size`, its
/// bounds), cut to the view; `None` when nothing of at least 4 × 4 points
/// is left or the numbers are not finite.
pub fn clip_rect(rect: (f64, f64, f64, f64), vw: f64, size: (f64, f64)) -> Option<(f64, f64, f64, f64)> {
    let (x, y, w, h) = rect;
    let finite = [x, y, w, h, vw, size.0, size.1].iter().all(|n| n.is_finite());
    if !finite || vw < 1.0 || size.0 < 1.0 || size.1 < 1.0 {
        return None;
    }
    let k = size.0 / vw;
    let (x0, y0) = ((x * k).max(0.0), (y * k).max(0.0));
    let (x1, y1) = (((x + w) * k).min(size.0), ((y + h) * k).min(size.1));
    (x1 - x0 >= 4.0 && y1 - y0 >= 4.0).then_some((x0, y0, x1 - x0, y1 - y0))
}

/// Settings → General → Pull to refresh (iPadOS): whether content views get
/// a `UIRefreshControl` (OPEN_QUESTIONS 109: suspected of turning trackpad
/// scrolls into back swipes).
static PULL_TO_REFRESH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn pull_to_refresh() -> bool {
    PULL_TO_REFRESH.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_pull_to_refresh(on: bool) {
    PULL_TO_REFRESH.store(on, std::sync::atomic::Ordering::Relaxed);
}

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
    /// The user ⌘-clicked a link to `url` in `opener` (WebKit's modifier
    /// flags on a link activation); the link did not navigate `opener`.
    fn background_tab_requested(&self, opener: TabId, url: Url);
    /// The selection watcher reported where the selection is (a JSON
    /// string, page-influenced; `tab` comes from WebKit).
    fn selection(&self, tab: Option<TabId>, body: String);
    /// Where the pointer (trackpad, mouse) is while it is within
    /// `EDGE_ZONE` points of the window's left edge, in window points
    /// (throttled), and `None` once it leaves that strip or stops hovering.
    /// For the sidebar's edge tab, which shows only near the pointer.
    fn edge_pointer(&self, _at: Option<(f64, f64)>) {}
    /// Whether the pointer (trackpad, mouse) is over the chrome: inside the
    /// window and not over a page (a hole counts as the chrome). Sent when
    /// it changes, and `false` once the pointer leaves the window or stops
    /// hovering. The chrome's web view does not reliably see the pointer
    /// leave it for a page, so the chrome uses `false` to know the pointer
    /// is no longer over its sidebar (2026-11-24).
    fn chrome_pointer(&self, _over: bool) {}
    /// ⌥ was pressed or let go, whatever has keyboard focus (macOS; the
    /// chrome marks what ⌥ changes, as the copy button's "to notebook").
    fn option_key(&self, _down: bool) {}
    /// Whether a main-frame download of `url` may start in `tab`
    /// (`policy::may_download`, and not too many at once). Downloads are
    /// refused unless the app implements this.
    fn may_download(&self, _tab: TabId, _url: &Url) -> bool {
        false
    }
    /// A download is starting: its id and the path (in the app's staging
    /// folder) to write it to, or `None` to refuse it. `source` is where it
    /// comes from (WebKit's request URL), `suggested` the page's or
    /// server's file name (untrusted), `expected` its announced size.
    fn download_destination(
        &self,
        _tab: TabId,
        _source: Option<Url>,
        _suggested: String,
        _mime: String,
        _expected: Option<u64>,
    ) -> Option<(u64, std::path::PathBuf)> {
        None
    }
    /// A frame inside `tab`'s page wants to download `url`: ask the user,
    /// then answer with `ContentHost::answer_frame_download(ask, allow)`.
    /// Unanswered requests are blocked after a minute.
    fn frame_download_requested(&self, _tab: TabId, _ask: u64, _url: &Url) {}
    /// Bytes received so far (about twice a second while it runs).
    fn download_progress(&self, _id: u64, _received: u64, _expected: Option<u64>) {}
    /// A download ended: complete (`None`), or failed or cancelled (a
    /// message, sanitised).
    fn download_finished(&self, _id: u64, _error: Option<String>) {}
    /// A note for the in-app console (native diagnostics, no page data).
    fn note(&self, _text: &str) {}
    /// The shortcut editor's recording: the ⌘ combination pressed (key,
    /// `MOD_*` bits), or `None` for Escape.
    fn key_recorded(&self, _key: Option<(String, u8)>) {}
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
/// A favicon the app drew: PNG bytes, and (2026-11-24) a copy for the
/// dark appearance when the icon is too dark to see there
/// (`snorkle_core::favicon::needs_dark_variant`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaviconPngs {
    pub png: Vec<u8>,
    pub dark: Option<Vec<u8>>,
}
/// Completion of a favicon fetch: the app-encoded icon, or `None`.
pub type FaviconDone = Box<dyn FnOnce(Option<FaviconPngs>) + Send>;
/// Completion of a rule-list install, with the number of lists installed.
pub type RulesDone = Box<dyn FnOnce(Result<usize, String>) + Send>;

/// Completion of a page-text read (AI chat): the script's string result,
/// page-controlled and bounded to [`MAX_PAGE_TEXT_BYTES`], or `None`.
pub type TextDone = Box<dyn FnOnce(Option<String>) + Send>;
/// Completion of an image fetch (image menu): bytes and MIME type, or a
/// message for the user. Runs on a background queue.
pub type ImageDone = Box<dyn FnOnce(Result<(Vec<u8>, String), String>) + Send>;
/// Completion of an offline-copy save: the page's web archive, or `None`.
pub type DataDone = Box<dyn FnOnce(Option<Vec<u8>>) + Send>;
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

/// The holes that cut into a view placed at `frame` (x, y, width, height,
/// window points from the top left). A page view is masked only where
/// chrome floats over it (2026-11-23): a hole beside it changes none of its
/// pixels, and any mask makes Core Animation draw the whole view offscreen
/// and composite it again on every frame of a scroll. Holes that only
/// touch its edge do not count.
pub fn holes_over(holes: &[Hole], frame: (f64, f64, f64, f64)) -> Vec<Hole> {
    let (x, y, w, h) = frame;
    holes.iter().filter(|o| o.x < x + w && o.x + o.w > x && o.y < y + h && o.y + o.h > y).copied().collect()
}

/// Validates holes reported by the chrome: finite, within reach of a
/// `width` × `height` window (a hole may wait just off-screen, e.g. the
/// hidden sidebar pop-over's, so it can slide in), non-empty, at most
/// [`MAX_HOLES`], and merged where they overlap (the mask's even-odd fill
/// would otherwise draw the overlap). Holes that only touch stay separate.
pub fn sanitize_holes(raw: &[Hole], width: f64, height: f64) -> Vec<Hole> {
    let reach = 2.0 * width.max(height).max(1.0);
    let mut out: Vec<Hole> = Vec::new();
    for h in raw.iter().take(MAX_HOLES) {
        if ![h.x, h.y, h.w, h.h, h.r].iter().all(|v| v.is_finite()) || h.w < 1.0 || h.h < 1.0 {
            continue;
        }
        if h.x.abs() > reach || h.y.abs() > reach || h.w > reach || h.h > reach {
            continue;
        }
        let corners = if h.corners & ALL_CORNERS == 0 { ALL_CORNERS } else { h.corners & ALL_CORNERS };
        let mut hole = Hole { x: h.x, y: h.y, w: h.w, h: h.h, r: h.r.clamp(0.0, h.w.min(h.h) / 2.0), corners };
        // Merge with every hole it overlaps (a merge can grow into others).
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
mod tests;
