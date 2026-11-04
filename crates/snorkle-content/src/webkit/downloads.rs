//! SECURITY-REVIEW: downloads (owner request 2026-11-03; channel D1 in
//! docs/SECURITY_REVIEW.md). The only place, with the policy hooks in
//! `delegates.rs`, that turns a navigation into a `WKDownload` (the
//! tripwire allows the token in these two files only).
//!
//! Flow: the navigation delegate answers `.download` for a main-frame
//! response WebKit cannot show or that the server marks as an attachment,
//! and for a clicked link with a `download` attribute, when
//! `ContentEvents::may_download` agrees (`policy::may_download`, a rate
//! limit and the storage cap). From a frame inside the page, the decision
//! waits for the user ([`ask_frame`], owner decision 2026-11-04). WebKit then hands us the `WKDownload` (`attach`), asks
//! where to write it, and the app answers with a path in its own staging
//! folder (`downloads/<id>/<name>`, the name cleaned by
//! `storage::clean_download_name`); `None` refuses it.
//!
//! Security assumptions:
//! - Nothing downloaded is opened, previewed, parsed or run by the app. A
//!   file only leaves the staging folder when the user saves or shares it.
//! - The destination is chosen by the app, never by the page: the
//!   suggested name is reduced to a single cleaned path component.
//! - Redirects are followed only to URLs `policy::may_download` accepts;
//!   authentication challenges get the system's default handling.
//! - macOS: a finished file is marked as quarantined (downloaded from the
//!   web by Snorkle), so Gatekeeper checks it if the user later opens it.
//! - All callbacks run on the main thread (`MainThreadOnly` classes).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::{Block, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_foundation::{
    NSHTTPURLResponse, NSObjectProtocol, NSProgressReporting, NSString, NSTimer, NSURLAuthenticationChallenge,
    NSURLCredential, NSURLRequest, NSURLResponse, NSURLSessionAuthChallengeDisposition, NSURL, NSError,
};
use objc2_web_kit::{WKDownload, WKDownloadDelegate, WKDownloadRedirectPolicy};
use snorkle_core::tabs::TabId;

use super::delegates::to_url;
use crate::ContentEvents;

pub struct DownloadIvars {
    tab: TabId,
    events: Rc<dyn ContentEvents>,
    /// Assigned when the app accepts the download (destination chosen).
    id: Cell<Option<u64>>,
    path: RefCell<Option<PathBuf>>,
    source: RefCell<Option<url::Url>>,
}

struct Active {
    download: Retained<WKDownload>,
    _delegate: Retained<DownloadDelegate>,
}

thread_local! {
    /// Downloads WebKit handed us, by address, until a destination is
    /// chosen (the download holds its delegate weakly, so we retain both).
    static PENDING: RefCell<HashMap<usize, Active>> = RefCell::new(HashMap::new());
    /// Accepted downloads, by id.
    static ACTIVE: RefCell<HashMap<u64, Active>> = RefCell::new(HashMap::new());
    /// Reports progress while any download runs.
    static TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
}

/// Downloads from a frame inside a page wait for the user's answer
/// (`ContentEvents::frame_download_requested`); unanswered after this long,
/// they are blocked.
const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Answers WebKit's held decision for a frame download (once).
type Answer = Box<dyn FnOnce(bool)>;

thread_local! {
    /// Frame downloads waiting for the user, by request id.
    static ASKS: RefCell<HashMap<u64, Answer>> = RefCell::new(HashMap::new());
    static NEXT_ASK: Cell<u64> = const { Cell::new(1) };
}

/// A frame inside `tab`'s page wants to download `url` (already accepted
/// by `ContentEvents::may_download`): holds WebKit's decision (`answer`)
/// and asks the user through the app. `answer(true)` starts the download.
pub fn ask_frame(tab: TabId, events: &Rc<dyn ContentEvents>, url: url::Url, answer: Answer) {
    let id = NEXT_ASK.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    ASKS.with(|a| a.borrow_mut().insert(id, answer));
    events.frame_download_requested(tab, id, &url);
    std::thread::spawn(move || {
        std::thread::sleep(ASK_TIMEOUT);
        crate::webkit::dispatch_main(move || answer_frame(id, false));
    });
}

