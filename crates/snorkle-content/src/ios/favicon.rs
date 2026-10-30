//! SECURITY-REVIEW: favicons for the pinned grid (owner decision 2026-10-30:
//! "sandboxed snapshot").
//!
//! Favicons are attacker-controlled image files, and image decoders are a
//! classic exploit target, so the app process never decodes one. Instead:
//! 1. A hidden, off-screen `WKWebView` with its own non-persistent data
//!    store (no cookies or site data shared with browsing), JavaScript
//!    disabled, no user scripts and no rule lists loads a fixed HTML string
//!    whose only content is `<img src="https://<host>/favicon.ico">`. The
//!    host was validated to `[a-z0-9.-]` by the caller, so it cannot inject
//!    markup. WebKit fetches and decodes the image in its sandboxed
//!    WebContent process.
//! 2. Our navigation delegate allows only that `about:blank` document.
//! 3. When loading finishes, WebKit renders a 64×64 snapshot; the app
//!    receives decoded pixels (a `UIImage` backed by WebKit's bitmap),
//!    redraws them into a standard-range 8-bit sRGB bitmap (128×128 px)
//!    and encodes that as PNG with UIKit. Only that app-made PNG reaches
//!    storage (which checks its header) and the chrome UI. The redraw
//!    matters: snapshots on wide-colour screens can use pixel formats the
//!    chrome then failed to display.
//!
//! A missing favicon renders as nothing (`alt=""`), giving a transparent
//! snapshot; PNGs below `MIN_ICON_BYTES` are treated as "no icon" (a
//! heuristic) and the chrome shows a letter tile.
//!
//! One fetch at a time, with a 15 s timeout. Privacy: each pinned site
//! learns the device's IP when its favicon is fetched (once, then cached).

#![allow(non_snake_case)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ptr::NonNull;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{Bool, NSObject};
use objc2::{define_class, extern_methods, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSError, NSObjectProtocol, NSString, NSURLAuthenticationChallenge, NSURLCredential, NSURLSessionAuthChallengeDisposition,
    NSURL,
};
use objc2_ui_kit::{
    UIColor, UIGraphicsImageRenderer, UIGraphicsImageRendererContext, UIGraphicsImageRendererFormat,
    UIGraphicsImageRendererFormatRange, UIImage, UIView,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKSnapshotConfiguration,
    WKWebViewConfiguration, WKWebsiteDataStore,
};

use super::bindings::WKWebView;
use crate::FaviconDone;

const SIZE: f64 = 64.0;
const TIMEOUT: Duration = Duration::from_secs(15);
/// Snapshots smaller than this (as PNG) are almost certainly blank.
const MIN_ICON_BYTES: usize = 220;

impl WKWebView {
    extern_methods!(
        #[unsafe(method(loadHTMLString:baseURL:))]
        #[unsafe(method_family = none)]
        unsafe fn loadHTMLString_baseURL(&self, html: &NSString, base: Option<&NSURL>) -> Option<Retained<WKNavigation>>;

        #[unsafe(method(takeSnapshotWithConfiguration:completionHandler:))]
        #[unsafe(method_family = none)]
        unsafe fn takeSnapshotWithConfiguration_completionHandler(
            &self,
            config: Option<&WKSnapshotConfiguration>,
            completion: &block2::DynBlock<dyn Fn(*mut UIImage, *mut NSError)>,
        );
    );
}

struct Job {
    generation: u64,
    web: Retained<WKWebView>,
    _nav: Retained<FaviconNav>,
    done: FaviconDone,
}

#[derive(Default)]
struct Fetcher {
    queue: VecDeque<(String, FaviconDone)>,
    current: Option<Job>,
    generation: u64,
    root: Option<Retained<UIView>>,
}

thread_local! {
    static FETCHER: RefCell<Fetcher> = RefCell::new(Fetcher::default());
}

