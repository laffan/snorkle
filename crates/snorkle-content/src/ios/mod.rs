//! SECURITY-REVIEW: iOS content host. Creates, lays out, drives and destroys
//! the per-tab `WKWebView`s and applies `snorkle_core::browser::Effect`s.
//!
//! Security assumptions:
//! - Content views are added as subviews of the Tauri window's root view,
//!   *after* (above) the chrome's webview, inside a container that clips to
//!   the content area the chrome reports (outside its sidebar and docked
//!   panels). The URL pill floats over the content area: the chrome reports
//!   its rectangle (and those of its popups and toasts) as holes, and every
//!   container and the sheet backdrop are masked there and do not take
//!   touches there (`keys.rs`), so the chrome underneath shows through and
//!   a page cannot draw over the address bar. Anything else the chrome
//!   shows over the content area hides content views (via `HideView`).
//! - Configurations are built only from `CONTENT_VIEW_SPEC`; no URL scheme
//!   handlers are installed. User scripts and the console handler (only
//!   while console capture is on) are installed by `injection.rs`.
//! - A popup sheet (`Pane::Sheet`) is centred in the content area over a
//!   dimming backdrop that swallows touches, so the page underneath keeps
//!   running but cannot be used until the sheet closes. Both stay below the
//!   toolbar, which shows the sheet's URL.
//! - Each `Profile` maps to its own `WKWebsiteDataStore`; private tabs share
//!   one non-persistent store per private session, which is wiped and
//!   released when the last private tab closes.
//! - All methods run on the main thread (`MainThreadMarker` is required).

mod bindings;
mod favicon;
mod gestures;
mod keys;
mod menu;
mod effects;
mod layout;
mod pages;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_core_foundation::{CGAffineTransform, CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSArray, NSDictionary, NSKeyValueObservingOptions, NSNotification, NSNotificationCenter,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSOperationQueue, NSString,
    NSURLRequest, NSURL, NSUUID, NSData, NSError,
};
use objc2_core_graphics::CGPath;
use objc2_quartz_core::{
    kCAFillRuleEvenOdd, kCAMediaTimingFunctionEaseInEaseOut, CABasicAnimation, CACurrentMediaTime,
    CAMediaTiming, CAMediaTimingFunction, CAShapeLayer, CATransaction,
};
use objc2_ui_kit::{
    UIActivityViewController, UIApplication, UIApplicationDidBecomeActiveNotification, UIApplicationDidEnterBackgroundNotification,
    UIApplicationDidReceiveMemoryWarningNotification, UIBezierPath, UIColor, UIControlEvents, UIRectCorner,
    UIReferenceLibraryViewController, UIRefreshControl, UITapGestureRecognizer, UIView, UIViewAutoresizing,
    UIUserInterfaceStyle, UIViewController, UIDocumentPickerViewController, UIImage, UIPasteboard,
    UIHoverGestureRecognizer,
};
use objc2_web_kit::{WKUserContentController, WKWebViewConfiguration, WKWebsiteDataStore};
use snorkle_core::browser::{AfterCreate, Effect, Pane};
use snorkle_core::os::OsVersion;
use snorkle_core::policy::INTERNAL_SCHEMES;
use snorkle_core::tabs::{Profile, TabId};
use snorkle_core::userscripts::Injection;
use url::Url;

pub(crate) use self::bindings::WKWebView;
use self::gestures::{HoverEdge, HoverIvars, RefreshTarget, ScrollProbe, TapFocus};
use crate::webkit::delegates::{NavDelegate, Observer, TabIvars, UiDelegate, KVO_KEYS};
use crate::webkit::{injection, rules, stores};
use crate::{clamp_inset, ContentEvents, DataKind, Hole, CONTENT_VIEW_SPEC, DEFAULT_PROFILE_STORE_ID};

// Adding any page-to-native channel requires writing installation code
// here, which this assertion forces to be a deliberate, reviewed change.
const _: () = assert!(
    CONTENT_VIEW_SPEC.user_scripts.is_empty()
        && CONTENT_VIEW_SPEC.script_message_handlers.is_empty()
        && CONTENT_VIEW_SPEC.url_scheme_handlers.is_empty()
);

