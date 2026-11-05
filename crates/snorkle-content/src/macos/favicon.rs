//! SECURITY-REVIEW: favicons for the pinned grid, macOS. The same design
//! as `ios/favicon.rs` (owner decision 2026-10-30, "sandboxed snapshot"):
//! the app process never decodes a favicon file.
//! 1. A hidden `WKWebView` (its own non-persistent data store, JavaScript
//!    off, no user scripts or rule lists), placed outside the window's
//!    visible area, loads a fixed HTML string whose only content is
//!    an image with an escaped HTTPS URL validated by the core. WebKit fetches and decodes the image in
//!    its sandboxed WebContent process.
//! 2. Our navigation delegate allows only that `about:blank` document.
//! 3. When loading finishes, WebKit renders a 64×64 snapshot (an `NSImage`
//!    of WebKit's pixels), which is redrawn into a 128×128, 8-bit RGBA
//!    bitmap and encoded as PNG by AppKit. Only that app-made PNG reaches
//!    storage (which checks its header) and the chrome UI.
//!
//! A cookie-free HTML request discovers a declared HTTPS icon; `/favicon.ico`
//! remains the fallback. Only the escaped image URL is rendered, never the site
//! HTML. Uniform snapshots are rejected using app-rendered pixels, not PNG size.
//! One job at a time with a 20 s deadline, including up to 5 s for discovery.
//! Privacy: the site and its declared icon host learn the device IP; browsing
//! cookies are not shared. Failed jobs may retry after five minutes.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{Bool, NSObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSCalibratedRGBColorSpace, NSGraphicsContext, NSImage, NSView,
};
use objc2_foundation::{
    NSDictionary, NSError, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURLAuthenticationChallenge,
    NSURLCredential, NSURLSessionAuthChallengeDisposition,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKSnapshotConfiguration,
    WKWebView, WKWebViewConfiguration, WKWebsiteDataStore,
};

use crate::FaviconDone;

const SIZE: f64 = 64.0;
const TIMEOUT: Duration = Duration::from_secs(20);

struct Job {
    generation: u64,
    fallback: Option<String>,
    web: Retained<WKWebView>,
    _nav: Retained<FaviconNav>,
    done: FaviconDone,
}

#[derive(Default)]
struct Fetcher {
    queue: VecDeque<(String, FaviconDone)>,
    current: Option<Job>,
    generation: u64,
    root: Option<Retained<NSView>>,
}

thread_local! {
    static FETCHER: RefCell<Fetcher> = RefCell::new(Fetcher::default());
}

/// Queues a favicon fetch for `host` (already validated ASCII). `done` runs
/// on the main thread with PNG bytes, or `None`.
pub fn fetch(mtm: MainThreadMarker, root: &NSView, host: String, done: FaviconDone) {
    let start_now = FETCHER.with(|f| {
        let mut f = f.borrow_mut();
        f.root.get_or_insert_with(|| root.retain());
        f.queue.push_back((host, done));
        f.current.is_none()
    });
    if start_now {
        start_next(mtm);
    }
}

fn start_next(mtm: MainThreadMarker) {
    let next = FETCHER.with(|f| {
        let mut f = f.borrow_mut();
        if f.current.is_some() {
            return None;
        }
        let (host, done) = f.queue.pop_front()?;
        f.generation += 1;
        Some((host, done, f.generation, f.root.clone()?))
    });
    let Some((host, done, generation, root)) = next else { return };
    let job = unsafe { start(mtm, &root, &host, generation, done) };
    FETCHER.with(|f| f.borrow_mut().current = Some(job));
    crate::webkit::http::discover_favicon(
        host,
        Box::new(move |url| {
            crate::webkit::dispatch_main(move || {
                let web = FETCHER.with(|f| {
                    f.borrow().current.as_ref().filter(|j| j.generation == generation).map(|j| j.web.clone())
                });
                if let Some(web) = web {
                    let html = snorkle_core::favicon::document(&url, SIZE as u32);
                    unsafe { web.loadHTMLString_baseURL(&NSString::from_str(&html), None) };
                }
            });
        }),
    );
    std::thread::spawn(move || {
        std::thread::sleep(TIMEOUT);
        crate::webkit::dispatch_main(move || finish(generation, None));
    });
}

unsafe fn start(mtm: MainThreadMarker, root: &NSView, host: &str, generation: u64, done: FaviconDone) -> Job {
    let config = WKWebViewConfiguration::new(mtm);
    config.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
    config.defaultWebpagePreferences().setAllowsContentJavaScript(false);
    // Below and left of the root view's origin: in the window (so WebKit
    // renders it) but outside its visible area.
    let frame = NSRect::new(NSPoint::new(-4.0 * SIZE, -4.0 * SIZE), NSSize::new(SIZE, SIZE));
    let web = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config);
    let nav = FaviconNav::new(mtm, generation);
    web.setNavigationDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*nav)));
    root.addSubview(&web);
    Job { generation, fallback: Some(format!("https://{host}/favicon.ico")), web, _nav: nav, done }
}

