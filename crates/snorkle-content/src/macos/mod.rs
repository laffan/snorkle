//! SECURITY-REVIEW: macOS content host. Creates, lays out, drives and
//! destroys the per-tab `WKWebView`s and applies
//! `snorkle_core::browser::Effect`s, with the same rules as the iPadOS host
//! (`ios/mod.rs`); the WebKit side (delegates, injection, rule lists,
//! configuration) is shared code in `crate::webkit`.
//!
//! Security assumptions:
//! - Content views live in a [`views::ContentRoot`] added to the Tauri
//!   window's content view *after* (above) the chrome's web view. Each sits
//!   in a [`views::Container`] that clips to the content area the chrome
//!   reports (outside its sidebar and docked panels). The URL pill, its
//!   popups and toasts float over the content area as holes: containers
//!   and the sheet backdrop are masked there and are not hit there, so the
//!   chrome underneath shows through and gets the clicks, and a page cannot
//!   draw over the address bar. Anything else the chrome shows over the
//!   content area hides content views (via `HideView`).
//! - Configurations are built only from `CONTENT_VIEW_SPEC`
//!   (`webkit::stores`); no URL scheme handlers are installed. User scripts
//!   and the console handler are installed by `webkit::injection`.
//! - A popup sheet (`Pane::Sheet`) is centred in the content area over a
//!   dimming backdrop that swallows clicks.
//! - Each `Profile` maps to its own `WKWebsiteDataStore`; private tabs share
//!   one non-persistent store per private session, which is wiped and
//!   released when the last private tab closes.
//! - All methods run on the main thread (`MainThreadMarker` is required).
//!
//! Differences from iPadOS: no pull to refresh (no such control on macOS);
//! page scale uses `WKWebView.pageZoom`, which on macOS reflows the page in
//! a wider viewport (as Safari's zoom does), instead of a view transform;
//! there is no memory-warning notification, so only the live-view budget
//! and the background TTL limit memory.

mod favicon;
mod keys;
mod layout;
mod pages;
mod pressure;
mod effects;
mod views;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAnimationContext, NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSApplicationDidResignActiveNotification, NSAutoresizingMaskOptions, NSColor, NSImage, NSModalResponseOK,
    NSPasteboard, NSSavePanel, NSSharingServicePicker, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow,
    NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSBundle, NSComparisonResult, NSData, NSError, NSKeyValueObservingOptions,
    NSNotification, NSNotificationCenter, NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSOperationQueue,
    NSFileManager, NSPoint, NSRect, NSRectEdge, NSSize, NSString, NSURLRequest, NSURL, NSUUID,
};
use objc2_quartz_core::CAMediaTimingFunction;
pub(crate) use objc2_web_kit::WKWebView;
use objc2_web_kit::{WKUserContentController, WKWebViewConfiguration, WKWebsiteDataStore};
use snorkle_core::browser::{AfterCreate, Effect, Pane};
use snorkle_core::os::OsVersion;
use snorkle_core::policy::INTERNAL_SCHEMES;
use snorkle_core::tabs::{Profile, TabId};
use snorkle_core::userscripts::Injection;
use url::Url;

use self::views::{Container, ContentRoot, Holes, Tl, GLIDE_SECONDS};
use crate::webkit::delegates::{NavDelegate, Observer, TabIvars, UiDelegate, KVO_KEYS};
use crate::webkit::{injection, rules, stores};
use crate::{clamp_inset, ContentEvents, DataKind, Hole, CONTENT_VIEW_SPEC, DEFAULT_PROFILE_STORE_ID};

pub use crate::webkit::{dispatch_main, os_version, physical_memory};


// Adding any page-to-native channel requires writing installation code
// here, which this assertion forces to be a deliberate, reviewed change.
const _: () = assert!(
    CONTENT_VIEW_SPEC.user_scripts.is_empty()
        && CONTENT_VIEW_SPEC.script_message_handlers.is_empty()
        && CONTENT_VIEW_SPEC.url_scheme_handlers.is_empty()
);

struct View {
    container: Retained<Container>,
    web: Retained<WKWebView>,
    controller: Retained<WKUserContentController>,
    pane: Cell<Pane>,
    _nav: Retained<NavDelegate>,
    _ui: Retained<UiDelegate>,
    kvo: Retained<Observer>,
}

