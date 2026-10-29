//! SECURITY-REVIEW: iOS content host. Creates, lays out, drives and destroys
//! the per-tab `WKWebView`s and applies `snorkle_core::browser::Effect`s.
//!
//! Security assumptions:
//! - Content views are added as subviews of the Tauri window's root view,
//!   *after* (above) the chrome's webview, inside a container that clips to
//!   the area below the chrome toolbar. A page therefore cannot draw over
//!   the address bar; the chrome hides content views (via `HideView`)
//!   whenever it shows anything that overlaps the content area.
//! - Configurations are built only from `CONTENT_VIEW_SPEC`; no user
//!   scripts, script message handlers or URL scheme handlers are installed.
//! - Each `Profile` maps to its own `WKWebsiteDataStore`; private tabs share
//!   one non-persistent store per private session, which is wiped and
//!   released when the last private tab closes.
//! - All methods run on the main thread (`MainThreadMarker` is required).

mod bindings;
mod delegates;
mod injection;
mod rules;

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSDate, NSDictionary, NSSet, NSKeyValueObservingOptions, NSNotification, NSNotificationCenter,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSOperationQueue, NSProcessInfo, NSString, NSURLRequest,
    NSURL, NSUUID,
};
use objc2_ui_kit::{
    UIApplication, UIApplicationDidEnterBackgroundNotification, UIApplicationDidReceiveMemoryWarningNotification,
    UITapGestureRecognizer, UIView, UIViewAutoresizing,
};
use objc2_web_kit::{
    WKContentMode, WKInactiveSchedulingPolicy, WKUserContentController, WKWebsiteDataTypeDiskCache,
    WKWebsiteDataTypeMemoryCache,
    WKWebViewConfiguration, WKWebsiteDataStore,
};
use snorkle_core::browser::{AfterCreate, Effect, Pane};
use snorkle_core::userscripts::Injection;
use snorkle_core::os::OsVersion;
use snorkle_core::policy::INTERNAL_SCHEMES;
use snorkle_core::tabs::{Profile, TabId};
use url::Url;

use self::bindings::WKWebView;
use self::delegates::{NavDelegate, Observer, TabIvars, TapFocus, UiDelegate, KVO_KEYS};
use crate::{clamp_inset, ContentEvents, CONTENT_VIEW_SPEC, DEFAULT_PROFILE_STORE_ID};

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
    _tap: Retained<TapFocus>,
    _tap_recognizer: Retained<UITapGestureRecognizer>,
    _nav: Retained<NavDelegate>,
    _ui: Retained<UiDelegate>,
    kvo: Retained<Observer>,
}

impl Drop for View {
    fn drop(&mut self) {
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
    root: Retained<UIView>,
    top: Cell<f64>,
    left: Cell<f64>,
    views: HashMap<TabId, View>,
    /// `interactionState` of discarded tabs (in memory only).
    saved: HashMap<TabId, Retained<AnyObject>>,
    default_store: Retained<WKWebsiteDataStore>,
    private_store: Option<Retained<WKWebsiteDataStore>>,
    events: Rc<dyn ContentEvents>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    user_agent: Retained<NSString>,
    scripts: injection::Scripts,
}

impl ContentHost {
    /// # Safety
    /// `root_view` must be a valid `UIView*` (the Tauri window's root view,
    /// from `raw_window_handle::UiKitWindowHandle::ui_view`) and this must be
    /// called on the main thread.
    pub unsafe fn new(root_view: *mut c_void, events: Rc<dyn ContentEvents>) -> Self {
        let mtm = MainThreadMarker::new().expect("ContentHost must be created on the main thread");
        let root = Retained::retain(root_view.cast::<UIView>()).expect("root view must not be null");
        let id = NSUUID::from_bytes(DEFAULT_PROFILE_STORE_ID);
        let default_store = WKWebsiteDataStore::dataStoreForIdentifier(&id, mtm);

        let center = NSNotificationCenter::defaultCenter();
        let queue = NSOperationQueue::mainQueue();
        let mut observers = Vec::new();
        let ev = events.clone();
        let mem = RcBlock::new(move |_n: NonNull<NSNotification>| ev.memory_warning());
        observers.push(center.addObserverForName_object_queue_usingBlock(
            Some(UIApplicationDidReceiveMemoryWarningNotification),
            None,
            Some(&queue),
            &mem,
        ));
        let ev = events.clone();
        let bg = RcBlock::new(move |_n: NonNull<NSNotification>| ev.did_enter_background());
        observers.push(center.addObserverForName_object_queue_usingBlock(
            Some(UIApplicationDidEnterBackgroundNotification),
            None,
            Some(&queue),
            &bg,
        ));

        ContentHost {
            mtm,
            root,
            top: Cell::new(0.0),
            left: Cell::new(0.0),
            views: HashMap::new(),
            saved: HashMap::new(),
            default_store,
            private_store: None,
            events,
            observers,
            user_agent: NSString::from_str(&crate::safari_desktop_user_agent(os_version())),
            scripts: injection::Scripts::default(),
        }
    }

