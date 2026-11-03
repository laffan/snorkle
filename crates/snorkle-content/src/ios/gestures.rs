//! UIKit gesture and control targets for content panes: tap to focus a
//! pane (split view), pull to refresh, and the pointer near the window's
//! left edge (the sidebar's edge tab). Each carries only a tab id or a
//! pointer position (native UI events; pages cannot trigger them).

use std::cell::Cell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::NSObjectProtocol;
use objc2_ui_kit::{UIGestureRecognizer, UIGestureRecognizerDelegate, UIGestureRecognizerState};

use super::bindings::WKWebView;
use crate::webkit::delegates::TabIvars;
use crate::{ContentEvents, EDGE_ZONE};

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
pub fn end_refreshing(web: &WKWebView) {
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

pub struct HoverIvars {
    pub events: Rc<dyn ContentEvents>,
    near: Cell<bool>,
    last: Cell<Option<std::time::Instant>>,
}

impl HoverIvars {
    pub fn new(events: Rc<dyn ContentEvents>) -> Self {
        HoverIvars { events, near: Cell::new(false), last: Cell::new(None) }
    }
}

define_class!(
    /// The target of a hover recognizer on the window's root view: reports
    /// the pointer's position while it is near the window's left edge (at
    /// most every 33 ms) and once when it leaves, so the chrome can show
    /// the sidebar's edge tab only near the pointer. Pointer positions
    /// only; it runs alongside every other recognizer.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleHoverEdge"]
    #[ivars = HoverIvars]
    pub struct HoverEdge;

    unsafe impl NSObjectProtocol for HoverEdge {}

    unsafe impl UIGestureRecognizerDelegate for HoverEdge {
        #[unsafe(method(gestureRecognizer:shouldRecognizeSimultaneouslyWithGestureRecognizer:))]
        fn simultaneous(&self, _g: &UIGestureRecognizer, _other: &UIGestureRecognizer) -> bool {
            true
        }
    }

    impl HoverEdge {
        #[unsafe(method(hovered:))]
        fn hovered(&self, g: &UIGestureRecognizer) {
            let i = self.ivars();
            let state = g.state();
            let p = g.locationInView(g.view().as_deref());
            let hovering = matches!(state, UIGestureRecognizerState::Began | UIGestureRecognizerState::Changed);
            if hovering && p.x.is_finite() && p.y.is_finite() && p.x <= EDGE_ZONE {
                let now = std::time::Instant::now();
                if i.near.get() && i.last.get().is_some_and(|t| now.duration_since(t).as_millis() < 33) {
                    return;
                }
                i.near.set(true);
                i.last.set(Some(now));
                i.events.edge_pointer(Some((p.x, p.y)));
            } else if i.near.replace(false) {
                i.events.edge_pointer(None);
            }
        }
    }
);

impl HoverEdge {
    pub fn new(mtm: MainThreadMarker, ivars: HoverIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }
}