/// Queues a favicon fetch for `host` (already validated ASCII). `done` runs
/// on the main thread with PNG bytes, or `None`.
pub fn fetch(mtm: MainThreadMarker, root: &UIView, host: String, done: FaviconDone) {
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
    std::thread::spawn(move || {
        std::thread::sleep(TIMEOUT);
        super::dispatch_main(move || finish(generation, None));
    });
}

unsafe fn start(mtm: MainThreadMarker, root: &UIView, host: &str, generation: u64, done: FaviconDone) -> Job {
    let config = WKWebViewConfiguration::new(mtm);
    config.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
    config.defaultWebpagePreferences().setAllowsContentJavaScript(false);
    // Off-screen but inside the window, so WebKit renders it.
    let frame = CGRect::new(CGPoint::new(-4.0 * SIZE, -4.0 * SIZE), CGSize::new(SIZE, SIZE));
    let web = WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config);
    web.setOpaque(false);
    web.setBackgroundColor(Some(&UIColor::clearColor()));
    web.setUserInteractionEnabled(false);
    let nav = FaviconNav::new(mtm, generation);
    web.setNavigationDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*nav)));
    root.addSubview(&web);
    // `host` is [a-z0-9.-] only (validated by the caller), so it is inert in
    // both the attribute and the URL.
    let html = format!(
        "<!doctype html><meta name=viewport content=\"width={s}\"><style>html,body{{margin:0;background:transparent}}img{{display:block;width:{s}px;height:{s}px;object-fit:contain}}</style><img alt=\"\" src=\"https://{host}/favicon.ico\">",
        s = SIZE as u32
    );
    web.loadHTMLString_baseURL(&NSString::from_str(&html), None);
    Job { generation, web, _nav: nav, done }
}

fn snapshot(generation: u64) {
    let web = FETCHER.with(|f| f.borrow().current.as_ref().filter(|j| j.generation == generation).map(|j| j.web.clone()));
    let Some(web) = web else { return };
    unsafe {
        let mtm = MainThreadMarker::new().expect("main thread");
        let cfg = WKSnapshotConfiguration::new(mtm);
        cfg.setRect(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(SIZE, SIZE)));
        cfg.setAfterScreenUpdates(true);
        let block = RcBlock::new(move |image: *mut UIImage, _err: *mut NSError| {
            let png = image.as_ref().map(plain_png).filter(|bytes| bytes.len() >= MIN_ICON_BYTES);
            finish(generation, png);
        });
        web.takeSnapshotWithConfiguration_completionHandler(Some(&cfg), &block);
    }
}

/// Redraws `image` into a standard-range (8-bit sRGB) bitmap at 2× and
/// encodes it as PNG, whatever pixel format the snapshot had.
fn plain_png(image: &UIImage) -> Vec<u8> {
    let format = UIGraphicsImageRendererFormat::preferredFormat();
    format.setScale(2.0);
    format.setOpaque(false);
    format.setPreferredRange(UIGraphicsImageRendererFormatRange::Standard);
    let renderer = UIGraphicsImageRenderer::initWithSize_format(
        UIGraphicsImageRenderer::alloc(),
        CGSize::new(SIZE, SIZE),
        &format,
    );
    let image = image.retain();
    let draw = RcBlock::new(move |_ctx: NonNull<UIGraphicsImageRendererContext>| {
        image.drawInRect(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(SIZE, SIZE)));
    });
    unsafe { renderer.PNGDataWithActions(RcBlock::as_ptr(&draw)) }.to_vec()
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
        fn decide(&self, _w: &WKWebView, action: &WKNavigationAction, handler: &block2::Block<dyn Fn(WKNavigationActionPolicy)>) {
            let is_blank = unsafe { action.request().URL() }
                .and_then(|u| u.absoluteString())
                .map(|s| s.to_string() == "about:blank")
                .unwrap_or(false);
            let main = unsafe { action.targetFrame() }.map(|f| unsafe { f.isMainFrame() }).unwrap_or(false);
            let policy = if is_blank && main { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel };
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
        fn deprecated_tls(&self, _w: &WKWebView, _c: &NSURLAuthenticationChallenge, handler: &block2::Block<dyn Fn(Bool)>) {
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