impl Drop for View {
    fn drop(&mut self) {
        WEB_VIEW_TABS.with(|m| m.borrow_mut().remove(&(Retained::as_ptr(&self.web) as usize)));
        unsafe {
            for key in KVO_KEYS {
                self.web.removeObserver_forKeyPath(&self.kvo, &NSString::from_str(key));
            }
            self.web.setNavigationDelegate(None);
            self.web.setUIDelegate(None);
            self.web.stopLoading();
        }
        self.container.removeFromSuperview();
    }
}

pub struct ContentHost {
    mtm: MainThreadMarker,
    window: Retained<NSWindow>,
    root: Retained<ContentRoot>,
    top: Cell<f64>,
    left: Cell<f64>,
    right: Cell<f64>,
    bottom: Cell<f64>,
    /// The first pane's share of a split (`Effect::SetSplitRatio`), 0.2–0.8.
    split_ratio: Cell<f64>,
    views: HashMap<TabId, View>,
    /// `interactionState` of discarded tabs (in memory only).
    saved: HashMap<TabId, Retained<AnyObject>>,
    default_store: Retained<WKWebsiteDataStore>,
    private_store: Option<Retained<WKWebsiteDataStore>>,
    events: Rc<dyn ContentEvents>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    scripts: injection::Scripts,
    tab_scripts: HashMap<TabId, injection::Scripts>,
    injections: crate::Injections,
    /// Page console capture (channel C13); off unless the user turns it on.
    console: bool,
    /// Dims the content area under a popup sheet and swallows its clicks.
    backdrop: Retained<Container>,
    /// Where the chrome floats over the content area (window coordinates).
    holes: Holes,
    /// The chrome's own web view (Tauri's), to give it keyboard focus.
    chrome: Option<Retained<NSView>>,
    /// What had keyboard focus before the pointer moved over the notebook
    /// (`hover_focus`), given it back when the pointer leaves.
    before_hover: Option<Retained<objc2_app_kit::NSResponder>>,
    /// The share sheet while it is open (AppKit does not retain it).
    sharing: Option<Retained<NSSharingServicePicker>>,
    /// Stacking order of the containers (a higher value is in front).
    z: Cell<u64>,
    _monitors: keys::Monitors,
    /// Memory pressure (macOS has no memory-warning notification).
    _pressure: pressure::Pressure,
}

thread_local! {
    /// Stacking key of each container (by address), read by `by_z`.
    static Z: RefCell<HashMap<usize, u64>> = RefCell::new(HashMap::new());
}

/// Orders subviews by their `Z` key; the chrome's own views (none here)
/// and anything without a key go to the back.
unsafe extern "C-unwind" fn by_z(a: NonNull<NSView>, b: NonNull<NSView>, _ctx: *mut c_void) -> NSComparisonResult {
    let key = |v: NonNull<NSView>| Z.with(|z| z.borrow().get(&(v.as_ptr() as usize)).copied().unwrap_or(0));
    match key(a).cmp(&key(b)) {
        std::cmp::Ordering::Less => NSComparisonResult::Ascending,
        std::cmp::Ordering::Equal => NSComparisonResult::Same,
        std::cmp::Ordering::Greater => NSComparisonResult::Descending,
    }
}


/// A configured content web view with its delegates, not yet placed.
pub(crate) struct WebViewParts {
    web: Retained<WKWebView>,
    nav: Retained<NavDelegate>,
    ui: Retained<UiDelegate>,
    kvo: Retained<Observer>,
}