struct View {
    container: Retained<UIView>,
    web: Retained<WKWebView>,
    controller: Retained<WKUserContentController>,
    pane: Cell<Pane>,
    /// Page scale (⋯ menu): the web view is laid out at its pane's size
    /// divided by this and drawn scaled by it, so a scale below 1 gives the
    /// page a wider viewport (more CSS pixels) with everything smaller.
    scale: Cell<f64>,
    _tap: Retained<TapFocus>,
    _tap_recognizer: Retained<UITapGestureRecognizer>,
    _refresh: Retained<RefreshTarget>,
    /// Attached to the page's scroll view only while pull to refresh is on.
    refresh_control: Retained<UIRefreshControl>,
    /// Debug builds: observes the page's scroll pan (`ScrollProbe`), with
    /// the recognizer it was added to (the target is removed on drop).
    probe: Option<(Retained<ScrollProbe>, Retained<objc2_ui_kit::UIGestureRecognizer>)>,
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
            if let Some((probe, pan)) = &self.probe {
                pan.removeTarget_action(Some(probe), Some(sel!(panned:)));
            }
        }
        self.container.removeFromSuperview();
    }
}

pub struct ContentHost {
    mtm: MainThreadMarker,
    root: Retained<UIView>,
    top: Cell<f64>,
    left: Cell<f64>,
    right: Cell<f64>,
    bottom: Cell<f64>,
    /// The first pane's share of a split (`Effect::SetSplitRatio`), 0.2–0.8.
    split_ratio: Cell<f64>,
    /// Where the chrome's History dialog shows its preview (`Pane::Preview`,
    /// `set_preview_frame`): window points from the top left.
    preview: Cell<Option<(f64, f64, f64, f64)>>,
    views: HashMap<TabId, View>,
    /// `interactionState` of discarded tabs (in memory only).
    saved: HashMap<TabId, Retained<AnyObject>>,
    default_store: Retained<WKWebsiteDataStore>,
    private_store: Option<Retained<WKWebsiteDataStore>>,
    events: Rc<dyn ContentEvents>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    /// Compiled scripts for tabs without view-mode overrides, and per tab
    /// for the rest.
    scripts: injection::Scripts,
    tab_scripts: HashMap<TabId, injection::Scripts>,
    injections: crate::Injections,
    /// Page console capture (channel C13); off unless the user turns it on.
    console: bool,
    /// Dims the content area under a popup sheet and swallows its touches.
    backdrop: Retained<UIView>,
    /// Target of the hover recognizer on the root view (edge tab).
    _hover: Retained<HoverEdge>,
    /// Where the chrome floats over the content area (window coordinates).
    holes: keys::Holes,
    /// The chrome's own web view (Tauri's), to give it keyboard focus.
    /// The chrome's web view (shared with the did-become-active observer).
    chrome: Rc<RefCell<Option<Retained<UIView>>>>,
}

/// No status bar (owner request 2026-11-23: the sidebar and pages reach
/// the top of the window). The window's root view controller is tao's,
/// which keeps the preference (`setPrefersStatusBarHidden:`, as tao's
/// `WindowExtIOS::set_prefers_status_bar_hidden`); asked again when the app
/// becomes active, in case the window was not set up yet at launch.
fn hide_status_bar(root: &UIView) {
    let Some(vc) = root.window().and_then(|w| w.rootViewController()) else { return };
    if vc.respondsToSelector(sel!(setPrefersStatusBarHidden:)) {
        let _: () = unsafe { msg_send![&*vc, setPrefersStatusBarHidden: Bool::YES] };
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
/// delegates and observers. Used both for our own configurations and for
/// the configuration WebKit passes to `createWebViewWithConfiguration`
/// (popups), which already carries the opener's data store and content
/// controller (and therefore its rule lists and injected scripts).
pub(crate) unsafe fn build_web_view(
    mtm: MainThreadMarker,
    tab: TabId,
    config: &WKWebViewConfiguration,
    frame: CGRect,
    events: &Rc<dyn ContentEvents>,
) -> WebViewParts {
    let spec = &CONTENT_VIEW_SPEC;
    let web = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, config);
    web.setAutoresizingMask(UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight);
    if web.respondsToSelector(sel!(setInspectable:)) {
        web.setInspectable(spec.inspectable);
    }
    web.setAllowsBackForwardNavigationGestures(spec.allows_back_forward_gestures && crate::swipe_navigation());
    web.setAllowsLinkPreview(spec.allows_link_preview);
    if spec.safari_desktop_user_agent {
        web.setCustomUserAgent(Some(&NSString::from_str(&crate::safari_desktop_user_agent(os_version()))));
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
    /// runs with JavaScript off (see `delegates.rs`).
    static OFFLINE_LOADS: RefCell<std::collections::HashSet<usize>> = RefCell::new(std::collections::HashSet::new());
}

/// Whether ⌘ was held for this navigation action (`modifierFlags`, a
/// `UIKeyModifierFlags` on iOS 18.4+; absent before, then never).
pub(crate) unsafe fn command_held(action: &objc2_web_kit::WKNavigationAction) -> bool {
    if !action.respondsToSelector(sel!(modifierFlags)) {
        return false;
    }
    let flags: objc2_ui_kit::UIKeyModifierFlags = msg_send![action, modifierFlags];
    flags.contains(objc2_ui_kit::UIKeyModifierFlags::Command)
}

/// A main-frame load ended (finished or failed): stops the pull-to-refresh
/// spinner if it started it.
pub(crate) fn load_ended(web: &WKWebView) {
    gestures::end_refreshing(web);
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

/// Creates the popup web view for `tab` with WebKit's configuration, parks
/// it until the core's `AdoptPopup` effect, and returns it to WebKit.
pub(crate) unsafe fn create_popup(
    mtm: MainThreadMarker,
    tab: TabId,
    config: &WKWebViewConfiguration,
    events: &Rc<dyn ContentEvents>,
) -> Retained<WKWebView> {
    let parts = build_web_view(mtm, tab, config, CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0)), events);
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
    }
}

