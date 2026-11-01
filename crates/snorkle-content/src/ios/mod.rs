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
mod delegates;
mod favicon;
pub mod http;
mod injection;
pub mod keychain;
mod keys;
mod menu;
mod rules;

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
    NSDate, NSDictionary, NSKeyValueObservingOptions, NSNotification, NSNotificationCenter,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSOperationQueue, NSProcessInfo, NSSet, NSString,
    NSURLRequest, NSURL, NSUUID,
};
use objc2_quartz_core::{kCAFillRuleEvenOdd, CAShapeLayer};
use objc2_ui_kit::{
    UIApplication, UIApplicationDidEnterBackgroundNotification, UIApplicationDidReceiveMemoryWarningNotification,
    UIBezierPath, UIColor, UIControlEvents, UIRefreshControl, UITapGestureRecognizer, UIView, UIViewAutoresizing,
};
use objc2_web_kit::{
    WKContentMode, WKInactiveSchedulingPolicy, WKUserContentController, WKWebViewConfiguration, WKWebsiteDataStore,
    WKWebsiteDataTypeCookies, WKWebsiteDataTypeDiskCache, WKWebsiteDataTypeFetchCache, WKWebsiteDataTypeFileSystem,
    WKWebsiteDataTypeIndexedDBDatabases, WKWebsiteDataTypeLocalStorage, WKWebsiteDataTypeMediaKeys,
    WKWebsiteDataTypeMemoryCache, WKWebsiteDataTypeOfflineWebApplicationCache,
    WKWebsiteDataTypeServiceWorkerRegistrations, WKWebsiteDataTypeSessionStorage, WKWebsiteDataTypeWebSQLDatabases,
};
use snorkle_core::browser::{AfterCreate, Effect, Pane};
use snorkle_core::os::OsVersion;
use snorkle_core::policy::INTERNAL_SCHEMES;
use snorkle_core::tabs::{Profile, TabId};
use snorkle_core::userscripts::Injection;
use url::Url;

