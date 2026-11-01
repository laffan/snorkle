//! SECURITY-REVIEW: WebKit delegate implementations for content web views.
//!
//! These are the only code paths through which a content web view reaches
//! native code. Every callback carries metadata produced by WebKit
//! (URL, frame info, navigation type, NSError) rather than a page-chosen
//! message, and forwards it to [`ContentEvents`] without interpretation.
//! There is deliberately no `WKScriptMessageHandler` anywhere.
//!
//! Security assumptions:
//! - WebKit invokes these on the main thread (`MainThreadOnly` classes).
//! - `decisionHandler`/`completionHandler` blocks must be called exactly
//!   once; every path below does so synchronously.
//! - Fail closed: an unparseable URL in a policy callback cancels the
//!   navigation.

use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;

use block2::Block;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{
    NSError, NSObjectProtocol, NSString, NSURLAuthenticationChallenge, NSURLCredential,
    NSURLSessionAuthChallengeDisposition, NSURL,
};
use objc2_ui_kit::{UIGestureRecognizer, UIGestureRecognizerDelegate};
use objc2_web_kit::{
    WKFrameInfo, WKMediaCaptureType, WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
    WKNavigationResponse, WKNavigationResponsePolicy, WKNavigationType, WKPermissionDecision, WKSecurityOrigin,
    WKUIDelegate, WKWebViewConfiguration, WKWindowFeatures,
};
use snorkle_core::browser::{NavErrorKind, NavKind, PopupStyle, Verdict};
use snorkle_core::policy::Frame;
use snorkle_core::tabs::TabId;
use url::Url;

use super::bindings::WKWebView;
use crate::ContentEvents;

pub struct TabIvars {
    pub tab: TabId,
    pub events: Rc<dyn ContentEvents>,
    /// Count of suppressed JS dialogs, to avoid flooding the chrome.
    pub dialogs: Cell<u32>,
}

impl TabIvars {
    pub fn new(tab: TabId, events: Rc<dyn ContentEvents>) -> Self {
        TabIvars { tab, events, dialogs: Cell::new(0) }
    }
}

/// Converts an `NSURL` to a WHATWG-parsed URL; `None` if it does not parse.
pub fn to_url(u: &NSURL) -> Option<Url> {
    let s = u.absoluteString()?;
    Url::parse(&s.to_string()).ok()
}

fn nav_kind(t: WKNavigationType) -> NavKind {
    match t {
        WKNavigationType::LinkActivated => NavKind::LinkActivated,
        WKNavigationType::FormSubmitted => NavKind::FormSubmitted,
        WKNavigationType::BackForward => NavKind::BackForward,
        WKNavigationType::Reload => NavKind::Reload,
        WKNavigationType::FormResubmitted => NavKind::FormResubmitted,
        _ => NavKind::Other,
    }
}

fn classify_error(err: &NSError) -> NavErrorKind {
    let domain = err.domain().to_string();
    let code = err.code();
    match (domain.as_str(), code) {
        ("NSURLErrorDomain", -999) => NavErrorKind::Cancelled,
        // WebKitErrorFrameLoadInterruptedByPolicyChange: our own Cancel.
        ("WebKitErrorDomain", 102) => NavErrorKind::Cancelled,
        // NSURLErrorSecureConnectionFailed … NSURLErrorClientCertificateRequired
        ("NSURLErrorDomain", -1206..=-1200) => NavErrorKind::Certificate,
        ("NSURLErrorDomain", _) => NavErrorKind::Network,
        _ => NavErrorKind::Other,
    }
}

