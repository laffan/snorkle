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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
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

        // Back from another app, iPadOS may leave nothing first responder
        // until the next tap, and then key commands (the shortcuts, e.g.
        // ⌘T) are not delivered: give the chrome first-responder status in
        // that case (a page or the chrome that still has it keeps it).
        let chrome: Rc<RefCell<Option<Retained<UIView>>>> = Rc::default();
        let (ev, ch) = (events.clone(), chrome.clone());
        let active = RcBlock::new(move |_n: NonNull<NSNotification>| {
            let Some(mtm) = MainThreadMarker::new() else { return };
            let note = menu::ensure_responder(mtm, ch.borrow().as_deref());
            ev.note(note);
        });
        observers.push(center.addObserverForName_object_queue_usingBlock(
            Some(UIApplicationDidBecomeActiveNotification),
            None,
            Some(&queue),
            &active,
        ));

        // Pointer near the left edge, for the sidebar's edge tab.
        let hover = HoverEdge::new(mtm, HoverIvars::new(events.clone()));
        let recognizer = UIHoverGestureRecognizer::initWithTarget_action(
            UIHoverGestureRecognizer::alloc(mtm),
            Some(&hover),
            Some(sel!(hovered:)),
        );
        recognizer.setDelegate(Some(ProtocolObject::from_ref(&*hover)));
        root.addGestureRecognizer(&recognizer);

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
            _hover: hover,
            holes,
            chrome,
            root,
            top: Cell::new(0.0),
            left: Cell::new(0.0),
            right: Cell::new(0.0),
            bottom: Cell::new(0.0),
            split_ratio: Cell::new(0.5),
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

    /// Frame of one pane: the whole content area, or part of it in split
    /// view (side by side, or one above the other): the first pane gets
    /// `split_ratio` of the space less the 8 pt gap (floored), the second
    /// the rest. The chrome draws the divider's handle in the gap, which no
    /// page covers (`ui/chrome.js` `splitSizes` uses the same formula).
    fn pane_frame(&self, pane: Pane) -> CGRect {
        let area = self.content_frame();
        let r = self.split_ratio.get();
        let sizes = |total: f64| {
            let first = ((total - SPLIT_GAP) * r).floor().max(0.0);
            (first, (total - SPLIT_GAP - first).max(0.0))
        };
        let (left_w, right_w) = sizes(area.size.width);
        let (top_h, bottom_h) = sizes(area.size.height);
        match pane {
            Pane::Full => area,
            Pane::Left => CGRect::new(area.origin, CGSize::new(left_w, area.size.height)),
            Pane::Right => CGRect::new(
                CGPoint::new(area.origin.x + area.size.width - right_w, area.origin.y),
                CGSize::new(right_w, area.size.height),
            ),
            Pane::Top => CGRect::new(area.origin, CGSize::new(area.size.width, top_h)),
            Pane::Bottom => CGRect::new(
                CGPoint::new(area.origin.x, area.origin.y + area.size.height - bottom_h),
                CGSize::new(area.size.width, bottom_h),
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
                self.place(&self.backdrop, self.content_frame(), 1.0);
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
    /// With `animate`, the page frames move over 0.25 s (the sidebar's
    /// slide in the chrome takes as long).
    /// `animate`: seconds since the chrome began its slide (`None`: move
    /// at once).
    pub fn set_insets(&mut self, top: f64, left: f64, right: f64, bottom: f64, holes: Vec<Hole>, animate: Option<f64>) {
        let before = self.content_frame().size.width;
        self.top.set(top);
        self.left.set(left);
        self.right.set(right);
        self.bottom.set(bottom);
        let b = self.root.bounds();
        let holes = crate::sanitize_holes(&holes, b.size.width, b.size.height);
        // The cut-outs glide (with the frames) only between sets of the
        // same shape; e.g. the sidebar's pop-over and toggle tab, which the
        // chrome always reports, on- or off-screen.
        let glide = animate.filter(|_| holes.len() == self.holes.borrow().len());
        *self.holes.borrow_mut() = holes;
        let grew = self.content_frame().size.width > before + 0.5;
        let moves: Vec<(Retained<UIView>, CGRect, f64)> =
            self.views.values().map(|v| (v.container.clone(), self.pane_frame(v.pane.get()), v.scale.get())).collect();
        if let Some(lag) = animate {
            let holes = self.holes.borrow().clone();
            let block = RcBlock::new(move || {
                for (view, frame, scale) in &moves {
                    place_view(view, *frame, &holes, glide, *scale);
                }
            });
            UIView::animateWithDuration_animations((GLIDE_SECONDS - lag).max(0.05), &block, self.mtm);
        } else {
            for (view, frame, scale) in &moves {
                self.place(view, *frame, *scale);
            }
        }
        for v in self.views.values() {
            if grew {
                // WebKit re-lays out the page asynchronously; check once it has.
                unsafe {
                    let _: () = msg_send![&*v.container, performSelector: sel!(snorkleFitWidth), withObject: Option::<&AnyObject>::None, afterDelay: 0.35f64];
                }
            }
        }
        self.sync_backdrop();
    }

    /// Sets a container's frame (and its web view's, scaled by `scale`)
    /// and cuts the holes out of it.
    fn place(&self, view: &UIView, frame: CGRect, scale: f64) {
        place_view(view, frame, &self.holes.borrow(), None, scale);
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
            set_dark(&v.web, self.injections.view_for(*tab).dark);
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

    /// Remembers the chrome's web view (a `WKWebView*` from Tauri).
    ///
    /// # Safety
    /// `view` must be a valid `UIView*` that outlives the host, or null.
    pub unsafe fn set_chrome_view(&mut self, view: *mut c_void) {
        *self.chrome.borrow_mut() = unsafe { Retained::retain(view.cast::<UIView>()) };
    }

    /// Gives the chrome's web view keyboard focus, so an input the chrome
    /// focuses after a shortcut (new-tab palette, find) receives typing
    /// even when a page, or nothing, was first responder.
    pub fn focus_chrome(&mut self) {
        if let Some(v) = self.chrome.borrow().as_ref() {
            v.becomeFirstResponder();
        }
    }

    /// Saves the tab's page as a web archive (an offline copy of a pinned
    /// or bookmarked page); `done` gets its bytes, or `None` (failed, or
    /// larger than `storage::MAX_OFFLINE_COPY_BYTES`).
    pub fn save_offline(&mut self, tab: TabId, done: crate::DataDone) {
        let Some(v) = self.views.get(&tab) else { return done(None) };
        let done = Cell::new(Some(done));
        let block = RcBlock::new(move |data: *mut NSData, _error: *mut NSError| {
            let Some(done) = done.take() else { return };
            let data = unsafe { data.as_ref() };
            done(data.filter(|d| d.length() <= snorkle_core::storage::MAX_OFFLINE_COPY_BYTES).map(|d| d.to_vec()));
        });
        unsafe { v.web.createWebArchiveDataWithCompletionHandler(&block) };
    }

    /// Shows an offline copy (web archive) in the tab, with `url` as its
    /// base URL, and JavaScript off for that load. The core has already
    /// accepted it (`Browser::use_offline_copy`).
    pub fn load_offline(&mut self, tab: TabId, url: &Url, data: Vec<u8>) {
        let Some(v) = self.views.get(&tab) else { return };
        let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url.as_str())) else { return };
        OFFLINE_LOADS.with(|s| s.borrow_mut().insert(Retained::as_ptr(&v.web) as usize));
        let data = NSData::with_bytes(&data);
        unsafe {
            v.web.loadData_MIMEType_characterEncodingName_baseURL(
                &data,
                &NSString::from_str("application/x-webarchive"),
                &NSString::from_str("utf-8"),
                &nsurl,
            );
        }
    }

    /// Image menu, once the image was fetched (`http::fetch_image`): Copy
    /// puts it on the pasteboard; Share opens the share sheet with it (as a
    /// file, from `from`, window points); Download opens the Files export
    /// picker for a copy. Files are written to a temporary folder that is
    /// emptied on the next use.
    pub fn image_result(
        &mut self,
        action: snorkle_core::imagemenu::ImageAction,
        url: &Url,
        data: Vec<u8>,
        mime: &str,
        from: (f64, f64, f64, f64),
    ) -> Result<(), String> {
        use snorkle_core::imagemenu::{download_name, ImageAction};
        if action == ImageAction::Copy {
            let image =
                UIImage::imageWithData(&NSData::with_bytes(&data)).ok_or("This kind of image cannot be copied.")?;
            unsafe { UIPasteboard::generalPasteboard().setImage(Some(&image)) };
            return Ok(());
        }
        let dir = std::env::temp_dir().join("snorkle-images");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(download_name(url, mime));
        std::fs::write(&path, &data).map_err(|e| e.to_string())?;
        if action == ImageAction::Download {
            self.export_file(&path)
        } else {
            self.share_file(&path, from)
        }
    }

    /// Opens the Files export picker for a copy of `path` (a downloaded or
    /// fetched file in the app's own folders).
    pub fn export_file(&mut self, path: &std::path::Path) -> Result<(), String> {
        let file = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let top = self.top_controller().ok_or("The file could not be shown.")?;
        let urls = NSArray::from_retained_slice(&[file]);
        let picker =
            UIDocumentPickerViewController::initForExportingURLs_asCopy(UIDocumentPickerViewController::alloc(self.mtm), &urls, true);
        top.presentViewController_animated_completion(&picker, true, None);
        Ok(())
    }

    /// The share sheet for the file at `path`, as a popover from `from`
    /// (window points).
    pub fn share_file(&mut self, path: &std::path::Path, from: (f64, f64, f64, f64)) -> Result<(), String> {
        let file = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let top = self.top_controller().ok_or("The file could not be shown.")?;
        unsafe {
            let items: Retained<NSArray> = NSArray::from_retained_slice(&[Retained::into_super(Retained::into_super(file))]);
            let vc = UIActivityViewController::initWithActivityItems_applicationActivities(
                UIActivityViewController::alloc(self.mtm),
                &items,
                None,
            );
            let Some(pop) = vc.popoverPresentationController() else { return Err("The share sheet is unavailable.".into()) };
            pop.setSourceView(Some(&self.root));
            pop.setSourceRect(rect(from));
            top.presentViewController_animated_completion(&vc, true, None);
        }
        Ok(())
    }

    /// Cancels a running download.
    pub fn cancel_download(&mut self, id: u64) {
        crate::webkit::downloads::cancel(id);
    }

    /// The user's answer to a download from a frame inside a page.
    pub fn answer_frame_download(&mut self, ask: u64, allow: bool) {
        crate::webkit::downloads::answer_frame(ask, allow);
    }

    /// Light, dark or the system's appearance for the whole window: the
    /// chrome, system UI it presents and content views (a content view in
    /// dark pages mode keeps its own dark override).
    /// Settings → General → Swipe between pages (also future views, through
    /// `crate::swipe_navigation`).
    pub fn set_swipe_navigation(&mut self, on: bool) {
        for v in self.views.values() {
            unsafe { v.web.setAllowsBackForwardNavigationGestures(on && CONTENT_VIEW_SPEC.allows_back_forward_gestures) };
        }
    }

    /// Settings → General → Pull to refresh (also future views, through
    /// `crate::pull_to_refresh`).
    pub fn set_pull_to_refresh(&mut self, on: bool) {
        for v in self.views.values() {
            let scroll = unsafe { v.web.scrollView() };
            if on {
                scroll.setRefreshControl(Some(&v.refresh_control));
            } else {
                if v.refresh_control.isRefreshing() {
                    v.refresh_control.endRefreshing();
                }
                scroll.setRefreshControl(None);
            }
        }
    }

    pub fn set_appearance(&mut self, appearance: crate::Appearance) {
        if let Some(window) = self.root.window() {
            window.setOverrideUserInterfaceStyle(match appearance {
                crate::Appearance::System => UIUserInterfaceStyle::Unspecified,
                crate::Appearance::Light => UIUserInterfaceStyle::Light,
                crate::Appearance::Dark => UIUserInterfaceStyle::Dark,
            });
        }
    }

    /// Keyboard shortcut bindings (validated, one per `SHORTCUTS` entry)
    /// and whether they are suspended (the editor is recording a key).
    pub fn set_shortcuts(&mut self, bindings: Vec<crate::KeyBinding>, suspend: bool) {
        menu::set_bindings(self.mtm, bindings, suspend);
    }

    /// The view controller to present system UI from: the window's root,
    /// or whatever it is already presenting.
    fn top_controller(&self) -> Option<Retained<UIViewController>> {
        let mut vc = self.root.window()?.rootViewController()?;
        while let Some(next) = vc.presentedViewController() {
            vc = next;
        }
        Some(vc)
    }

    /// The system share sheet for `url` (an http(s) page URL from the
    /// core), as a popover pointing at `from` (window coordinates; the
    /// share button). On iPad a popover needs a source, or UIKit throws.
    pub fn share(&mut self, url: &str, from: (f64, f64, f64, f64)) {
        let from = rect(from);
        let Some(top) = self.top_controller() else { return };
        let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url)) else { return };
        unsafe {
            let items: Retained<NSArray> =
                NSArray::from_retained_slice(&[Retained::into_super(Retained::into_super(nsurl))]);
            let vc = UIActivityViewController::initWithActivityItems_applicationActivities(
                UIActivityViewController::alloc(self.mtm),
                &items,
                None,
            );
            let Some(pop) = vc.popoverPresentationController() else { return };
            pop.setSourceView(Some(&self.root));
            pop.setSourceRect(from);
            top.presentViewController_animated_completion(&vc, true, None);
        }
    }

    /// The system dictionary ("Define") for `term`.
    pub fn define(&mut self, term: &str, from: (f64, f64, f64, f64)) {
        let from = rect(from);
        let Some(top) = self.top_controller() else { return };
        let vc = UIReferenceLibraryViewController::initWithTerm(
            UIReferenceLibraryViewController::alloc(self.mtm),
            &NSString::from_str(term),
        );
        if let Some(pop) = vc.popoverPresentationController() {
            pop.setSourceView(Some(&self.root));
            pop.setSourceRect(from);
        }
        top.presentViewController_animated_completion(&vc, true, None);
    }

    /// Reads the selected text of `tab`'s page (C20; `selection::TEXT_JS`).
    pub fn read_selection(&mut self, tab: TabId, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, snorkle_core::selection::TEXT_JS, done),
            None => done(None),
        }
    }

    /// Find in page (⌘F): the constant `find::find_js` highlights the
    /// matches of `query`, scrolls to the current one and returns its
    /// position, "index/total" (channel C18; `find::parse_position`).
    pub fn find(&mut self, tab: TabId, query: String, step: snorkle_core::find::FindStep, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, &snorkle_core::find::find_js(&query, step), done),
            None => done(None),
        }
    }

    /// Removes the find highlights (closing the find bar).
    pub fn end_find(&mut self, tab: TabId) {
        if let Some(v) = self.views.get(&tab) {
            injection::run_now(self.mtm, &v.web, snorkle_core::find::CLEAR_JS);
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
            injection::make_scripts(self.mtm, &all, console, view.headings.then_some(&self.events), Some(&self.events))
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
        unsafe { stores::clear_data(std::iter::once(&*self.default_store).chain(self.private_store.as_deref()), kind) }
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
                        self.place(&v.container, self.pane_frame(*pane), v.scale.get());
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
                Effect::SetSplitRatio(permille) => {
                    self.split_ratio.set(f64::from((*permille).clamp(200, 800)) / 1000.0);
                    for v in self.views.values().filter(|v| !v.container.isHidden()) {
                        self.place(&v.container, self.pane_frame(v.pane.get()), v.scale.get());
                    }
                }
                Effect::SetZoom { tab, percent } => {
                    if let Some(v) = self.views.get(tab) {
                        // The core only sends levels from 50 to 150 %.
                        v.scale.set(f64::from((*percent).clamp(50, 150)) / 100.0);
                        self.place(&v.container, self.pane_frame(v.pane.get()), v.scale.get());
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
            stores::wipe(self.mtm, &store);
        }
    }

    unsafe fn create_view(&mut self, tab: TabId, profile: Profile) -> View {
        let mtm = self.mtm;
        let config = stores::content_configuration(mtm, &self.store_for(profile));
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
        apply_mask(&container, &self.holes.borrow(), None);
        set_dark(&web, self.injections.view_for(tab).dark);
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
        if crate::pull_to_refresh() {
            web.scrollView().setRefreshControl(Some(&control));
        }

        // Debug builds: note each scroll gesture in the console (OPEN_QUESTIONS 109).
        let probe = if cfg!(debug_assertions) {
            let p = ScrollProbe::new(mtm, TabIvars::new(tab, self.events.clone()));
            let pan: Retained<objc2_ui_kit::UIGestureRecognizer> = msg_send![&*web.scrollView(), panGestureRecognizer];
            pan.addTarget_action(&p, sel!(panned:));
            Some((p, pan))
        } else {
            None
        };

        View {
            container,
            web,
            controller,
            pane: Cell::new(Pane::Full),
            scale: Cell::new(1.0),
            _tap: tap,
            _tap_recognizer: recognizer,
            _refresh: refresh,
            refresh_control: control,
            probe,
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

/// Dark mode prefers a dark appearance for the page (`prefers-color-scheme`
/// and the default canvas); otherwise the system's.
fn set_dark(web: &WKWebView, dark: bool) {
    web.setOverrideUserInterfaceStyle(if dark { UIUserInterfaceStyle::Dark } else { UIUserInterfaceStyle::Unspecified });
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