/// SECURITY-REVIEW: builds a content web view from `config` and attaches our
/// delegates and observers (as `ios/mod.rs` `build_web_view`).
pub(crate) unsafe fn build_web_view(
    mtm: MainThreadMarker,
    tab: TabId,
    config: &WKWebViewConfiguration,
    size: NSSize,
    events: &Rc<dyn ContentEvents>,
) -> WebViewParts {
    let spec = &CONTENT_VIEW_SPEC;
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
    let web = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, config);
    if web.respondsToSelector(sel!(setInspectable:)) {
        web.setInspectable(spec.inspectable);
    }
    web.setAllowsBackForwardNavigationGestures(spec.allows_back_forward_gestures && crate::swipe_navigation());
    web.setAllowsLinkPreview(spec.allows_link_preview);
    // Pinch to zoom on the trackpad, as on the iPad (off by default on macOS).
    web.setAllowsMagnification(true);
    if spec.safari_desktop_user_agent {
        web.setCustomUserAgent(Some(&NSString::from_str(&crate::safari_desktop_user_agent(webkit_version()))));
    }
    let nav = NavDelegate::new(mtm, TabIvars::new(tab, events.clone()));
    let ui = UiDelegate::new(mtm, TabIvars::new(tab, events.clone()));
    let kvo = Observer::new(mtm, TabIvars::new(tab, events.clone()));
    web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*nav)));
    web.setUIDelegate(Some(ProtocolObject::from_ref(&*ui)));
    for key in KVO_KEYS {
        web.addObserver_forKeyPath_options_context(
            &kvo,
            &NSString::from_str(key),
            NSKeyValueObservingOptions::New,
            std::ptr::null_mut(),
        );
    }
    WebViewParts { web, nav, ui, kvo }
}

thread_local! {
    /// Mounted web views by address, to attribute console messages (which
    /// name their web view) to a tab.
    static WEB_VIEW_TABS: RefCell<HashMap<usize, TabId>> = RefCell::new(HashMap::new());
    /// Popup web views returned to WebKit, waiting for `Effect::AdoptPopup`.
    static PENDING_POPUPS: RefCell<HashMap<TabId, WebViewParts>> = RefCell::new(HashMap::new());
    /// Web views (by address) about to load an offline copy: its navigation
    /// runs with JavaScript off (see `webkit/delegates.rs`).
    static OFFLINE_LOADS: RefCell<std::collections::HashSet<usize>> = RefCell::new(std::collections::HashSet::new());
}

/// Whether `web`'s next main-frame navigation is an offline copy (and
/// forgets it).
pub(crate) fn take_offline_load(web: *const AnyObject) -> bool {
    OFFLINE_LOADS.with(|s| s.borrow_mut().remove(&(web as usize)))
}

/// The tab of a mounted web view, by address.
pub(crate) fn tab_for_web_view(web: *const AnyObject) -> Option<TabId> {
    WEB_VIEW_TABS.with(|m| m.borrow().get(&(web as usize)).copied())
}

/// Whether ⌘ was held for this navigation action (`modifierFlags`, an
/// `NSEventModifierFlags` on macOS).
pub(crate) unsafe fn command_held(action: &objc2_web_kit::WKNavigationAction) -> bool {
    action.modifierFlags().contains(objc2_app_kit::NSEventModifierFlags::Command)
}

/// A main-frame load ended. Nothing to do on macOS (no pull to refresh).
pub(crate) fn load_ended(_web: &WKWebView) {}

/// Creates the popup web view for `tab` with WebKit's configuration, parks
/// it until the core's `AdoptPopup` effect, and returns it to WebKit.
pub(crate) unsafe fn create_popup(
    mtm: MainThreadMarker,
    tab: TabId,
    config: &WKWebViewConfiguration,
    events: &Rc<dyn ContentEvents>,
) -> Retained<WKWebView> {
    let parts = build_web_view(mtm, tab, config, NSSize::new(0.0, 0.0), events);
    let web = parts.web.clone();
    PENDING_POPUPS.with(|p| p.borrow_mut().insert(tab, parts));
    web
}

impl Drop for ContentHost {
    fn drop(&mut self) {
        self.views.clear();
        let center = NSNotificationCenter::defaultCenter();
        for o in &self.observers {
            unsafe { center.removeObserver(AsRef::<AnyObject>::as_ref(&**o)) };
        }
        self.root.removeFromSuperview();
    }
}

/// Gap between the panes of a split, where the chrome draws the divider.
const SPLIT_GAP: f64 = 8.0;