/// Gap between the panes of a split, where the chrome draws the divider.
const SPLIT_GAP: f64 = 8.0;

/// Sets a container's frame and its web view's, and cuts `holes` out.
/// The web view gets the container's size divided by `scale` and a scale
/// transform, so it fills the container exactly (bounds and center, since
/// `frame` is undefined on a transformed view).
fn place_view(view: &UIView, frame: CGRect, holes: &[Hole], glide: Option<f64>, scale: f64) {
    view.setFrame(frame);
    let b = view.bounds();
    let s = if scale.is_finite() && (0.25..=4.0).contains(&scale) { scale } else { 1.0 };
    for sub in view.subviews().iter() {
        // Autoresizing works on frames: only while unscaled.
        sub.setAutoresizingMask(if s == 1.0 {
            UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight
        } else {
            UIViewAutoresizing::None
        });
        sub.setTransform(CGAffineTransform { a: s, b: 0.0, c: 0.0, d: s, tx: 0.0, ty: 0.0 });
        sub.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(b.size.width / s, b.size.height / s)));
        sub.setCenter(CGPoint::new(b.size.width / 2.0, b.size.height / 2.0));
    }
    apply_mask(view, holes, glide);
}

/// A frame as `(x, y, width, height)`, for `crate::holes_over`.
fn rect_tuple(r: CGRect) -> (f64, f64, f64, f64) {
    (r.origin.x, r.origin.y, r.size.width, r.size.height)
}

/// Masks `view` (a direct subview of the root view) so the holes are not
/// drawn; no mask when there are none. Holes never overlap
/// (`sanitize_holes` merges them), as the even-odd fill requires.
///
/// With `glide` (seconds since the chrome started its 0.25 s CSS slide),
/// the mask's path animates from the old holes to the new ones with an
/// explicit animation, ease-in-out, begun that long ago so it lines up with
/// the chrome's slide despite the IPC delay. (An implicit layer action is
/// not used: inside a `UIView` animation block it may be suppressed, and
/// then the cut-outs jump while the sidebar slides.) Callers glide only
/// when the number of holes is unchanged, so both paths have the same shape.
fn apply_mask(view: &UIView, holes: &[Hole], glide: Option<f64>) {
    let f = view.frame();
    let b = view.bounds();
    let layer = view.layer();
    if holes.is_empty() {
        unsafe { layer.setMask(None) };
        return;
    }
    let path = UIBezierPath::bezierPathWithRect(b);
    for h in holes {
        let rect = CGRect::new(CGPoint::new(h.x - f.origin.x, h.y - f.origin.y), CGSize::new(h.w, h.h));
        let hole = if h.corners == crate::ALL_CORNERS {
            UIBezierPath::bezierPathWithRoundedRect_cornerRadius(rect, h.r)
        } else {
            UIBezierPath::bezierPathWithRoundedRect_byRoundingCorners_cornerRadii(
                rect,
                UIRectCorner(h.corners as usize),
                CGSize::new(h.r, h.r),
            )
        };
        path.appendPath(&hole);
    }
    let new_path = path.CGPath();
    let existing = layer.mask().and_then(|m| m.downcast::<CAShapeLayer>().ok());
    let old_path = existing.as_ref().and_then(|m| m.path());
    let mask = existing.unwrap_or_else(CAShapeLayer::layer);
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    mask.setFrame(b);
    mask.setPath(Some(&new_path));
    unsafe {
        mask.setFillRule(kCAFillRuleEvenOdd);
        layer.setMask(Some(&mask));
    }
    CATransaction::commit();
    if let (Some(lag), Some(old)) = (glide, old_path) {
        let anim = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("path")));
        // CGPath is toll-free bridged: a valid Objective-C object.
        let as_obj = |p: &CGPath| unsafe { &*(p as *const CGPath as *const AnyObject) };
        unsafe {
            anim.setFromValue(Some(as_obj(&old)));
            anim.setToValue(Some(as_obj(&new_path)));
        }
        anim.setDuration(GLIDE_SECONDS);
        anim.setBeginTime(CACurrentMediaTime() - lag.clamp(0.0, GLIDE_SECONDS));
        anim.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(unsafe {
            kCAMediaTimingFunctionEaseInEaseOut
        })));
        mask.addAnimation_forKey(&anim, Some(&NSString::from_str("snorkleGlide")));
    }
}