/// The user's answer to a frame download (or the timeout's). Unknown or
/// already answered ids are ignored.
pub fn answer_frame(id: u64, allow: bool) {
    if let Some(answer) = ASKS.with(|a| a.borrow_mut().remove(&id)) {
        answer(allow);
    }
}

/// Progress is reported this often while downloads run.
const POLL_SECONDS: f64 = 0.5;

/// Takes over a download WebKit created from a navigation of `tab`.
pub fn attach(download: &WKDownload, tab: TabId, events: &Rc<dyn ContentEvents>) {
    let mtm = MainThreadMarker::from(download);
    let delegate = DownloadDelegate::alloc(mtm).set_ivars(DownloadIvars {
        tab,
        events: events.clone(),
        id: Cell::new(None),
        path: RefCell::new(None),
        source: RefCell::new(None),
    });
    let delegate: Retained<DownloadDelegate> = unsafe { msg_send![super(delegate), init] };
    unsafe { download.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*delegate))) };
    let key = download as *const WKDownload as usize;
    PENDING.with(|p| p.borrow_mut().insert(key, Active { download: download.retain(), _delegate: delegate }));
}

/// Cancels a running download (the user pressed Cancel). WebKit then
/// reports it as failed, which the app records as cancelled.
pub fn cancel(id: u64) {
    let download = ACTIVE.with(|a| a.borrow().get(&id).map(|x| x.download.clone()));
    if let Some(d) = download {
        unsafe { d.cancel(None) };
    }
}

/// Drops our references once WebKit is done with a download, after the
/// current callback has returned.
fn release(key: usize, id: Option<u64>) {
    crate::webkit::dispatch_main(move || {
        PENDING.with(|p| p.borrow_mut().remove(&key));
        if let Some(id) = id {
            ACTIVE.with(|a| a.borrow_mut().remove(&id));
        }
        if ACTIVE.with(|a| a.borrow().is_empty()) {
            if let Some(t) = TIMER.with(|t| t.borrow_mut().take()) {
                t.invalidate();
            }
        }
    });
}

fn start_polling(events: Rc<dyn ContentEvents>) {
    if TIMER.with(|t| t.borrow().is_some()) {
        return;
    }
    let tick = RcBlock::new(move |_t: NonNull<NSTimer>| {
        let now: Vec<(u64, i64, i64)> = ACTIVE.with(|a| {
            a.borrow()
                .iter()
                .map(|(id, x)| {
                    let p = x.download.progress();
                    (*id, p.completedUnitCount(), p.totalUnitCount())
                })
                .collect()
        });
        for (id, done, total) in now {
            events.download_progress(id, done.max(0) as u64, (total > 0).then_some(total as u64));
        }
    });
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(POLL_SECONDS, true, &tick) };
    TIMER.with(|t| *t.borrow_mut() = Some(timer));
}

/// macOS: marks a downloaded file as quarantined (from the web, by
/// Snorkle), so Gatekeeper checks it when it is opened.
#[cfg(target_os = "macos")]
fn quarantine(path: &std::path::Path, source: Option<&url::Url>) {
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSDictionary, NSURLQuarantinePropertiesKey};
    let file = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let mut keys = vec![NSString::from_str("LSQuarantineAgentName"), NSString::from_str("LSQuarantineType")];
    let mut values: Vec<Retained<AnyObject>> = vec![
        Retained::into_super(Retained::into_super(NSString::from_str("Snorkle"))),
        Retained::into_super(Retained::into_super(NSString::from_str("LSQuarantineTypeWebDownload"))),
    ];
    if let Some(u) = source.filter(|u| matches!(u.scheme(), "http" | "https")) {
        if let Some(n) = NSURL::URLWithString(&NSString::from_str(u.as_str())) {
            keys.push(NSString::from_str("LSQuarantineDataURL"));
            values.push(Retained::into_super(Retained::into_super(n)));
        }
    }
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let value_refs: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
    let dict = NSDictionary::from_slices(&key_refs, &value_refs);
    let _ = unsafe { file.setResourceValue_forKey_error(Some(&dict), NSURLQuarantinePropertiesKey) };
}

