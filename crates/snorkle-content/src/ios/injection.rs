//! SECURITY-REVIEW: the ONLY place that injects scripts into content web
//! views (owner-approved 2026-10-29 for user scripts/styles and
//! black-and-white mode; see docs/SECURITY_REVIEW.md). The tripwire tests
//! allow `WKUserScript`, `addUserScript` and `evaluateJavaScript` in this
//! file only.
//!
//! Security assumptions:
//! - Sources come from `snorkle_core::userscripts::injections` /
//!   `view_mode_js` (validated, guarded, literals escaped).
//! - Everything runs in WebKit's isolated default client world, main frame
//!   only, so pages cannot see or call the injected code.
//! - No script message handler exists: injected code has no way to send
//!   anything back to native code. `evaluateJavaScript` is called without a
//!   completion handler, so no page-influenced value is ever read back.

#![allow(non_snake_case)]

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{extern_methods, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSError, NSString};
use objc2_web_kit::{WKContentWorld, WKFrameInfo, WKUserContentController, WKUserScript, WKUserScriptInjectionTime};
use snorkle_core::userscripts::{InjectAt, Injection};

use super::bindings::WKWebView;

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
pub struct Scripts(Vec<Retained<WKUserScript>>);

/// Builds main-frame-only user scripts in the default client world.
pub fn make_scripts(mtm: MainThreadMarker, injections: &[Injection]) -> Scripts {
    let world = unsafe { WKContentWorld::defaultClientWorld(mtm) };
    injections
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
                    true,
                    &world,
                )
            }
        })
        .collect::<Vec<_>>()
        .into()
}

impl From<Vec<Retained<WKUserScript>>> for Scripts {
    fn from(v: Vec<Retained<WKUserScript>>) -> Self {
        Scripts(v)
    }
}

/// Replaces all user scripts on a web view's controller. Takes effect on the
/// next page load.
pub fn install(controller: &WKUserContentController, scripts: &Scripts) {
    unsafe {
        controller.removeAllUserScripts();
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