    pub fn live_views(&self) -> usize {
        self.views.len()
    }

    fn content_frame(&self) -> CGRect {
        let b = self.root.bounds();
        let top = clamp_inset(self.top.get(), b.size.height);
        let left = clamp_inset(self.left.get(), b.size.width);
        CGRect::new(
            CGPoint::new(left, top),
            CGSize::new((b.size.width - left).max(0.0), (b.size.height - top).max(0.0)),
        )
    }

    /// Frame of one pane: the whole content area, or one half of it with a
    /// 2 pt divider in split view.
    fn pane_frame(&self, pane: Pane) -> CGRect {
        let area = self.content_frame();
        const GAP: f64 = 2.0;
        let half = ((area.size.width - GAP) / 2.0).floor().max(0.0);
        match pane {
            Pane::Full => area,
            Pane::Left => CGRect::new(area.origin, CGSize::new(half, area.size.height)),
            Pane::Right => CGRect::new(
                CGPoint::new(area.origin.x + area.size.width - half, area.origin.y),
                CGSize::new(half, area.size.height),
            ),
        }
    }

    /// Insets of the content area, in points, reported by the trusted
    /// chrome UI: `top` is the bottom edge of the toolbar, `left` the right
    /// edge of the docked tab sidebar (0 when collapsed). Content containers
    /// never extend over either.
    pub fn set_insets(&mut self, top: f64, left: f64) {
        self.top.set(top);
        self.left.set(left);
        for v in self.views.values() {
            v.container.setFrame(self.pane_frame(v.pane.get()));
        }
    }

    /// Replaces the injected scripts (user scripts, black-and-white mode) in
    /// every content view; they apply from the next load. `run_now`, if
    /// given, is run once in every loaded page (used to toggle
    /// black-and-white without reloading).
    pub fn set_injections(&mut self, injections: &[Injection], run_now: Option<&str>) {
        self.scripts = injection::make_scripts(self.mtm, injections);
        for v in self.views.values() {
            injection::install(&v.controller, &self.scripts);
            if let Some(js) = run_now {
                injection::run_now(self.mtm, &v.web, js);
            }
        }
    }

    /// Empties the HTTP disk and memory caches of every profile's store.
    /// Cookies, storage and logins are kept.
    pub fn clear_cache(&mut self) {
        unsafe {
            let types = NSSet::from_slice(&[WKWebsiteDataTypeDiskCache, WKWebsiteDataTypeMemoryCache]);
            for store in std::iter::once(&self.default_store).chain(self.private_store.as_ref()) {
                let done = RcBlock::new(|| {});
                store.removeDataOfTypes_modifiedSince_completionHandler(&types, &NSDate::distantPast(), &done);
            }
        }
    }

