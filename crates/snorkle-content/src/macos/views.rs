//! SECURITY-REVIEW: the AppKit views content web views live in, and the
//! cut-outs for chrome that floats over pages (`crate::Hole`).
//!
//! - [`ContentRoot`] covers the window's content view, above the chrome's
//!   web view. It draws nothing and is not itself hit: a click that lands
//!   on it and not on one of its subviews (a page container or the sheet
//!   backdrop) goes to the chrome underneath.
//! - [`Container`] holds one tab's web view (or is the sheet backdrop). It
//!   is not hit inside a hole, so clicks there reach the chrome, and its
//!   layer is masked with the holes cut out, so a page is never drawn
//!   over or under the URL pill, its popups, toasts or the sidebar tab.
//!
//! Geometry: every view here is unflipped (AppKit's default, origin at the
//! bottom left), as is the window's content view, so a layer's coordinates
//! match its view's. The chrome reports rectangles from the top left
//! (CSS); [`Tl`] is such a rectangle and [`Tl::ns`] converts it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSEvent, NSResponder, NSView};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGMutablePath, CGPath};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::{
    kCAFillRuleEvenOdd, kCAMediaTimingFunctionEaseInEaseOut, CABasicAnimation, CACurrentMediaTime, CAMediaTiming,
    CAMediaTimingFunction, CAShapeLayer, CATransaction,
};
use snorkle_core::tabs::TabId;

use crate::Hole;

/// The current holes, shared by the host and every container.
pub type Holes = Rc<RefCell<Vec<Hole>>>;

/// A rectangle measured from the top left of the root view, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tl {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Tl {
    /// The same rectangle in AppKit's coordinates of a view `height` tall.
    pub fn ns(self, height: f64) -> NSRect {
        NSRect::new(NSPoint::new(self.x, height - self.y - self.h), NSSize::new(self.w, self.h))
    }
}

/// Length of the chrome's sidebar slide (CSS `.25s ease-in-out`).
pub const GLIDE_SECONDS: f64 = 0.25;

pub struct RootIvars {
    /// The pointer left the window (the edge tab hides again).
    pub on_exit: RefCell<Option<Box<dyn Fn()>>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleContentRoot"]
    #[ivars = RootIvars]
    pub struct ContentRoot;

    impl ContentRoot {
        /// Only its subviews take clicks; elsewhere the chrome below does.
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: point] };
            hit.filter(|v| !std::ptr::eq(Retained::as_ptr(v), (self as *const Self).cast()))
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            if let Some(f) = self.ivars().on_exit.borrow().as_ref() {
                f();
            }
        }
    }
);

impl ContentRoot {
    pub fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RootIvars { on_exit: RefCell::new(None) });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

pub struct ContainerIvars {
    holes: Holes,
    /// The tab whose page this holds (`None`: the sheet backdrop).
    pub tab: Option<TabId>,
    /// Its frame as last placed, from the top left (for the mask and for
    /// hit-testing, which works in the root's coordinates).
    pub placed: Cell<Tl>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SnorkleContentContainer"]
    #[ivars = ContainerIvars]
    pub struct Container;

    impl Container {
        /// Not hit inside a hole. `point` is in the root's coordinates.
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            let height = unsafe { self.superview() }.map(|s| s.bounds().size.height).unwrap_or(0.0);
            let (x, y) = (point.x, height - point.y);
            if self.ivars().holes.borrow().iter().any(|h| h.contains(x, y)) {
                None
            } else {
                unsafe { msg_send![super(self), hitTest: point] }
            }
        }

        /// A click on the backdrop (or the container's own margin) goes
        /// nowhere: not to the page below and not to the chrome.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {}
    }
);

