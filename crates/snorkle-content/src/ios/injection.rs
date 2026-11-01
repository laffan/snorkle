//! SECURITY-REVIEW: the ONLY place that injects scripts into content web
//! views (owner-approved 2026-10-29 for user scripts/styles and
//! black-and-white mode; see docs/SECURITY_REVIEW.md) and the only place
//! with a script message handler (the console, owner request 2026-10-30,
//! channel C13). The tripwire tests allow `WKUserScript`, `addUserScript`,
//! `evaluateJavaScript`, `WKScriptMessageHandler`,
//! `addScriptMessageHandler` and `pageWorld` in this file only.
//!
//! Security assumptions:
//! - Sources come from `snorkle_core::userscripts::injections` /
//!   `view_mode_js` (validated, guarded, literals escaped) and the constant
//!   `snorkle_core::console::PAGE_HOOK_JS`.
//! - User scripts and view modes run in WebKit's isolated default client
//!   world, main frame only, so pages cannot see or call them, and cannot
//!   send anything back. `evaluateJavaScript` is called without a
//!   completion handler, so no page-influenced value is ever read back.
//! - Exception, only while console capture is on: the console hook runs in
//!   the page world of every frame and posts to one message handler, which
//!   pages can also call directly. The handler accepts string bodies only
//!   and passes them, with WebKit's frame origin and the sending tab, to
//!   `ContentEvents::console_message`, which only records them as text
//!   (sanitised, size- and rate-limited in `snorkle_core::console`). When
//!   capture is off the handler and hook are removed from every controller.
//! - Headers view (channel C14, while headers mode is on for a tab): the
//!   extractor (`snorkle_core::viewmodes`) runs in the client world and posts
//!   to a handler registered in the client world only, so pages cannot call
//!   it. String bodies only, passed to `ContentEvents::headings`, which
//!   parses them strictly (`viewmodes::parse_headings`).

#![allow(non_snake_case)]

use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{define_class, extern_methods, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSError, NSObjectProtocol, NSString};
use objc2_web_kit::{
    WKContentWorld, WKFrameInfo, WKScriptMessage, WKScriptMessageHandler, WKUserContentController, WKUserScript,
    WKUserScriptInjectionTime,
};
use snorkle_core::console::{PAGE_HANDLER_NAME, PAGE_HOOK_JS};
use snorkle_core::viewmodes::HEADINGS_HANDLER_NAME;
use snorkle_core::userscripts::{InjectAt, Injection};

use super::bindings::WKWebView;
use crate::ContentEvents;

impl WKWebView {
    extern_methods!(
        /// iOS 14+. Kept here (not in bindings.rs) so script evaluation is
        /// confined to this file.
        #[unsafe(method(evaluateJavaScript:inFrame:inContentWorld:completionHandler:))]
        #[unsafe(method_family = none)]
        unsafe fn evaluateJavaScript_inFrame_inContentWorld_completionHandler(
            &self,
            source: &NSString,
            frame: Option<&WKFrameInfo>,
            world: &WKContentWorld,
            completion: Option<&block2::DynBlock<dyn Fn(*mut AnyObject, *mut NSError)>>,
        );
    );
}

/// Compiled user scripts, opaque outside this module.
#[derive(Default)]
pub struct Scripts(Vec<Retained<WKUserScript>>, Option<Console>, Option<Retained<HeadingsHandler>>);

/// The console hook and its handler, present only while capture is on.
struct Console {
    hook: Retained<WKUserScript>,
    handler: Retained<ConsoleHandler>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleConsoleHandler"]
    #[ivars = Rc<dyn ContentEvents>]
    struct ConsoleHandler;

    unsafe impl NSObjectProtocol for ConsoleHandler {}