/// A page's appearance never follows the app's (the window's appearance,
/// Settings → General → Appearance): without dark pages mode the view is
/// Aqua over white; with it, Dark Aqua (`prefers-color-scheme`, the default
/// canvas of pages that support a dark scheme) over black. (Until
/// 2026-11-15 a view without the mode followed the window, and pages with
/// no background of their own showed the dark window colour.)
///
/// With night vision (2026-11-17) the colour past the page's edges is
/// drawn as night vision draws white: pure red in sRGB, no green or blue
/// (`viewmodes::NIGHT_CSS` covers the page, not what the view draws past
/// it). Black stays black.
fn set_light(web: &WKWebView, night: bool) {
    let color = if night {
        NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0)
    } else {
        NSColor::whiteColor()
    };
    unsafe { web.setUnderPageBackgroundColor(Some(&color)) };
    let a = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua });
    let _: () = unsafe { msg_send![web, setAppearance: a.as_deref()] };
}

fn set_dark(web: &WKWebView, dark: bool, night: bool) {
    if !dark {
        set_light(web, night);
        return;
    }
    unsafe { web.setUnderPageBackgroundColor(Some(&NSColor::blackColor())) };
    let a = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua });
    let _: () = unsafe { msg_send![web, setAppearance: a.as_deref()] };
}

/// A rectangle from the chrome (window coordinates from the top left),
/// made finite and non-negative.
fn rect((x, y, w, h): (f64, f64, f64, f64)) -> Tl {
    let f = |v: f64| if v.is_finite() { v.clamp(0.0, 100_000.0) } else { 0.0 };
    Tl { x: f(x), y: f(y), w: f(w).max(1.0), h: f(h).max(1.0) }
}

unsafe fn load(web: &WKWebView, url: &Url) {
    if let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url.as_str())) {
        web.loadRequest(&NSURLRequest::requestWithURL(&nsurl));
    }
}

/// Encodes a snapshot as JPEG (Save for Later).
fn thumbnail_jpeg(image: &NSImage) -> Option<Vec<u8>> {
    jpeg_data(image, crate::THUMB_QUALITY)
}

/// Encodes a snapshot as JPEG at `quality` (thumbnails, notebook clips).
fn jpeg_data(image: &NSImage, quality: f64) -> Option<Vec<u8>> {
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImageCompressionFactor};
    let tiff = image.TIFFRepresentation()?;
    let rep = NSBitmapImageRep::imageRepWithData(&tiff)?;
    let quality = objc2_foundation::NSNumber::new_f64(quality);
    let quality: Retained<AnyObject> = Retained::into_super(Retained::into_super(Retained::into_super(quality)));
    let props = objc2_foundation::NSDictionary::from_retained_objects(&[unsafe { NSImageCompressionFactor }], &[quality]);
    unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::JPEG, &props) }.map(|d| d.to_vec())
}

/// Opens a non-web URL (mailto:, tel:, …) in another app after the user
/// confirmed it in the chrome. Web and internal schemes are refused here as
/// a second check; the core never asks for them.
fn open_external(url: &Url) {
    let scheme = url.scheme();
    if matches!(scheme, "http" | "https" | "file" | "javascript" | "data" | "blob" | "about")
        || INTERNAL_SCHEMES.contains(&scheme)
    {
        return;
    }
    if let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url.as_str())) {
        NSWorkspace::sharedWorkspace().openURL(&nsurl);
    }
}

/// Version of the system web engine, as Safari's version. On macOS, WebKit
/// is updated with Safari, also on older macOS versions (Safari 27 runs on
/// macOS 15 and 26), so the installed Safari's version is the engine's;
/// the OS version only if Safari's cannot be read.
pub fn webkit_version() -> OsVersion {
    let read = || -> Option<OsVersion> {
        let bundle = NSBundle::bundleWithPath(&NSString::from_str("/Applications/Safari.app"))?;
        let info = bundle.infoDictionary()?;
        let v = info.objectForKey(&NSString::from_str("CFBundleShortVersionString"))?;
        let s = v.downcast_ref::<NSString>()?.to_string();
        let mut parts = s.split('.').map(|p| p.trim().parse::<u32>().ok());
        let major = parts.next().flatten()?;
        let minor = parts.next().flatten().unwrap_or(0);
        let patch = parts.next().flatten().unwrap_or(0);
        Some(OsVersion::new(major, minor, patch))
    };
    read().unwrap_or_else(os_version)
}