impl Container {
    pub fn new(mtm: MainThreadMarker, tab: Option<TabId>, holes: Holes) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ContainerIvars {
            holes,
            tab,
            placed: Cell::new(Tl { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        this.setWantsLayer(true);
        if let Some(layer) = this.layer() {
            layer.setMasksToBounds(true);
        }
        this
    }

    /// The tab of the container `view` is in (or is), if any.
    pub fn tab_of(view: &NSView) -> Option<TabId> {
        let mut v: Option<Retained<NSView>> = Some(objc2::Message::retain(view));
        while let Some(cur) = v {
            if let Some(c) = cur.downcast_ref::<Container>() {
                return c.ivars().tab;
            }
            v = unsafe { cur.superview() };
        }
        None
    }

    /// Rounds the container's corners (a popup sheet).
    pub fn set_corner_radius(&self, radius: f64) {
        if let Some(layer) = self.layer() {
            layer.setCornerRadius(radius);
        }
    }
}

/// Masks `container` so the holes are not drawn; no mask when there are
/// none. Holes never overlap (`sanitize_holes` merges them), as the
/// even-odd fill requires.
///
/// With `glide` (seconds since the chrome started its 0.25 s CSS slide),
/// the mask's path animates from the old holes to the new ones, begun that
/// long ago so it lines up with the chrome's slide. Callers glide only
/// when the number of holes is unchanged, so both paths have the same shape.
pub fn apply_mask(container: &Container, holes: &[Hole], glide: Option<f64>) {
    let Some(layer) = container.layer() else { return };
    if holes.is_empty() {
        unsafe { layer.setMask(None) };
        return;
    }
    let f = container.ivars().placed.get();
    let path = CGMutablePath::new();
    unsafe {
        CGMutablePath::add_rect(Some(&path), std::ptr::null(), CGRect::new(CGPoint::ZERO, CGSize::new(f.w, f.h)));
    }
    for h in holes {
        // Hole, local and unflipped.
        let x0 = h.x - f.x;
        let x1 = x0 + h.w;
        let y1 = f.h - (h.y - f.y);
        let y0 = y1 - h.h;
        let r = |bit: u8| if h.corners & bit != 0 { h.r } else { 0.0 };
        add_rounded(&path, (x0, y0, x1, y1), r(1), r(2), r(4), r(8));
    }
    let new_path: &CGPath = &path;
    let existing = layer.mask().and_then(|m| m.downcast::<CAShapeLayer>().ok());
    let old_path = existing.as_ref().and_then(|m| m.path());
    let mask = existing.unwrap_or_else(CAShapeLayer::layer);
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    mask.setFrame(CGRect::new(CGPoint::ZERO, CGSize::new(f.w, f.h)));
    mask.setPath(Some(new_path));
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
            anim.setToValue(Some(as_obj(new_path)));
        }
        anim.setDuration(GLIDE_SECONDS);
        anim.setBeginTime(CACurrentMediaTime() - lag.clamp(0.0, GLIDE_SECONDS));
        anim.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(unsafe {
            kCAMediaTimingFunctionEaseInEaseOut
        })));
        mask.addAnimation_forKey(&anim, Some(&NSString::from_str("snorkleGlide")));
    }
}

/// Adds a rectangle (`x0 < x1`, `y0 < y1`, unflipped: `y1` is the top)
/// with its own radius at each corner: top left, top right, bottom left,
/// bottom right. A radius of 0 is a square corner.
fn add_rounded(path: &CGMutablePath, (x0, y0, x1, y1): (f64, f64, f64, f64), tl: f64, tr: f64, bl: f64, br: f64) {
    let p = Some(path);
    let m = std::ptr::null();
    unsafe {
        CGMutablePath::move_to_point(p, m, x0 + bl, y0);
        CGMutablePath::add_line_to_point(p, m, x1 - br, y0);
        CGMutablePath::add_arc_to_point(p, m, x1, y0, x1, y0 + br, br);
        CGMutablePath::add_line_to_point(p, m, x1, y1 - tr);
        CGMutablePath::add_arc_to_point(p, m, x1, y1, x1 - tr, y1, tr);
        CGMutablePath::add_line_to_point(p, m, x0 + tl, y1);
        CGMutablePath::add_arc_to_point(p, m, x0, y1, x0, y1 - tl, tl);
        CGMutablePath::add_line_to_point(p, m, x0, y0 + bl);
        CGMutablePath::add_arc_to_point(p, m, x0, y0, x0 + bl, y0, bl);
    }
    CGMutablePath::close_subpath(p);
}