    pub fn apply(&mut self, effect: &Effect) {
        unsafe {
            match effect {
                Effect::CreateView { tab, profile, then } => {
                    if self.views.contains_key(tab) {
                        return;
                    }
                    let view = self.create_view(*tab, *profile);
                    match then {
                        AfterCreate::Load(url) => load(&view.web, url),
                        AfterCreate::Restore { fallback } => match self.saved.remove(tab) {
                            Some(state) => view.web.setInteractionState(Some(&state)),
                            None => {
                                if let Some(url) = fallback {
                                    load(&view.web, url)
                                }
                            }
                        },
                    }
                    self.views.insert(*tab, view);
                }
                Effect::DestroyView { tab, keep_state } => {
                    if let Some(v) = self.views.remove(tab) {
                        if *keep_state {
                            if let Some(state) = v.web.interactionState() {
                                self.saved.insert(*tab, state);
                            }
                        }
                        rules::unregister(*tab);
                        drop(v);
                    }
                    if !*keep_state {
                        self.saved.remove(tab);
                    }
                }
                Effect::ShowView { tab, pane } => {
                    if let Some(v) = self.views.get(tab) {
                        v.pane.set(*pane);
                        v.container.setFrame(self.pane_frame(*pane));
                        self.root.bringSubviewToFront(&v.container);
                        v.container.setHidden(false);
                    }
                }
                Effect::HideView(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.container.setHidden(true);
                    }
                }
                Effect::Load { tab, url } => {
                    if let Some(v) = self.views.get(tab) {
                        load(&v.web, url);
                    }
                }
                Effect::GoBack(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.web.goBack();
                    }
                }
                Effect::GoForward(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.web.goForward();
                    }
                }
                Effect::Reload(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.web.reload();
                    }
                }
                Effect::Stop(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.web.stopLoading();
                    }
                }
                Effect::ReleasePrivateData => self.release_private_store(),
                Effect::OpenExternal(url) => open_external(self.mtm, url),
                // Handled by the app layer, not the content host.
                Effect::RecordHistory { .. } | Effect::PersistSession(_) | Effect::NotifyChrome => {}
            }
        }
    }

    unsafe fn store_for(&mut self, profile: Profile) -> Retained<WKWebsiteDataStore> {
        match profile {
            Profile::Default => self.default_store.clone(),
            Profile::Private => {
                self.private_store.get_or_insert_with(|| WKWebsiteDataStore::nonPersistentDataStore(self.mtm)).clone()
            }
        }
    }

    unsafe fn release_private_store(&mut self) {
        // Saved interaction states of private tabs were dropped with their
        // tabs (DestroyView keep_state=false). Wipe the store explicitly
        // before releasing it, in case WebKit keeps it alive internally.
        if let Some(store) = self.private_store.take() {
            let types = WKWebsiteDataStore::allWebsiteDataTypes(self.mtm);
            let done = RcBlock::new(|| {});
            store.removeDataOfTypes_modifiedSince_completionHandler(&types, &NSDate::distantPast(), &done);
        }
    }

    unsafe fn create_view(&mut self, tab: TabId, profile: Profile) -> View {
        let mtm = self.mtm;
        let spec = &CONTENT_VIEW_SPEC;
        let config = WKWebViewConfiguration::new(mtm);
        config.setWebsiteDataStore(&self.store_for(profile));

        let prefs = config.preferences();
        prefs.setJavaScriptCanOpenWindowsAutomatically(spec.javascript_can_open_windows_automatically);
        prefs.setFraudulentWebsiteWarningEnabled(spec.fraudulent_website_warning);
        if prefs.respondsToSelector(sel!(setElementFullscreenEnabled:)) {
            prefs.setElementFullscreenEnabled(spec.element_fullscreen);
        }
        if prefs.respondsToSelector(sel!(setInactiveSchedulingPolicy:)) {
            prefs.setInactiveSchedulingPolicy(WKInactiveSchedulingPolicy::Suspend);
        }
        if config.respondsToSelector(sel!(setUpgradeKnownHostsToHTTPS:)) {
            config.setUpgradeKnownHostsToHTTPS(spec.upgrade_known_hosts_to_https);
        }
        if config.respondsToSelector(sel!(setLimitsNavigationsToAppBoundDomains:)) {
            config.setLimitsNavigationsToAppBoundDomains(spec.limits_navigations_to_app_bound_domains);
        }
        // iOS-only property, absent from the macOS-generated bindings.
        if config.respondsToSelector(sel!(setAllowsInlineMediaPlayback:)) {
            let _: () = msg_send![&*config, setAllowsInlineMediaPlayback: true];
        }

        if spec.safari_desktop_user_agent {
            // Desktop-class content to match the desktop UA set below
            // (Safari's default on iPad, except in narrow multitasking).
            config.defaultWebpagePreferences().setPreferredContentMode(WKContentMode::Desktop);
        }

        let controller = config.userContentController();

        let frame = self.content_frame();
        let container = UIView::initWithFrame(UIView::alloc(mtm), frame);
        container.setClipsToBounds(true);
        container.setAutoresizingMask(UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight);
        container.setHidden(true);

        let web_frame = CGRect::new(CGPoint::new(0.0, 0.0), frame.size);
        let web = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), web_frame, &config);
        web.setAutoresizingMask(UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight);
        if web.respondsToSelector(sel!(setInspectable:)) {
            web.setInspectable(spec.inspectable);
        }
        web.setAllowsBackForwardNavigationGestures(spec.allows_back_forward_gestures);
        web.setAllowsLinkPreview(spec.allows_link_preview);
        if spec.safari_desktop_user_agent {
            web.setCustomUserAgent(Some(&self.user_agent));
        }

        let nav = NavDelegate::new(mtm, TabIvars::new(tab, self.events.clone()));
        let ui = UiDelegate::new(mtm, TabIvars::new(tab, self.events.clone()));
        let kvo = Observer::new(mtm, TabIvars::new(tab, self.events.clone()));
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

        container.addSubview(&web);
        self.root.addSubview(&container);
        injection::install(&controller, &self.scripts);
        rules::register(tab, controller.clone());

        let tap = TapFocus::new(mtm, TabIvars::new(tab, self.events.clone()));
        let recognizer = UITapGestureRecognizer::initWithTarget_action(
            UITapGestureRecognizer::alloc(mtm),
            Some(&tap),
            Some(sel!(tapped:)),
        );
        recognizer.setCancelsTouchesInView(false);
        recognizer.setDelegate(Some(ProtocolObject::from_ref(&*tap)));
        container.addGestureRecognizer(&recognizer);

        View {
            container,
            web,
            controller,
            pane: Cell::new(Pane::Full),
            _tap: tap,
            _tap_recognizer: recognizer,
            _nav: nav,
            _ui: ui,
            kvo,
        }
    }

    /// Installs content-blocking rule lists for `key` into every content
    /// web view (now and future), reusing WebKit's compiled cache when it
    /// has a complete set for `key`; otherwise `make` runs on a background
    /// thread and WebKit compiles its output. `done` runs on the main thread.
    pub fn install_rules(&mut self, key: String, make: crate::MakeRuleLists, done: crate::RulesDone) {
        rules::install(self.mtm, key, make, done);
    }
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

/// Schedules `f` on the main queue. Always asynchronous, even when called
/// from the main thread, so effects are never applied re-entrantly from
/// inside a WebKit delegate callback.
pub fn dispatch_main(f: impl FnOnce() + Send + 'static) {
    let cell = Cell::new(Some(f));
    let block = RcBlock::new(move || {
        if let Some(f) = cell.take() {
            f()
        }
    });
    unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
}

pub fn physical_memory() -> u64 {
    NSProcessInfo::processInfo().physicalMemory()
}

pub fn os_version() -> OsVersion {
    let v = NSProcessInfo::processInfo().operatingSystemVersion();
    let c = |x: isize| u32::try_from(x).unwrap_or(0);
    OsVersion::new(c(v.majorVersion), c(v.minorVersion), c(v.patchVersion))
}
