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

pub struct ProbeIvars {
    tab: TabIvars,
    /// At the gesture's start: the scroll view's offset, the number of
    /// touches (0 for a trackpad or mouse-wheel scroll) and the modifier
    /// keys UIKit reports as held.
    start: Cell<(f64, f64)>,
    touches: Cell<usize>,
    mods: Cell<objc2_ui_kit::UIKeyModifierFlags>,
}

define_class!(
    /// Scroll probe (debug builds, 2026-11-04; trackpad scrolls reported
    /// sideways or as Back): an extra target on each page's own scroll pan
    /// recognizer, which only observes it. When a pan ends it notes, in the
    /// console, how it was made (trackpad or touch), the modifier keys held,
    /// the direction of the gesture and how far the page moved. Native UI
    /// events only; pages cannot trigger it.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleScrollProbe"]
    #[ivars = ProbeIvars]
    pub struct ScrollProbe;

    unsafe impl NSObjectProtocol for ScrollProbe {}

    impl ScrollProbe {
        #[unsafe(method(panned:))]
        fn panned(&self, g: &UIGestureRecognizer) {
            let i = self.ivars();
            let Some(view) = g.view() else { return };
            let offset: objc2_core_foundation::CGPoint = unsafe { msg_send![&*view, contentOffset] };
            match g.state() {
                UIGestureRecognizerState::Began => {
                    i.start.set((offset.x, offset.y));
                    i.touches.set(g.numberOfTouches());
                    i.mods.set(g.modifierFlags());
                    if let Some(window) = view.window() {
                        let scene: Option<Retained<AnyObject>> = unsafe { msg_send![&*window, windowScene] };
                        let orientation: isize = scene.as_ref().map_or(0, |scene| unsafe {
                            msg_send![&**scene, interfaceOrientation]
                        });
                        // Public UIKit coordinates only. Comparing the window and
                        // view vectors helps distinguish event rotation from layout.
                        i.tab.events.note(&format!(
                            "scroll orientation tab {}: scene {}, interface {} (1 portrait, 2 upside-down, 3 landscape-left, 4 landscape-right)",
                            i.tab.tab.0, scene.is_some(), orientation,
                        ));
                    }
                }
                UIGestureRecognizerState::Ended | UIGestureRecognizerState::Cancelled => {
                    let t: objc2_core_foundation::CGPoint = unsafe { msg_send![g, translationInView: &*view] };
                    if let Some(window) = view.window() {
                        let w: objc2_core_foundation::CGPoint = unsafe { msg_send![g, translationInView: &*window] };
                        i.tab.events.note(&format!(
                            "scroll window vector tab {}: x {:+.0} y {:+.0}", i.tab.tab.0, w.x, w.y,
                        ));
                    }
                    let (sx, sy) = i.start.get();
                    let m = i.mods.get();
                    let mut held = Vec::new();
                    for (flag, name) in [
                        (objc2_ui_kit::UIKeyModifierFlags::Shift, "shift"),
                        (objc2_ui_kit::UIKeyModifierFlags::Command, "cmd"),
                        (objc2_ui_kit::UIKeyModifierFlags::Alternate, "alt"),
                        (objc2_ui_kit::UIKeyModifierFlags::Control, "ctrl"),
                        (objc2_ui_kit::UIKeyModifierFlags::AlphaShift, "capslock"),
                    ] {
                        if m.contains(flag) {
                            held.push(name);
                        }
                    }
                    let how = if i.touches.get() == 0 { "trackpad/mouse" } else { "touch" };
                    let ended = if g.state() == UIGestureRecognizerState::Cancelled { " (cancelled)" } else { "" };
                    i.tab.events.note(&format!(
                        "scroll probe tab {}: {how}{ended}, gesture x {:+.0} y {:+.0}, page moved x {:+.0} y {:+.0}, keys held: {}",
                        i.tab.tab.0,
                        t.x,
                        t.y,
                        offset.x - sx,
                        offset.y - sy,
                        if held.is_empty() { "none".to_string() } else { held.join("+") },
                    ));
                }
                _ => {}
            }
        }
    }
);

impl ScrollProbe {
    pub fn new(mtm: MainThreadMarker, tab: TabIvars) -> Retained<Self> {
        let ivars = ProbeIvars {
            tab,
            start: Cell::new((0.0, 0.0)),
            touches: Cell::new(0),
            mods: Cell::new(objc2_ui_kit::UIKeyModifierFlags::empty()),
        };
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }
}