fn failing_url(err: &NSError) -> Option<Url> {
    let info = err.userInfo();
    let key = NSString::from_str("NSErrorFailingURLStringKey");
    let value = info.objectForKey(&key)?;
    let s = value.downcast_ref::<NSString>()?;
    Url::parse(&s.to_string()).ok()
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleNavigationDelegate"]
    #[ivars = TabIvars]
    pub struct NavDelegate;

    unsafe impl NSObjectProtocol for NavDelegate {}

    unsafe impl WKNavigationDelegate for NavDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_policy(
            &self,
            _web_view: &WKWebView,
            action: &WKNavigationAction,
            handler: &Block<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let policy = unsafe { self.policy_for(action) };
            handler.call((policy,));
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(
            &self,
            _web_view: &WKWebView,
            response: &WKNavigationResponse,
            handler: &Block<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            // Downloads are not supported yet: anything WebKit cannot render
            // is cancelled rather than handed to a download path.
            let can_show = unsafe { response.canShowMIMEType() };
            let policy = if can_show { WKNavigationResponsePolicy::Allow } else { WKNavigationResponsePolicy::Cancel };
            handler.call((policy,));
        }

        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn did_start(&self, _web_view: &WKWebView, _nav: Option<&WKNavigation>) {
            self.ivars().events.provisional_start(self.ivars().tab);
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn did_commit(&self, web_view: &WKWebView, _nav: Option<&WKNavigation>) {
            if let Some(url) = unsafe { web_view.URL() }.as_deref().and_then(to_url) {
                self.ivars().events.commit(self.ivars().tab, url);
            }
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, web_view: &WKWebView, _nav: Option<&WKNavigation>) {
            end_refreshing(web_view);
            self.ivars().events.finish(self.ivars().tab);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(&self, web_view: &WKWebView, _nav: Option<&WKNavigation>, error: &NSError) {
            end_refreshing(web_view);
            let i = self.ivars();
            i.events.fail(
                i.tab,
                true,
                classify_error(error),
                failing_url(error),
                error.localizedDescription().to_string(),
            );
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(&self, web_view: &WKWebView, _nav: Option<&WKNavigation>, error: &NSError) {
            end_refreshing(web_view);
            let i = self.ivars();
            i.events.fail(
                i.tab,
                false,
                classify_error(error),
                failing_url(error),
                error.localizedDescription().to_string(),
            );
        }

        /// Certificate policy: always the system's default evaluation. There
        /// is no code path that accepts an untrusted certificate or supplies
        /// a credential.
        #[unsafe(method(webView:didReceiveAuthenticationChallenge:completionHandler:))]
        fn auth_challenge(
            &self,
            _web_view: &WKWebView,
            _challenge: &NSURLAuthenticationChallenge,
            handler: &Block<dyn Fn(NSURLSessionAuthChallengeDisposition, *mut NSURLCredential)>,
        ) {
            handler.call((NSURLSessionAuthChallengeDisposition::PerformDefaultHandling, std::ptr::null_mut()));
        }

        /// TLS 1.0/1.1: never allowed.
        #[unsafe(method(webView:authenticationChallenge:shouldAllowDeprecatedTLS:))]
        fn deprecated_tls(
            &self,
            _web_view: &WKWebView,
            _challenge: &NSURLAuthenticationChallenge,
            handler: &Block<dyn Fn(Bool)>,
        ) {
            handler.call((Bool::NO,));
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn process_terminated(&self, _web_view: &WKWebView) {
            self.ivars().events.process_terminated(self.ivars().tab);
        }
    }
);

impl NavDelegate {
    pub fn new(mtm: MainThreadMarker, ivars: TabIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    unsafe fn policy_for(&self, action: &WKNavigationAction) -> WKNavigationActionPolicy {
        // Downloads are not supported yet.
        if action.respondsToSelector(objc2::sel!(shouldPerformDownload)) && action.shouldPerformDownload() {
            return WKNavigationActionPolicy::Cancel;
        }
        let Some(url) = action.request().URL().as_deref().and_then(to_url) else {
            return WKNavigationActionPolicy::Cancel;
        };
        let frame = match action.targetFrame() {
            // No target frame: a new-window request. WebKit follows up with
            // `createWebViewWithConfiguration`, which runs the policy itself
            // and never creates a web view, so nothing can load here.
            None => return WKNavigationActionPolicy::Allow,
            Some(f) if f.isMainFrame() => Frame::Main,
            Some(_) => Frame::Sub,
        };
        let i = self.ivars();
        match i.events.decide_navigation(i.tab, &url, frame, nav_kind(action.navigationType())) {
            Verdict::Allow => WKNavigationActionPolicy::Allow,
            Verdict::Cancel => WKNavigationActionPolicy::Cancel,
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleUIDelegate"]
    #[ivars = TabIvars]
    pub struct UiDelegate;

    unsafe impl NSObjectProtocol for UiDelegate {}

    unsafe impl WKUIDelegate for UiDelegate {
        /// target=_blank / window.open. The core runs the navigation policy
        /// and creates a tab or sheet; we then return a real web view built
        /// from WebKit's `configuration` (required by WebKit), so
        /// `window.opener` works for OAuth/payment popups. WebKit itself
        /// applies `noopener` where the page or link asks for it. Returning
        /// nil cancels. A window opened with a size (`window.open(url, name,
        /// "width=…")`, as sign-in buttons do) is shown as a sheet over its
        /// opener; anything else as a tab.
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(
            &self,
            _web_view: &WKWebView,
            config: &WKWebViewConfiguration,
            action: &WKNavigationAction,
            features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            let sized = unsafe { features.width().is_some() || features.height().is_some() };
            self.popup_for(config, action, if sized { PopupStyle::Sheet } else { PopupStyle::Tab })
        }

        /// `window.close()`; the core only honours it for popups.
        #[unsafe(method(webViewDidClose:))]
        fn did_close(&self, _web_view: &WKWebView) {
            self.ivars().events.close_requested(self.ivars().tab);
        }

        // JavaScript dialogs are suppressed for now (no trusted dialog UI
        // yet; see docs/OPEN_QUESTIONS.md). The chrome is told so it can show
        // a non-blocking note attributed to the frame's origin.
        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn alert(&self, _web_view: &WKWebView, _message: &NSString, frame: &WKFrameInfo, handler: &Block<dyn Fn()>) {
            self.note_dialog(frame);
            handler.call(());
        }

        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn confirm(
            &self,
            _web_view: &WKWebView,
            _message: &NSString,
            frame: &WKFrameInfo,
            handler: &Block<dyn Fn(Bool)>,
        ) {
            self.note_dialog(frame);
            handler.call((Bool::NO,));
        }

        #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
        fn prompt(
            &self,
            _web_view: &WKWebView,
            _prompt: &NSString,
            _default: Option<&NSString>,
            frame: &WKFrameInfo,
            handler: &Block<dyn Fn(*mut NSString)>,
        ) {
            self.note_dialog(frame);
            handler.call((std::ptr::null_mut(),));
        }

        /// Camera/microphone: defer to WebKit's own system prompt, which
        /// names the requesting origin.
        #[unsafe(method(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:))]
        fn media_permission(
            &self,
            _web_view: &WKWebView,
            _origin: &WKSecurityOrigin,
            _frame: &WKFrameInfo,
            _kind: WKMediaCaptureType,
            handler: &Block<dyn Fn(WKPermissionDecision)>,
        ) {
            handler.call((WKPermissionDecision::Prompt,));
        }

        #[unsafe(method(webView:requestDeviceOrientationAndMotionPermissionForOrigin:initiatedByFrame:decisionHandler:))]
        fn motion_permission(
            &self,
            _web_view: &WKWebView,
            _origin: &WKSecurityOrigin,
            _frame: &WKFrameInfo,
            handler: &Block<dyn Fn(WKPermissionDecision)>,
        ) {
            handler.call((WKPermissionDecision::Prompt,));
        }
    }
);

impl UiDelegate {
    fn popup_for(
        &self,
        config: &WKWebViewConfiguration,
        action: &WKNavigationAction,
        style: PopupStyle,
    ) -> Option<Retained<WKWebView>> {
        let i = self.ivars();
        let raw = unsafe { action.request().URL() };
        let url = raw.as_deref().and_then(to_url);
        // A URL that is present but does not parse is refused (fail closed);
        // no URL at all means an about:blank popup.
        if raw.is_some() && url.is_none() {
            return None;
        }
        let tab = i.events.popup_requested(i.tab, url, style)?;
        Some(unsafe { super::create_popup(self.mtm(), tab, config, &i.events) })
    }

    pub fn new(mtm: MainThreadMarker, ivars: TabIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    fn note_dialog(&self, frame: &WKFrameInfo) {
        let i = self.ivars();
        let n = i.dialogs.get().saturating_add(1);
        i.dialogs.set(n);
        if n <= 20 {
            let host = unsafe { frame.securityOrigin().host() }.to_string();
            i.events.dialog_suppressed(i.tab, host);
        }
    }
}

/// Key paths observed on every content `WKWebView`.
pub const KVO_KEYS: &[&str] =
    &["URL", "title", "estimatedProgress", "canGoBack", "canGoForward", "hasOnlySecureContent"];

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleWebViewObserver"]
    #[ivars = TabIvars]
    pub struct Observer;

    unsafe impl NSObjectProtocol for Observer {}

    impl Observer {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe(
            &self,
            key_path: Option<&NSString>,
            object: Option<&AnyObject>,
            _change: Option<&AnyObject>,
            _context: *mut c_void,
        ) {
            let (Some(key), Some(web)) = (key_path, object.and_then(|o| o.downcast_ref::<WKWebView>())) else {
                return;
            };
            let i = self.ivars();
            unsafe {
                match key.to_string().as_str() {
                    "URL" => {
                        if let Some(u) = web.URL().as_deref().and_then(to_url) {
                            i.events.url_changed(i.tab, u);
                        }
                    }
                    "title" => i.events.title_changed(i.tab, web.title().map(|t| t.to_string()).unwrap_or_default()),
                    "estimatedProgress" => i.events.progress(i.tab, web.estimatedProgress()),
                    "canGoBack" | "canGoForward" => i.events.history_state(i.tab, web.canGoBack(), web.canGoForward()),
                    "hasOnlySecureContent" => i.events.secure_content(i.tab, web.hasOnlySecureContent()),
                    _ => {}
                }
            }
        }
    }
);

impl Observer {
    pub fn new(mtm: MainThreadMarker, ivars: TabIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    /// Tap on a content pane → focus that tab (split view). Recognises
    /// simultaneously with WebKit's own gestures and never cancels touches,
    /// so the page still receives the tap. Carries only the tab id.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleTapFocus"]
    #[ivars = TabIvars]
    pub struct TapFocus;

    unsafe impl NSObjectProtocol for TapFocus {}

    unsafe impl UIGestureRecognizerDelegate for TapFocus {
        #[unsafe(method(gestureRecognizer:shouldRecognizeSimultaneouslyWithGestureRecognizer:))]
        fn simultaneous(&self, _g: &UIGestureRecognizer, _other: &UIGestureRecognizer) -> bool {
            true
        }
    }

    impl TapFocus {
        #[unsafe(method(tapped:))]
        fn tapped(&self, _g: &UIGestureRecognizer) {
            self.ivars().events.focus_requested(self.ivars().tab);
        }
    }
);

/// Stops the pull-to-refresh spinner once the load it started ends.
fn end_refreshing(web: &WKWebView) {
    let control = unsafe { web.scrollView() }.refreshControl();
    if let Some(c) = control.filter(|c| c.isRefreshing()) {
        c.endRefreshing();
    }
}

define_class!(
    /// Pull to refresh: the refresh control's action asks the core to
    /// reload the tab (a native UI event; pages cannot trigger it).
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleRefreshTarget"]
    #[ivars = TabIvars]
    pub struct RefreshTarget;

    unsafe impl NSObjectProtocol for RefreshTarget {}

    impl RefreshTarget {
        #[unsafe(method(refresh:))]
        fn refresh(&self, control: &NSObject) {
            self.ivars().events.reload_requested(self.ivars().tab);
            // Safety net: if no load starts (e.g. the tab shows a notice),
            // no finish or fail arrives to stop the spinner.
            let none: *const AnyObject = std::ptr::null();
            let _: () = unsafe {
                msg_send![control, performSelector: sel!(endRefreshing), withObject: none, afterDelay: 15.0f64]
            };
        }
    }
);

impl RefreshTarget {
    pub fn new(mtm: MainThreadMarker, ivars: TabIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }
}

impl TapFocus {
    pub fn new(mtm: MainThreadMarker, ivars: TabIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }
}