use self::bindings::WKWebView;
use self::delegates::{NavDelegate, Observer, RefreshTarget, TabIvars, TapFocus, UiDelegate, KVO_KEYS};
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
    _tap: Retained<TapFocus>,
    _tap_recognizer: Retained<UITapGestureRecognizer>,
    _refresh: Retained<RefreshTarget>,
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
    root: Retained<UIView>,
    top: Cell<f64>,
    left: Cell<f64>,
    right: Cell<f64>,
    bottom: Cell<f64>,
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
    /// Where the chrome floats over the content area (window coordinates).
    holes: keys::Holes,
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

        menu::install(mtm, events.clone());
        let holes = keys::Holes::default();
        // A container too, so it also leaves the holes (and their touches) out.
        let backdrop = keys::KeyContainer::new_view(mtm, CGRect::ZERO, events.clone(), holes.clone());
        backdrop.setBackgroundColor(Some(&UIColor::colorWithWhite_alpha(0.0, 0.45)));
        backdrop.setHidden(true);
        root.addSubview(&backdrop);

        ContentHost {
            mtm,
            backdrop,
            holes,
            root,
            top: Cell::new(0.0),
            left: Cell::new(0.0),
            right: Cell::new(0.0),
            bottom: Cell::new(0.0),
            views: HashMap::new(),
            saved: HashMap::new(),
            default_store,
            private_store: None,
            events,
            observers,
            scripts: injection::Scripts::default(),
            tab_scripts: HashMap::new(),
            injections: crate::Injections::default(),
            console: false,
        }
    }

    pub fn live_views(&self) -> usize {
        self.views.len()
    }

    fn content_frame(&self) -> CGRect {
        let b = self.root.bounds();
        let top = clamp_inset(self.top.get(), b.size.height);
        let left = clamp_inset(self.left.get(), b.size.width);
        let right = clamp_inset(self.right.get(), b.size.width);
        let bottom = clamp_inset(self.bottom.get(), b.size.height);
        CGRect::new(
            CGPoint::new(left, top),
            CGSize::new((b.size.width - left - right).max(0.0), (b.size.height - top - bottom).max(0.0)),
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
            // Window-sized, centred, with a margin that leaves the page
            // underneath visible.
            Pane::Sheet => {
                const MARGIN: f64 = 16.0;
                let w = (area.size.width - 2.0 * MARGIN).clamp(0.0, 620.0);
                let h = (area.size.height - 2.0 * MARGIN).clamp(0.0, 820.0);
                CGRect::new(
                    CGPoint::new(
                        (area.origin.x + (area.size.width - w) / 2.0).floor(),
                        (area.origin.y + (area.size.height - h) / 2.0).floor(),
                    ),
                    CGSize::new(w, h),
                )
            }
        }
    }

    /// Shows the backdrop under the visible sheet, if any, and keeps the
    /// sheet above it.
    fn sync_backdrop(&self) {
        let sheet = self.views.values().find(|v| v.pane.get() == Pane::Sheet && !v.container.isHidden());
        match sheet {
            Some(v) => {
                self.place(&self.backdrop, self.content_frame());
                self.root.bringSubviewToFront(&self.backdrop);
                self.root.bringSubviewToFront(&v.container);
                self.backdrop.setHidden(false);
            }
            None => self.backdrop.setHidden(true),
        }
    }

    /// Insets of the content area, in points, reported by the trusted
    /// chrome UI: `top` is the bottom edge of the toolbar, `left` the right
    /// edge of the docked tab sidebar (0 when collapsed), `right` the width
    /// of a docked right panel (site scripts, headers), `bottom` the height
    /// of the docked console (0 when closed). Content containers never
    /// extend over any of them. `holes` are where the chrome floats over
    /// the content area (see the module docs).
    pub fn set_insets(&mut self, top: f64, left: f64, right: f64, bottom: f64, holes: Vec<Hole>) {
        let before = self.content_frame().size.width;
        self.top.set(top);
        self.left.set(left);
        self.right.set(right);
        self.bottom.set(bottom);
        let b = self.root.bounds();
        *self.holes.borrow_mut() = crate::sanitize_holes(&holes, b.size.width, b.size.height);
        let grew = self.content_frame().size.width > before + 0.5;
        for v in self.views.values() {
            self.place(&v.container, self.pane_frame(v.pane.get()));
            if grew {
                // WebKit re-lays out the page asynchronously; check once it has.
                unsafe {
                    let _: () = msg_send![&*v.container, performSelector: sel!(snorkleFitWidth), withObject: Option::<&AnyObject>::None, afterDelay: 0.35f64];
                }
            }
        }
        self.sync_backdrop();
    }

    /// Sets a container's frame (and its web view's, which normally follows
    /// by autoresizing) and cuts the holes out of it.
    fn place(&self, view: &UIView, frame: CGRect) {
        view.setFrame(frame);
        for sub in view.subviews().iter() {
            sub.setFrame(view.bounds());
        }
        apply_mask(view, &self.holes.borrow());
    }

    /// Replaces everything injected (user scripts, view modes with per-tab
    /// overrides) in every content view; it applies from the next load.
    /// With `run_now`, each loaded page is also brought in line with its
    /// view modes immediately.
    pub fn set_injections(&mut self, injections: crate::Injections, run_now: bool) {
        self.injections = injections;
        self.compile_scripts();
        for (tab, v) in &self.views {
            injection::install(self.mtm, &v.controller, self.scripts_for(*tab));
            if run_now {
                injection::run_now(self.mtm, &v.web, &self.injections.view_for(*tab).apply_js);
            }
        }
    }

    /// Turns page console capture on or off for every content view. Takes
    /// effect from the next page load.
    pub fn set_console_capture(&mut self, on: bool) {
        self.console = on;
        self.compile_scripts();
        for (tab, v) in &self.views {
            injection::install(self.mtm, &v.controller, self.scripts_for(*tab));
        }
    }

    /// Runs the "Detect Headers" sampler in `tab`'s page (headers view on);
    /// the sample arrives through `ContentEvents::headings`.
    pub fn sample_headings(&mut self, tab: TabId) {
        if let Some(v) = self.views.get(&tab) {
            injection::run_now(self.mtm, &v.web, &snorkle_core::viewmodes::headings_sample_js());
        }
    }

    /// Reads `tab`'s page for the AI chat (title, visible text, selection;
    /// `snorkle_core::ai::PAGE_TEXT_JS`). `done` runs on the main thread.
    pub fn read_page_text(&mut self, tab: TabId, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, snorkle_core::ai::PAGE_TEXT_JS, done),
            None => done(None),
        }
    }

    /// Scrolls `tab`'s page to one of its headings (headers view).
    pub fn scroll_to_heading(&mut self, tab: TabId, index: u32) {
        if let Some(v) = self.views.get(&tab) {
            injection::run_now(self.mtm, &v.web, &snorkle_core::viewmodes::scroll_to_heading_js(index));
        }
    }

    fn compile_scripts(&mut self) {
        let console = self.console.then_some(&self.events);
        let build = |view: &crate::ViewInjections| {
            let all: Vec<Injection> = self.injections.user.iter().chain(&view.sources).cloned().collect();
            injection::make_scripts(self.mtm, &all, console, view.headings.then_some(&self.events))
        };
        self.scripts = build(&self.injections.view);
        self.tab_scripts = self.injections.tabs.iter().map(|(t, v)| (*t, build(v))).collect();
    }

    fn scripts_for(&self, tab: TabId) -> &injection::Scripts {
        self.tab_scripts.get(&tab).unwrap_or(&self.scripts)
    }

    /// Fetches a favicon for `host` in a sandboxed, throwaway web view (see
    /// `favicon.rs`). `done` runs on the main thread.
    pub fn fetch_favicon(&mut self, host: String, done: crate::FaviconDone) {
        favicon::fetch(self.mtm, &self.root, host, done);
    }

    /// Deletes one kind of website data from every profile's store.
    pub fn clear_data(&mut self, kind: DataKind) {
        unsafe {
            let cache = [
                WKWebsiteDataTypeDiskCache,
                WKWebsiteDataTypeMemoryCache,
                WKWebsiteDataTypeFetchCache,
                WKWebsiteDataTypeOfflineWebApplicationCache,
            ];
            let site = [
                WKWebsiteDataTypeLocalStorage,
                WKWebsiteDataTypeSessionStorage,
                WKWebsiteDataTypeIndexedDBDatabases,
                WKWebsiteDataTypeWebSQLDatabases,
                WKWebsiteDataTypeServiceWorkerRegistrations,
                WKWebsiteDataTypeFileSystem,
                WKWebsiteDataTypeMediaKeys,
            ];
            let list: Vec<&NSString> = match kind {
                DataKind::Cache => cache.to_vec(),
                DataKind::Cookies => vec![WKWebsiteDataTypeCookies],
                DataKind::SiteData => site.to_vec(),
                DataKind::Logins => std::iter::once(WKWebsiteDataTypeCookies).chain(site).collect(),
            };
            let types = NSSet::from_slice(&list);
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
                Effect::AdoptPopup { tab } => {
                    if let Some(parts) = PENDING_POPUPS.with(|p| p.borrow_mut().remove(tab)) {
                        let view = self.mount(*tab, parts);
                        self.views.insert(*tab, view);
                    }
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
                        self.sync_backdrop();
                    }
                    if !*keep_state {
                        self.saved.remove(tab);
                    }
                }
                Effect::ShowView { tab, pane } => {
                    if let Some(v) = self.views.get(tab) {
                        v.pane.set(*pane);
                        self.place(&v.container, self.pane_frame(*pane));
                        self.root.bringSubviewToFront(&v.container);
                        v.container.setHidden(false);
                        set_corner_radius(&v.container, if *pane == Pane::Sheet { 12.0 } else { 0.0 });
                    }
                    self.sync_backdrop();
                }
                Effect::HideView(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.container.setHidden(true);
                    }
                    self.sync_backdrop();
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

        let frame = self.content_frame();
        let parts = build_web_view(mtm, tab, &config, CGRect::new(CGPoint::new(0.0, 0.0), frame.size), &self.events);
        self.mount(tab, parts)
    }

    /// Puts a web view into a clipped container in the content area and
    /// wires injection, content blocking and tap-to-focus. Used for views we
    /// create and for popups WebKit asked us to create.
    unsafe fn mount(&mut self, tab: TabId, parts: WebViewParts) -> View {
        let mtm = self.mtm;
        let WebViewParts { web, nav, ui, kvo } = parts;
        let controller = web.configuration().userContentController();
        let frame = self.content_frame();
        let container = keys::KeyContainer::new_view(mtm, frame, self.events.clone(), self.holes.clone());
        container.setClipsToBounds(true);
        container.setAutoresizingMask(UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight);
        container.setHidden(true);
        web.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), frame.size));

        container.addSubview(&web);
        apply_mask(&container, &self.holes.borrow());
        self.root.addSubview(&container);
        injection::install(mtm, &controller, self.scripts_for(tab));
        rules::register(tab, controller.clone());
        WEB_VIEW_TABS.with(|m| m.borrow_mut().insert(Retained::as_ptr(&web) as usize, tab));

        let tap = TapFocus::new(mtm, TabIvars::new(tab, self.events.clone()));
        let recognizer = UITapGestureRecognizer::initWithTarget_action(
            UITapGestureRecognizer::alloc(mtm),
            Some(&tap),
            Some(sel!(tapped:)),
        );
        recognizer.setCancelsTouchesInView(false);
        recognizer.setDelegate(Some(ProtocolObject::from_ref(&*tap)));
        container.addGestureRecognizer(&recognizer);

        // Pull down to refresh.
        let refresh = RefreshTarget::new(mtm, TabIvars::new(tab, self.events.clone()));
        let control = UIRefreshControl::new(mtm);
        control.addTarget_action_forControlEvents(Some(&refresh), sel!(refresh:), UIControlEvents::ValueChanged);
        web.scrollView().setRefreshControl(Some(&control));

        View {
            container,
            web,
            controller,
            pane: Cell::new(Pane::Full),
            _tap: tap,
            _tap_recognizer: recognizer,
            _refresh: refresh,
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
    web.setAllowsBackForwardNavigationGestures(spec.allows_back_forward_gestures);
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

/// Masks `view` (a direct subview of the root view) so the holes that
/// intersect it are not drawn; no mask when none does. Holes never overlap
/// (`sanitize_holes` merges them), as the even-odd fill requires.
fn apply_mask(view: &UIView, holes: &[Hole]) {
    let f = view.frame();
    let b = view.bounds();
    let local: Vec<(CGRect, f64)> = holes
        .iter()
        .map(|h| (CGRect::new(CGPoint::new(h.x - f.origin.x, h.y - f.origin.y), CGSize::new(h.w, h.h)), h.r))
        .filter(|(r, _)| {
            r.origin.x < b.size.width
                && r.origin.y < b.size.height
                && r.origin.x + r.size.width > 0.0
                && r.origin.y + r.size.height > 0.0
        })
        .collect();
    let layer = view.layer();
    if local.is_empty() {
        unsafe { layer.setMask(None) };
        return;
    }
    let path = UIBezierPath::bezierPathWithRect(b);
    for (rect, radius) in &local {
        path.appendPath(&UIBezierPath::bezierPathWithRoundedRect_cornerRadius(*rect, *radius));
    }
    let mask = CAShapeLayer::layer();
    mask.setFrame(b);
    mask.setPath(Some(&path.CGPath()));
    unsafe {
        mask.setFillRule(kCAFillRuleEvenOdd);
        layer.setMask(Some(&mask));
    }
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