    unsafe impl WKScriptMessageHandler for ConsoleHandler {
        /// Page console output (or anything a page posts to the handler).
        /// String bodies only; attribution comes from WebKit.
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(&self, _controller: &WKUserContentController, message: &WKScriptMessage) {
            unsafe {
                let body = message.body();
                let Some(text) = body.downcast_ref::<NSString>() else { return };
                let frame = message.frameInfo();
                let host = frame.securityOrigin().host().to_string();
                let web: *const AnyObject = msg_send![message, webView];
                let tab = super::tab_for_web_view(web);
                self.ivars().console_message(tab, host, frame.isMainFrame(), text.to_string());
            }
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleHeadingsHandler"]
    #[ivars = Rc<dyn ContentEvents>]
    struct HeadingsHandler;

    unsafe impl NSObjectProtocol for HeadingsHandler {}

    unsafe impl WKScriptMessageHandler for HeadingsHandler {
        /// The headers extractor's report (client world only). String bodies only.
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(&self, _controller: &WKUserContentController, message: &WKScriptMessage) {
            unsafe {
                if !message.frameInfo().isMainFrame() {
                    return;
                }
                let body = message.body();
                let Some(text) = body.downcast_ref::<NSString>() else { return };
                let web: *const AnyObject = msg_send![message, webView];
                let tab = super::tab_for_web_view(web);
                self.ivars().headings(tab, text.to_string());
            }
        }
    }
);

impl HeadingsHandler {
    fn new(mtm: MainThreadMarker, events: Rc<dyn ContentEvents>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(events);
        unsafe { msg_send![super(this), init] }
    }
}

impl ConsoleHandler {
    fn new(mtm: MainThreadMarker, events: Rc<dyn ContentEvents>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(events);
        unsafe { msg_send![super(this), init] }
    }
}

/// Builds main-frame-only user scripts in the default client world, plus
/// the console hook (page world, all frames) when `console` is given and
/// the headings handler (client world) when `headings` is given.
pub fn make_scripts(
    mtm: MainThreadMarker,
    injections: &[Injection],
    console: Option<&Rc<dyn ContentEvents>>,
    headings: Option<&Rc<dyn ContentEvents>>,
) -> Scripts {
    let headings = headings.map(|events| HeadingsHandler::new(mtm, events.clone()));
    let world = unsafe { WKContentWorld::defaultClientWorld(mtm) };
    let console = console.map(|events| unsafe {
        Console {
            hook: WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
                WKUserScript::alloc(mtm),
                &NSString::from_str(PAGE_HOOK_JS),
                WKUserScriptInjectionTime::AtDocumentStart,
                false,
                &WKContentWorld::pageWorld(mtm),
            ),
            handler: ConsoleHandler::new(mtm, events.clone()),
        }
    });
    let scripts: Vec<_> = injections
        .iter()
        .map(|i| {
            let at = match i.at {
                InjectAt::DocumentStart => WKUserScriptInjectionTime::AtDocumentStart,
                InjectAt::DocumentEnd => WKUserScriptInjectionTime::AtDocumentEnd,
            };
            unsafe {
                WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
                    WKUserScript::alloc(mtm),
                    &NSString::from_str(&i.source),
                    at,
                    !i.all_frames,
                    &world,
                )
            }
        })
        .collect();
    Scripts(scripts, console, headings)
}

/// Replaces all user scripts (and the console handler) on a web view's
/// controller. Scripts take effect on the next page load. Popups share
/// their opener's controller, so this runs more than once per controller:
/// the handler is always removed before it is added (adding a name twice
/// raises an exception).
pub fn install(mtm: MainThreadMarker, controller: &WKUserContentController, scripts: &Scripts) {
    unsafe {
        let page = WKContentWorld::pageWorld(mtm);
        let client = WKContentWorld::defaultClientWorld(mtm);
        let name = NSString::from_str(PAGE_HANDLER_NAME);
        let headings_name = NSString::from_str(HEADINGS_HANDLER_NAME);
        controller.removeAllUserScripts();
        controller.removeScriptMessageHandlerForName_contentWorld(&name, &page);
        controller.removeScriptMessageHandlerForName_contentWorld(&headings_name, &client);
        if let Some(c) = &scripts.1 {
            controller.addScriptMessageHandler_contentWorld_name(ProtocolObject::from_ref(&*c.handler), &page, &name);
            controller.addUserScript(&c.hook);
        }
        if let Some(h) = &scripts.2 {
            controller.addScriptMessageHandler_contentWorld_name(ProtocolObject::from_ref(&**h), &client, &headings_name);
        }
        for s in &scripts.0 {
            controller.addUserScript(s);
        }
    }
}

/// Runs constant code in an already-loaded page's main frame, client world,
/// without reading any result.
pub fn run_now(mtm: MainThreadMarker, web: &WKWebView, source: &str) {
    unsafe {
        let world = WKContentWorld::defaultClientWorld(mtm);
        web.evaluateJavaScript_inFrame_inContentWorld_completionHandler(&NSString::from_str(source), None, &world, None);
    }
}