/// Length of the chrome's sidebar slide (CSS `.25s ease-in-out`).
const GLIDE_SECONDS: f64 = 0.25;

/// A page's appearance never follows the app's (Settings → General →
/// Appearance, the window's interface style): without dark pages mode the
/// view is light with a white canvas; with it, it prefers dark
/// (`prefers-color-scheme`, the default canvas of pages that support a
/// dark scheme) over a black canvas, so sites with a dark theme use it.
/// (Until 2026-11-15 a view without the mode followed the window, so with
/// the app dark, pages declaring both schemes turned dark and pages with
/// no background of their own were drawn on the system's black.)
///
/// With night vision (2026-11-17) the canvas is drawn as night vision
/// draws white: pure red, no green or blue (`viewmodes::NIGHT_CSS` covers
/// the page but not the view's own canvas, which shows before a page
/// paints and past its edges when it is pulled). Black stays black.
fn set_light(web: &WKWebView, night: bool) {
    let color = if night { UIColor::colorWithRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0) } else { UIColor::whiteColor() };
    web.setOpaque(false);
    web.setBackgroundColor(Some(&color));
    unsafe { web.scrollView() }.setBackgroundColor(Some(&color));
    web.setOverrideUserInterfaceStyle(UIUserInterfaceStyle::Light);
}

fn set_dark(web: &WKWebView, dark: bool, night: bool) {
    if !dark {
        set_light(web, night);
        return;
    }
    // Set the native canvas before navigation can create its first document.
    let color = UIColor::blackColor();
    web.setOpaque(false);
    web.setBackgroundColor(Some(&color));
    unsafe { web.scrollView() }.setBackgroundColor(Some(&color));
    web.setOverrideUserInterfaceStyle(UIUserInterfaceStyle::Dark);
}

/// A popover source rectangle from the chrome (window coordinates), made
/// finite and non-negative.
fn rect((x, y, w, h): (f64, f64, f64, f64)) -> CGRect {
    let f = |v: f64| if v.is_finite() { v.clamp(0.0, 100_000.0) } else { 0.0 };
    CGRect::new(CGPoint::new(f(x), f(y)), CGSize::new(f(w).max(1.0), f(h).max(1.0)))
}

/// Rounds a container's corners (it already clips to its bounds).
fn set_corner_radius(view: &UIView, radius: f64) {
    unsafe {
        let layer: Retained<AnyObject> = msg_send![view, layer];
        let _: () = msg_send![&*layer, setCornerRadius: radius];
    }
}

unsafe fn load(web: &WKWebView, url: &Url) {
    if let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url.as_str())) {
        web.loadRequest(&NSURLRequest::requestWithURL(&nsurl));
    }
}

/// Opens a non-web URL (mailto:, tel:, …) in another app after the user
/// confirmed it in the chrome. Web and internal schemes are refused here as
/// a second check; the core never asks for them.
unsafe fn open_external(mtm: MainThreadMarker, url: &Url) {
    let scheme = url.scheme();
    if matches!(scheme, "http" | "https" | "file" | "javascript" | "data" | "blob" | "about")
        || INTERNAL_SCHEMES.contains(&scheme)
    {
        return;
    }
    if let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url.as_str())) {
        let app = UIApplication::sharedApplication(mtm);
        app.openURL_options_completionHandler(&nsurl, &NSDictionary::new(), None);
    }
}

pub use crate::webkit::{dispatch_main, os_version, physical_memory};


/// Version of the system web engine, as Safari's version (sites read it
/// from the user agent): on iPadOS, WebKit ships with the OS, so this is
/// the OS version.
pub fn webkit_version() -> OsVersion {
    os_version()
}
