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
    NSWindowButton, NSWorkspace,
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
    /// The share sheet while it is open (AppKit does not retain it).
    sharing: Option<Retained<NSSharingServicePicker>>,
    /// Stacking order of the containers (a higher value is in front).
    z: Cell<u64>,
    _monitors: keys::Monitors,
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

impl ContentHost {
    /// # Safety
    /// `window` must be a valid `NSWindow*` (the Tauri main window, from
    /// `WebviewWindow::ns_window`) whose content view holds the chrome's
    /// web view, and this must be called on the main thread.
    pub unsafe fn new(window: *mut c_void, events: Rc<dyn ContentEvents>) -> Self {
        let mtm = MainThreadMarker::new().expect("ContentHost must be created on the main thread");
        let window = Retained::retain(window.cast::<NSWindow>()).expect("window must not be null");
        let content = window.contentView().expect("the window has a content view");
        let root = ContentRoot::new(mtm, content.bounds());
        root.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&root);
        // Pointer leaving the window (the edge tab hides).
        let area = NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::ZERO,
            NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveInKeyWindow
                | NSTrackingAreaOptions::InVisibleRect,
            Some(&root),
            None,
        );
        root.addTrackingArea(&area);

        let id = NSUUID::from_bytes(DEFAULT_PROFILE_STORE_ID);
        let default_store = WKWebsiteDataStore::dataStoreForIdentifier(&id, mtm);

        // Leaving the app (another app is active): what iPadOS does on
        // entering the background (discard idle views, write the session).
        let center = NSNotificationCenter::defaultCenter();
        let queue = NSOperationQueue::mainQueue();
        let ev = events.clone();
        let bg = RcBlock::new(move |_n: NonNull<NSNotification>| ev.did_enter_background());
        let observers = vec![center.addObserverForName_object_queue_usingBlock(
            Some(NSApplicationDidResignActiveNotification),
            None,
            Some(&queue),
            &bg,
        )];

        let monitors = keys::install(mtm, &window, &root, events.clone());
        let holes = Holes::default();
        let backdrop = Container::new(mtm, None, holes.clone());
        if let Some(layer) = backdrop.layer() {
            let black = NSColor::colorWithWhite_alpha(0.0, 0.45);
            layer.setBackgroundColor(Some(&black.CGColor()));
        }
        backdrop.setHidden(true);
        root.addSubview(&backdrop);

        ContentHost {
            mtm,
            window,
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
            backdrop,
            holes,
            chrome: None,
            sharing: None,
            z: Cell::new(0),
            _monitors: monitors,
        }
    }

    pub fn live_views(&self) -> usize {
        self.views.len()
    }

    fn height(&self) -> f64 {
        self.root.bounds().size.height
    }

    fn content_frame(&self) -> Tl {
        let b = self.root.bounds().size;
        let top = clamp_inset(self.top.get(), b.height);
        let left = clamp_inset(self.left.get(), b.width);
        let right = clamp_inset(self.right.get(), b.width);
        let bottom = clamp_inset(self.bottom.get(), b.height);
        Tl { x: left, y: top, w: (b.width - left - right).max(0.0), h: (b.height - top - bottom).max(0.0) }
    }

    /// Frame of one pane, from the top left; the same layout as on iPadOS
    /// (`ios/mod.rs` `pane_frame`, `ui/chrome.js` `splitSizes`).
    fn pane_frame(&self, pane: Pane) -> Tl {
        let a = self.content_frame();
        let r = self.split_ratio.get();
        let sizes = |total: f64| {
            let first = ((total - SPLIT_GAP) * r).floor().max(0.0);
            (first, (total - SPLIT_GAP - first).max(0.0))
        };
        let (left_w, right_w) = sizes(a.w);
        let (top_h, bottom_h) = sizes(a.h);
        match pane {
            Pane::Full => a,
            Pane::Left => Tl { w: left_w, ..a },
            Pane::Right => Tl { x: a.x + a.w - right_w, w: right_w, ..a },
            Pane::Top => Tl { h: top_h, ..a },
            Pane::Bottom => Tl { y: a.y + a.h - bottom_h, h: bottom_h, ..a },
            Pane::Sheet => {
                const MARGIN: f64 = 16.0;
                let w = (a.w - 2.0 * MARGIN).clamp(0.0, 620.0);
                let h = (a.h - 2.0 * MARGIN).clamp(0.0, 820.0);
                Tl { x: (a.x + (a.w - w) / 2.0).floor(), y: (a.y + (a.h - h) / 2.0).floor(), w, h }
            }
        }
    }

    /// Moves `view` in front of the other containers without taking it out
    /// of the window (removing a web view from its window would pause it).
    fn bring_to_front(&self, view: &NSView) {
        let n = self.z.get() + 1;
        self.z.set(n);
        Z.with(|z| z.borrow_mut().insert(view as *const NSView as usize, n));
        unsafe { self.root.sortSubviewsUsingFunction_context(by_z, std::ptr::null_mut()) };
    }

    /// Shows the backdrop under the visible sheet, if any, and keeps the
    /// sheet above it.
    fn sync_backdrop(&self) {
        let sheet = self.views.values().find(|v| v.pane.get() == Pane::Sheet && !v.container.isHidden());
        match sheet {
            Some(v) => {
                self.place(&self.backdrop, self.content_frame());
                self.bring_to_front(&self.backdrop);
                self.bring_to_front(&v.container);
                self.backdrop.setHidden(false);
            }
            None => self.backdrop.setHidden(true),
        }
    }

    /// Insets of the content area reported by the chrome (see the iPadOS
    /// host for their meaning), the holes, and whether to animate (seconds
    /// since the chrome began its slide).
    ///
    /// The window's close, minimise and zoom buttons sit in a strip at the
    /// top of the docked sidebar; they are hidden while it is not docked
    /// (`left` is 0), so they never float over a page.
    pub fn set_insets(&mut self, top: f64, left: f64, right: f64, bottom: f64, holes: Vec<Hole>, animate: Option<f64>) {
        self.top.set(top);
        self.left.set(left);
        self.right.set(right);
        self.bottom.set(bottom);
        let b = self.root.bounds().size;
        let holes = crate::sanitize_holes(&holes, b.width, b.height);
        let glide = animate.filter(|_| holes.len() == self.holes.borrow().len());
        *self.holes.borrow_mut() = holes;
        self.set_window_buttons(clamp_inset(left, b.width) > 0.5);
        let moves: Vec<(Retained<Container>, Tl)> =
            self.views.values().map(|v| (v.container.clone(), self.pane_frame(v.pane.get()))).collect();
        let height = self.height();
        match animate {
            Some(lag) => {
                let holes = self.holes.borrow().clone();
                let duration = (GLIDE_SECONDS - lag).max(0.05);
                let changes = RcBlock::new(move |ctx: NonNull<NSAnimationContext>| {
                    let ctx = unsafe { ctx.as_ref() };
                    ctx.setDuration(duration);
                    ctx.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(unsafe {
                        objc2_quartz_core::kCAMediaTimingFunctionEaseInEaseOut
                    })));
                    for (view, frame) in &moves {
                        view.ivars().placed.set(*frame);
                        let animator: Retained<AnyObject> = unsafe { msg_send![&**view, animator] };
                        let _: () = unsafe { msg_send![&*animator, setFrame: frame.ns(height)] };
                        views::apply_mask(view, &holes, glide);
                    }
                });
                NSAnimationContext::runAnimationGroup_completionHandler(&changes, None);
            }
            None => {
                for (view, frame) in &moves {
                    self.place(view, *frame);
                }
            }
        }
        self.sync_backdrop();
    }

    fn set_window_buttons(&self, shown: bool) {
        for b in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
            if let Some(button) = self.window.standardWindowButton(b) {
                button.setHidden(!shown);
            }
        }
    }

    /// Sets a container's frame and cuts the holes out of it.
    fn place(&self, view: &Container, frame: Tl) {
        view.ivars().placed.set(frame);
        view.setFrame(frame.ns(self.height()));
        views::apply_mask(view, &self.holes.borrow(), None);
    }

    /// Replaces everything injected; see the iPadOS host.
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

    /// Turns page console capture on or off (from the next page load).
    pub fn set_console_capture(&mut self, on: bool) {
        self.console = on;
        self.compile_scripts();
        for (tab, v) in &self.views {
            injection::install(self.mtm, &v.controller, self.scripts_for(*tab));
        }
    }

    pub fn sample_headings(&mut self, tab: TabId) {
        if let Some(v) = self.views.get(&tab) {
            injection::run_now(self.mtm, &v.web, &snorkle_core::viewmodes::headings_sample_js());
        }
    }

    /// Save for Later: a JPEG thumbnail of what the tab's view shows now
    /// (its visible bounds, `THUMB_WIDTH` points wide), or `None` without a
    /// live view or if WebKit cannot draw it.
    pub fn thumbnail(&mut self, tab: TabId, done: crate::ThumbDone) {
        let Some(v) = self.views.get(&tab) else {
            done(None);
            return;
        };
        let done = RefCell::new(Some(done));
        unsafe {
            let cfg = objc2_web_kit::WKSnapshotConfiguration::new(self.mtm);
            cfg.setSnapshotWidth(Some(&objc2_foundation::NSNumber::new_f64(crate::THUMB_WIDTH)));
            let block = block2::RcBlock::new(move |image: *mut NSImage, _err: *mut NSError| {
                let jpeg = image.as_ref().and_then(thumbnail_jpeg);
                if let Some(f) = done.borrow_mut().take() {
                    f(jpeg);
                }
            });
            v.web.takeSnapshotWithConfiguration_completionHandler(Some(&cfg), &block);
        }
    }

    /// ⋯ → Open in App is an iPadOS feature (AppKit has no "universal
    /// links only" open); the chrome does not offer it here.
    pub fn open_in_app(&mut self, _url: &Url, done: crate::OpenedDone) {
        done(false);
    }

    pub fn read_page_text(&mut self, tab: TabId, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, snorkle_core::ai::PAGE_TEXT_JS, done),
            None => done(None),
        }
    }

    /// Remembers the chrome's web view (a `WKWebView*` from Tauri).
    ///
    /// # Safety
    /// `view` must be a valid `NSView*` that outlives the host, or null.
    pub unsafe fn set_chrome_view(&mut self, view: *mut c_void) {
        self.chrome = unsafe { Retained::retain(view.cast::<NSView>()) };
    }

    /// Gives the chrome's web view keyboard focus.
    pub fn focus_chrome(&mut self) {
        if let Some(v) = self.chrome.as_ref() {
            self.window.makeFirstResponder(Some(v));
        }
    }

    /// Saves the tab's page as a web archive (offline copy).
    pub fn save_offline(&mut self, tab: TabId, done: crate::DataDone) {
        self.web_archive(tab, snorkle_core::storage::MAX_OFFLINE_COPY_BYTES, done);
    }

    /// WebKit's web archive of the tab's page as it is now
    /// (`createWebArchiveData`), at most `max` bytes; `None` otherwise.
    /// Save for Later reads the resources WebKit holds for the page from it.
    pub fn web_archive(&mut self, tab: TabId, max: usize, done: crate::DataDone) {
        let Some(v) = self.views.get(&tab) else { return done(None) };
        let done = Cell::new(Some(done));
        let block = RcBlock::new(move |data: *mut NSData, _error: *mut NSError| {
            let Some(done) = done.take() else { return };
            let data = unsafe { data.as_ref() };
            done(data.filter(|d| d.length() <= max).map(|d| d.to_vec()));
        });
        unsafe { v.web.createWebArchiveDataWithCompletionHandler(&block) };
    }

    /// Save for Later's capture (channel C22): runs the constant capture
    /// code (`snorkle_core::later`) in the page's client world, main frame,
    /// awaiting its promise, and hands its string result (at most `max`
    /// bytes) to `done`; `None` otherwise.
    pub fn run_capture(&mut self, tab: TabId, body: &str, max: usize, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::call_async(self.mtm, &v.web, body, max, done),
            None => done(None),
        }
    }

    /// Shows an offline copy (web archive), JavaScript off for that load.
    pub fn load_offline(&mut self, tab: TabId, url: &Url, data: Vec<u8>) {
        self.load_archive(tab, url, data, false);
    }

    /// Loads a web archive in the tab with `url` as its base URL: an
    /// offline copy (JavaScript off) or a Save for Later archive (its
    /// page's scripts run, JavaScript off only for a copy of the live
    /// document or while choosing links). The core has already accepted it
    /// (`Browser::use_offline_copy`, `Browser::show_saved_copy`).
    pub fn load_archive(&mut self, tab: TabId, url: &Url, data: Vec<u8>, scripts: bool) {
        let Some(v) = self.views.get(&tab) else { return };
        let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url.as_str())) else { return };
        OFFLINE_LOADS.with(|s| {
            let key = Retained::as_ptr(&v.web) as usize;
            if scripts {
                s.borrow_mut().remove(&key);
            } else {
                s.borrow_mut().insert(key);
            }
        });
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

    /// Image menu, once the image was fetched: Copy puts it on the
    /// pasteboard; Share opens the share menu with it (as a file, from
    /// `from`, window points from the top left); Download asks where to
    /// save it (a save panel attached to the window). Files for Share are
    /// written to a temporary folder that is emptied on the next use.
    pub fn image_result(
        &mut self,
        action: snorkle_core::imagemenu::ImageAction,
        url: &Url,
        data: Vec<u8>,
        mime: &str,
        from: (f64, f64, f64, f64),
    ) -> Result<(), String> {
        use snorkle_core::imagemenu::{download_name, ImageAction};
        let bytes = NSData::with_bytes(&data);
        match action {
            ImageAction::Copy => {
                let image =
                    NSImage::initWithData(NSImage::alloc(), &bytes).ok_or("This kind of image cannot be copied.")?;
                let pasteboard = NSPasteboard::generalPasteboard();
                pasteboard.clearContents();
                let items = NSArray::from_retained_slice(&[image]);
                let ok: bool = unsafe { msg_send![&*pasteboard, writeObjects: &*items] };
                if ok {
                    Ok(())
                } else {
                    Err("The image could not be copied.".into())
                }
            }
            ImageAction::Download => {
                let dir = std::env::temp_dir().join("snorkle-images");
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let path = dir.join(download_name(url, mime));
                std::fs::write(&path, &data).map_err(|e| e.to_string())?;
                self.export_file(&path)
            }
            ImageAction::Share => {
                let dir = std::env::temp_dir().join("snorkle-images");
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let path = dir.join(download_name(url, mime));
                std::fs::write(&path, &data).map_err(|e| e.to_string())?;
                self.share_file(&path, from)
            }
        }
    }

    /// Asks where to save a copy of `path` (a save-panel sheet on the
    /// window) and copies it there with its attributes (the quarantine
    /// mark of a download goes with it).
    pub fn export_file(&mut self, path: &std::path::Path) -> Result<(), String> {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "download".into());
        let source = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let panel = NSSavePanel::savePanel(self.mtm);
        panel.setNameFieldStringValue(&NSString::from_str(&name));
        let p = panel.clone();
        let done = RcBlock::new(move |response: isize| {
            if response != NSModalResponseOK {
                return;
            }
            let Some(dest) = p.URL() else { return };
            let fm = NSFileManager::defaultManager();
            // The panel already asked whether to replace an existing file.
            let _ = fm.removeItemAtURL_error(&dest);
            let _ = fm.copyItemAtURL_toURL_error(&source, &dest);
        });
        panel.beginSheetModalForWindow_completionHandler(&self.window, &done);
        Ok(())
    }

    /// The share menu for the file at `path`, pointing at `from`.
    pub fn share_file(&mut self, path: &std::path::Path, from: (f64, f64, f64, f64)) -> Result<(), String> {
        let file = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        self.show_sharing(Retained::into_super(Retained::into_super(file)), from);
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

    /// The share menu for one item, pointing at `from` (window points from
    /// the top left).
    fn show_sharing(&mut self, item: Retained<AnyObject>, from: (f64, f64, f64, f64)) {
        let items: Retained<NSArray> = NSArray::from_retained_slice(&[item]);
        let picker = unsafe { NSSharingServicePicker::initWithItems(NSSharingServicePicker::alloc(), &items) };
        let rect = rect(from).ns(self.height());
        picker.showRelativeToRect_ofView_preferredEdge(rect, &self.root, NSRectEdge::MaxY);
        self.sharing = Some(picker);
    }

    /// Light, dark or the system's appearance for the window: the chrome,
    /// system UI it presents and content views (a content view in dark
    /// pages mode keeps its own dark appearance).
    /// macOS has no pull to refresh.
    pub fn set_pull_to_refresh(&mut self, _on: bool) {}

    /// Settings → General → Swipe between pages (also future views, through
    /// `crate::swipe_navigation`).
    pub fn set_swipe_navigation(&mut self, on: bool) {
        for v in self.views.values() {
            unsafe { v.web.setAllowsBackForwardNavigationGestures(on && CONTENT_VIEW_SPEC.allows_back_forward_gestures) };
        }
    }

    pub fn set_appearance(&mut self, appearance: crate::Appearance) {
        let named = |name: &NSString| NSAppearance::appearanceNamed(name);
        let a = match appearance {
            crate::Appearance::System => None,
            crate::Appearance::Light => named(unsafe { NSAppearanceNameAqua }),
            crate::Appearance::Dark => named(unsafe { NSAppearanceNameDarkAqua }),
        };
        let _: () = unsafe { msg_send![&*self.window, setAppearance: a.as_deref()] };
    }

    /// Keyboard shortcut bindings and whether they are suspended (the
    /// editor is recording a key).
    pub fn set_shortcuts(&mut self, bindings: Vec<crate::KeyBinding>, suspend: bool) {
        keys::set_bindings(bindings, suspend);
    }

    /// The system share menu for `url` (an http(s) page URL from the core),
    /// pointing at `from` (the share button).
    pub fn share(&mut self, url: &str, from: (f64, f64, f64, f64)) {
        let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url)) else { return };
        self.show_sharing(Retained::into_super(Retained::into_super(nsurl)), from);
    }

    /// The system dictionary ("Look Up") for `term`, at the selection.
    pub fn define(&mut self, term: &str, from: (f64, f64, f64, f64)) {
        let r = rect(from).ns(self.height());
        let text = NSAttributedString::from_nsstring(&NSString::from_str(term));
        self.root.showDefinitionForAttributedString_atPoint(Some(&text), NSPoint::new(r.origin.x, r.origin.y));
    }

    pub fn read_selection(&mut self, tab: TabId, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, snorkle_core::selection::TEXT_JS, done),
            None => done(None),
        }
    }

    pub fn find(&mut self, tab: TabId, query: String, step: snorkle_core::find::FindStep, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, &snorkle_core::find::find_js(&query, step), done),
            None => done(None),
        }
    }

    pub fn end_find(&mut self, tab: TabId) {
        if let Some(v) = self.views.get(&tab) {
            injection::run_now(self.mtm, &v.web, snorkle_core::find::CLEAR_JS);
        }
    }

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

    pub fn fetch_favicon(&mut self, host: String, done: crate::FaviconDone) {
        favicon::fetch(self.mtm, &self.root, host, done);
    }

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
                        Z.with(|z| z.borrow_mut().remove(&(Retained::as_ptr(&v.container) as usize)));
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
                        self.bring_to_front(&v.container);
                        v.container.setHidden(false);
                        v.container.set_corner_radius(if *pane == Pane::Sheet { 12.0 } else { 0.0 });
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
                        self.place(&v.container, self.pane_frame(v.pane.get()));
                    }
                }
                Effect::SetZoom { tab, percent } => {
                    if let Some(v) = self.views.get(tab) {
                        // The core only sends levels from 50 to 150 %.
                        v.web.setPageZoom(f64::from((*percent).clamp(50, 150)) / 100.0);
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
                        v.web.stopLoading();
                        v.web.reloadFromOrigin();
                    }
                }
                Effect::Stop(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.web.stopLoading();
                    }
                }
                Effect::ReleasePrivateData => {
                    if let Some(store) = self.private_store.take() {
                        stores::wipe(self.mtm, &store);
                    }
                }
                Effect::OpenExternal(url) => open_external(url),
                // Handled by the app layer, not the content host.
                Effect::RecordHistory { .. }
                | Effect::PersistSession(_)
                | Effect::NotifyChrome
                | Effect::OpenSavedPage { .. } => {}
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

    unsafe fn create_view(&mut self, tab: TabId, profile: Profile) -> View {
        let config = stores::content_configuration(self.mtm, &self.store_for(profile));
        let size = self.content_frame();
        let parts = build_web_view(self.mtm, tab, &config, NSSize::new(size.w, size.h), &self.events);
        self.mount(tab, parts)
    }

    /// Puts a web view into a clipped container in the content area and
    /// wires injection and content blocking. Used for views we create and
    /// for popups WebKit asked us to create.
    unsafe fn mount(&mut self, tab: TabId, parts: WebViewParts) -> View {
        let mtm = self.mtm;
        let WebViewParts { web, nav, ui, kvo } = parts;
        let controller = web.configuration().userContentController();
        let container = Container::new(mtm, Some(tab), self.holes.clone());
        container.setHidden(true);
        self.root.addSubview(&container);
        self.place(&container, self.content_frame());
        web.setFrame(container.bounds());
        web.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        container.addSubview(&web);
        set_dark(&web, self.injections.view_for(tab).dark);
        injection::install(mtm, &controller, self.scripts_for(tab));
        rules::register(tab, controller.clone());
        WEB_VIEW_TABS.with(|m| m.borrow_mut().insert(Retained::as_ptr(&web) as usize, tab));
        View { container, web, controller, pane: Cell::new(Pane::Full), _nav: nav, _ui: ui, kvo }
    }

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

/// Dark mode prefers a dark appearance for the page (`prefers-color-scheme`
/// and the default canvas); otherwise the window's.
fn set_dark(web: &WKWebView, dark: bool) {
    let color = if dark { NSColor::blackColor() } else { NSColor::windowBackgroundColor() };
    unsafe { web.setUnderPageBackgroundColor(Some(&color)) };
    let a = if dark { NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) } else { None };
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
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImageCompressionFactor};
    let tiff = image.TIFFRepresentation()?;
    let rep = NSBitmapImageRep::imageRepWithData(&tiff)?;
    let quality = objc2_foundation::NSNumber::new_f64(crate::THUMB_QUALITY);
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