#[cfg(not(target_os = "macos"))]
fn quarantine(_path: &std::path::Path, _source: Option<&url::Url>) {}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleDownloadDelegate"]
    #[ivars = DownloadIvars]
    pub struct DownloadDelegate;

    unsafe impl NSObjectProtocol for DownloadDelegate {}

    unsafe impl WKDownloadDelegate for DownloadDelegate {
        /// Where to write the file: the app's staging folder, or nil
        /// (refused, WebKit cancels the download).
        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        fn decide_destination(
            &self,
            download: &WKDownload,
            response: &NSURLResponse,
            suggested: &NSString,
            handler: &Block<dyn Fn(*mut NSURL)>,
        ) {
            let i = self.ivars();
            let source = unsafe { download.originalRequest() }
                .and_then(|r| r.URL())
                .or_else(|| response.URL())
                .as_deref()
                .and_then(to_url);
            let mime = response.MIMEType().map(|m| m.to_string()).unwrap_or_default();
            let expected = u64::try_from(response.expectedContentLength()).ok().filter(|n| *n > 0);
            match i.events.download_destination(i.tab, source.clone(), suggested.to_string(), mime, expected) {
                Some((id, path)) => {
                    i.id.set(Some(id));
                    *i.path.borrow_mut() = Some(path.clone());
                    *i.source.borrow_mut() = source;
                    let key = download as *const WKDownload as usize;
                    if let Some(entry) = PENDING.with(|p| p.borrow_mut().remove(&key)) {
                        ACTIVE.with(|a| a.borrow_mut().insert(id, entry));
                    }
                    start_polling(i.events.clone());
                    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
                    handler.call((Retained::as_ptr(&url) as *mut NSURL,));
                }
                None => {
                    release(download as *const WKDownload as usize, None);
                    handler.call((std::ptr::null_mut(),));
                }
            }
        }

        /// Redirects only to addresses a download may come from.
        #[unsafe(method(download:willPerformHTTPRedirection:newRequest:decisionHandler:))]
        fn redirect(
            &self,
            _download: &WKDownload,
            _response: &NSHTTPURLResponse,
            request: &NSURLRequest,
            handler: &Block<dyn Fn(WKDownloadRedirectPolicy)>,
        ) {
            let ok = request.URL().as_deref().and_then(to_url).is_some_and(|u| snorkle_core::policy::may_download(&u));
            handler.call((if ok { WKDownloadRedirectPolicy::Allow } else { WKDownloadRedirectPolicy::Cancel },));
        }

        /// No credentials are ever supplied; the system decides.
        #[unsafe(method(download:didReceiveAuthenticationChallenge:completionHandler:))]
        fn auth(
            &self,
            _download: &WKDownload,
            _challenge: &NSURLAuthenticationChallenge,
            handler: &Block<dyn Fn(NSURLSessionAuthChallengeDisposition, *mut NSURLCredential)>,
        ) {
            handler.call((NSURLSessionAuthChallengeDisposition::PerformDefaultHandling, std::ptr::null_mut()));
        }

        #[unsafe(method(downloadDidFinish:))]
        fn did_finish(&self, download: &WKDownload) {
            let i = self.ivars();
            if let (Some(id), Some(path)) = (i.id.get(), i.path.borrow().as_ref()) {
                quarantine(path, i.source.borrow().as_ref());
                i.events.download_finished(id, None);
            }
            release(download as *const WKDownload as usize, i.id.get());
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        fn did_fail(&self, download: &WKDownload, error: &NSError, _resume: Option<&objc2_foundation::NSData>) {
            let i = self.ivars();
            if let Some(id) = i.id.get() {
                let msg = snorkle_core::text::sanitize_page_text(&error.localizedDescription().to_string(), 200);
                i.events.download_finished(id, Some(if msg.is_empty() { "The download failed.".into() } else { msg }));
            }
            release(download as *const WKDownload as usize, i.id.get());
        }
    }
);