fn snapshot(generation: u64) {
    let web =
        FETCHER.with(|f| f.borrow().current.as_ref().filter(|j| j.generation == generation).map(|j| j.web.clone()));
    let Some(web) = web else { return };
    unsafe {
        let mtm = MainThreadMarker::new().expect("main thread");
        let cfg = WKSnapshotConfiguration::new(mtm);
        cfg.setRect(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(SIZE, SIZE)));
        let block = RcBlock::new(move |image: *mut NSImage, _err: *mut NSError| {
            let png = image.as_ref().and_then(plain_png);
            finish_snapshot(generation, png);
        });
        web.takeSnapshotWithConfiguration_completionHandler(Some(&cfg), &block);
    }
}

/// Redraws `image` into a 128×128, 8-bit RGBA bitmap and encodes it as PNG,
/// whatever pixel format the snapshot had.
fn plain_png(image: &NSImage) -> Option<Vec<u8>> {
    const PX: isize = 2 * SIZE as isize;
    unsafe {
        let rep = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            PX,
            PX,
            8,
            4,
            true,
            false,
            NSCalibratedRGBColorSpace,
            0,
            0,
        )?;
        let stride = usize::try_from(rep.bytesPerRow()).ok()?;
        let data = rep.bitmapData();
        if data.is_null() || !(512..=4096).contains(&stride) {
            return None;
        }
        // Transparent image areas must start blank, not with allocator contents.
        std::ptr::write_bytes(data, 0, stride * PX as usize);
        let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&ctx));
        image.drawInRect(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PX as f64, PX as f64)));
        ctx.flushGraphics();
        NSGraphicsContext::restoreGraphicsState_class();
        // The app allocated a 128 x 128, 8-bit, four-channel bitmap above.
        let pixels = std::slice::from_raw_parts(data, stride * PX as usize);
        if !snorkle_core::favicon::has_detail(pixels, PX as usize, PX as usize, stride) {
            return None;
        }

        let png = rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())?;
        Some(png.to_vec())
    }
}

fn finish_snapshot(generation: u64, png: Option<Vec<u8>>) {
    if png.is_none() {
        let retry = FETCHER.with(|f| {
            let mut f = f.borrow_mut();
            let job = f.current.as_mut().filter(|j| j.generation == generation)?;
            Some((job.web.clone(), job.fallback.take()?))
        });
        if let Some((web, url)) = retry {
            let html = snorkle_core::favicon::document(&url, SIZE as u32);
            unsafe { web.loadHTMLString_baseURL(&NSString::from_str(&html), None) };
            return;
        }
    }
    finish(generation, png);
}

fn finish(generation: u64, png: Option<Vec<u8>>) {
    let job = FETCHER.with(|f| {
        let mut f = f.borrow_mut();
        match &f.current {
            Some(j) if j.generation == generation => f.current.take(),
            _ => None,
        }
    });
    let Some(job) = job else { return };
    unsafe {
        job.web.setNavigationDelegate(None);
        job.web.stopLoading();
    }
    job.web.removeFromSuperview();
    (job.done)(png);
    if let Some(mtm) = MainThreadMarker::new() {
        start_next(mtm);
    }
}

pub struct NavIvars {
    generation: Cell<u64>,
}

define_class!(
    /// Allows only the fixed about:blank document; snapshots on finish.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleFaviconNavigation"]
    #[ivars = NavIvars]
    struct FaviconNav;

    unsafe impl NSObjectProtocol for FaviconNav {}

    unsafe impl WKNavigationDelegate for FaviconNav {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(
            &self,
            _w: &WKWebView,
            action: &WKNavigationAction,
            handler: &block2::Block<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let is_blank = unsafe { action.request().URL() }
                .and_then(|u| u.absoluteString())
                .map(|s| s.to_string() == "about:blank")
                .unwrap_or(false);
            let main = unsafe { action.targetFrame() }.map(|f| unsafe { f.isMainFrame() }).unwrap_or(false);
            let policy =
                if is_blank && main { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel };
            handler.call((policy,));
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, _w: &WKWebView, _n: Option<&WKNavigation>) {
            snapshot(self.ivars().generation.get());
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(&self, _w: &WKWebView, _n: Option<&WKNavigation>, _e: &NSError) {
            finish(self.ivars().generation.get(), None);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(&self, _w: &WKWebView, _n: Option<&WKNavigation>, _e: &NSError) {
            finish(self.ivars().generation.get(), None);
        }

        #[unsafe(method(webView:didReceiveAuthenticationChallenge:completionHandler:))]
        fn auth(
            &self,
            _w: &WKWebView,
            _c: &NSURLAuthenticationChallenge,
            handler: &block2::Block<dyn Fn(NSURLSessionAuthChallengeDisposition, *mut NSURLCredential)>,
        ) {
            handler.call((NSURLSessionAuthChallengeDisposition::PerformDefaultHandling, std::ptr::null_mut()));
        }

        #[unsafe(method(webView:authenticationChallenge:shouldAllowDeprecatedTLS:))]
        fn deprecated_tls(
            &self,
            _w: &WKWebView,
            _c: &NSURLAuthenticationChallenge,
            handler: &block2::Block<dyn Fn(Bool)>,
        ) {
            handler.call((Bool::NO,));
        }
    }
);

impl FaviconNav {
    fn new(mtm: MainThreadMarker, generation: u64) -> Retained<Self> {
        let this: Allocated<Self> = Self::alloc(mtm);
        let this = this.set_ivars(NavIvars { generation: Cell::new(generation) });
        unsafe { msg_send![super(this), init] }
    }
}
