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

use std::cell::{Cell, RefCell};
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
    NSDate, NSDictionary, NSError, NSKeyValueObservingOptions, NSNotification, NSNotificationCenter,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSOperationQueue, NSProcessInfo, NSString, NSURLRequest,
    NSURL, NSUUID,
};
use objc2_ui_kit::{
    UIApplication, UIApplicationDidEnterBackgroundNotification, UIApplicationDidReceiveMemoryWarningNotification,
    UIView, UIViewAutoresizing,
};
use objc2_web_kit::{
    WKContentRuleList, WKContentRuleListStore, WKInactiveSchedulingPolicy, WKUserContentController,
    WKWebViewConfiguration, WKWebsiteDataStore,
};
use snorkle_core::browser::{AfterCreate, Effect};
use snorkle_core::os::OsVersion;
use snorkle_core::policy::INTERNAL_SCHEMES;
use snorkle_core::tabs::{Profile, TabId};
use url::Url;

use self::bindings::WKWebView;
use self::delegates::{NavDelegate, Observer, TabIvars, UiDelegate, KVO_KEYS};
use crate::{clamp_top, ContentEvents, CONTENT_VIEW_SPEC, DEFAULT_PROFILE_STORE_ID, RULE_LIST_ID};

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
    views: HashMap<TabId, View>,
    /// `interactionState` of discarded tabs (in memory only).
    saved: HashMap<TabId, Retained<AnyObject>>,
    default_store: Retained<WKWebsiteDataStore>,
    private_store: Option<Retained<WKWebsiteDataStore>>,
    rules: Rc<RefCell<Option<Retained<WKContentRuleList>>>>,
    controllers: Rc<RefCell<HashMap<TabId, Retained<WKUserContentController>>>>,
    events: Rc<dyn ContentEvents>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
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
            views: HashMap::new(),
            saved: HashMap::new(),
            default_store,
            private_store: None,
            rules: Rc::new(RefCell::new(None)),
            controllers: Rc::new(RefCell::new(HashMap::new())),
            events,
            observers,
        }
    }

    pub fn live_views(&self) -> usize {
        self.views.len()
    }

    fn content_frame(&self) -> CGRect {
        let b = self.root.bounds();
        let top = clamp_top(self.top.get(), b.size.height);
        CGRect::new(CGPoint::new(0.0, top), CGSize::new(b.size.width, (b.size.height - top).max(0.0)))
    }

    /// Top inset of the content area (bottom edge of the chrome toolbar), in
    /// points. Reported by the trusted chrome UI.
    pub fn set_top(&mut self, top: f64) {
        self.top.set(top);
        let frame = self.content_frame();
        for v in self.views.values() {
            v.container.setFrame(frame);
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
                        self.controllers.borrow_mut().remove(tab);
                        drop(v);
                    }
                    if !*keep_state {
                        self.saved.remove(tab);
                    }
                }
                Effect::ShowView(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.container.setFrame(self.content_frame());
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

        let controller = config.userContentController();
        if let Some(list) = self.rules.borrow().as_ref() {
            controller.addContentRuleList(list);
        }

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
        self.controllers.borrow_mut().insert(tab, controller.clone());
        View { container, web, controller, _nav: nav, _ui: ui, kvo }
    }

    /// Compiles and installs the content-blocking rule list (`None` removes
    /// it). Compilation runs inside WebKit, off the main thread; the
    /// completion handler attaches the list to every live web view.
    pub fn install_rules(&mut self, json: Option<String>) {
        unsafe {
            let Some(json) = json else {
                *self.rules.borrow_mut() = None;
                for v in self.views.values() {
                    v.controller.removeAllContentRuleLists();
                }
                return;
            };
            let Some(store) = WKContentRuleListStore::defaultStore(self.mtm) else { return };
            let rules = self.rules.clone();
            let controllers = self.controllers.clone();
            let done = RcBlock::new(move |list: *mut WKContentRuleList, err: *mut NSError| {
                if !err.is_null() {
                    return;
                }
                let Some(list) = Retained::retain(list) else { return };
                for c in controllers.borrow().values() {
                    c.removeAllContentRuleLists();
                    c.addContentRuleList(&list);
                }
                *rules.borrow_mut() = Some(list);
            });
            store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
                Some(&NSString::from_str(RULE_LIST_ID)),
                Some(&NSString::from_str(&json)),
                Some(&done),
            );
        }
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
