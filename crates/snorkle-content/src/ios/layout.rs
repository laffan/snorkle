//! The iPadOS content host: creating it, and where page views go (content
//! area, split panes, the sheet backdrop, insets and holes).

use super::*;

impl ContentHost {
    /// # Safety
    /// `root_view` must be a valid `UIView*` (the Tauri window's root view,
    /// from `raw_window_handle::UiKitWindowHandle::ui_view`) and this must be
    /// called on the main thread.
    pub unsafe fn new(root_view: *mut c_void, events: Rc<dyn ContentEvents>) -> Self {
        let mtm = MainThreadMarker::new().expect("ContentHost must be created on the main thread");
        let root = Retained::retain(root_view.cast::<UIView>()).expect("root view must not be null");
        let id = NSUUID::from_bytes(DEFAULT_PROFILE_STORE_ID);
        let default_store = WKWebsiteDataStore::dataStoreForIdentifier(&id, mtm);

        let center = NSNotificationCenter::defaultCenter();
        let queue = NSOperationQueue::mainQueue();
        let mut observers = Vec::new();
        let ev = events.clone();
        let mem = RcBlock::new(move |_n: NonNull<NSNotification>| ev.memory_warning());
        observers.push(center.addObserverForName_object_queue_usingBlock(
            Some(UIApplicationDidReceiveMemoryWarningNotification),
            None,
            Some(&queue),
            &mem,
        ));
        let ev = events.clone();
        let bg = RcBlock::new(move |_n: NonNull<NSNotification>| ev.did_enter_background());
        observers.push(center.addObserverForName_object_queue_usingBlock(
            Some(UIApplicationDidEnterBackgroundNotification),
            None,
            Some(&queue),
            &bg,
        ));

        // Back from another app, iPadOS may leave nothing first responder
        // until the next tap, and then key commands (the shortcuts, e.g.
        // ⌘T) are not delivered: give the chrome first-responder status in
        // that case (a page or the chrome that still has it keeps it).
        let chrome: Rc<RefCell<Option<Retained<UIView>>>> = Rc::default();
        let (ev, ch, r) = (events.clone(), chrome.clone(), root.clone());
        let active = RcBlock::new(move |_n: NonNull<NSNotification>| {
            let Some(mtm) = MainThreadMarker::new() else { return };
            let note = menu::ensure_responder(mtm, ch.borrow().as_deref());
            ev.note(note);
            hide_status_bar(&r);
        });
        observers.push(center.addObserverForName_object_queue_usingBlock(
            Some(UIApplicationDidBecomeActiveNotification),
            None,
            Some(&queue),
            &active,
        ));

        // Pointer near the left edge, for the sidebar's edge tab.
        let hover = HoverEdge::new(mtm, HoverIvars::new(events.clone()));
        let recognizer = UIHoverGestureRecognizer::initWithTarget_action(
            UIHoverGestureRecognizer::alloc(mtm),
            Some(&hover),
            Some(sel!(hovered:)),
        );
        recognizer.setDelegate(Some(ProtocolObject::from_ref(&*hover)));
        root.addGestureRecognizer(&recognizer);

        menu::install(mtm, events.clone());
        hide_status_bar(&root);
        let holes = keys::Holes::default();
        // A container too, so it also leaves the holes (and their touches) out.
        let backdrop = keys::KeyContainer::new_view(mtm, CGRect::ZERO, events.clone(), holes.clone());
        backdrop.setBackgroundColor(Some(&UIColor::colorWithWhite_alpha(0.0, 0.45)));
        backdrop.setHidden(true);
        root.addSubview(&backdrop);

        ContentHost {
            mtm,
            backdrop,
            _hover: hover,
            holes,
            chrome,
            root,
            top: Cell::new(0.0),
            left: Cell::new(0.0),
            right: Cell::new(0.0),
            bottom: Cell::new(0.0),
            split_ratio: Cell::new(0.5),
            preview: Cell::new(None),
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
        }
    }

    pub fn live_views(&self) -> usize {
        self.views.len()
    }

    pub(super) fn content_frame(&self) -> CGRect {
        let b = self.root.bounds();
        let top = clamp_inset(self.top.get(), b.size.height);
        let left = clamp_inset(self.left.get(), b.size.width);
        let right = clamp_inset(self.right.get(), b.size.width);
        let bottom = clamp_inset(self.bottom.get(), b.size.height);
        CGRect::new(
            CGPoint::new(left, top),
            CGSize::new((b.size.width - left - right).max(0.0), (b.size.height - top - bottom).max(0.0)),
        )
    }

    /// Frame of one pane: the whole content area, or part of it in split
    /// view (side by side, or one above the other): the first pane gets
    /// `split_ratio` of the space less the 8 pt gap (floored), the second
    /// the rest. The chrome draws the divider's handle in the gap, which no
    /// page covers (`ui/chrome/sidebar.js` `splitSizes` uses the same formula).
    pub(super) fn pane_frame(&self, pane: Pane) -> CGRect {
        let area = self.content_frame();
        let r = self.split_ratio.get();
        let sizes = |total: f64| {
            let first = ((total - SPLIT_GAP) * r).floor().max(0.0);
            (first, (total - SPLIT_GAP - first).max(0.0))
        };
        let (left_w, right_w) = sizes(area.size.width);
        let (top_h, bottom_h) = sizes(area.size.height);
        match pane {
            Pane::Full => area,
            Pane::Left => CGRect::new(area.origin, CGSize::new(left_w, area.size.height)),
            Pane::Right => CGRect::new(
                CGPoint::new(area.origin.x + area.size.width - right_w, area.origin.y),
                CGSize::new(right_w, area.size.height),
            ),
            Pane::Top => CGRect::new(area.origin, CGSize::new(area.size.width, top_h)),
            Pane::Bottom => CGRect::new(
                CGPoint::new(area.origin.x, area.origin.y + area.size.height - bottom_h),
                CGSize::new(area.size.width, bottom_h),
            ),
            // Window-sized, centred, with a margin that leaves the page
            // underneath visible.
            Pane::Sheet => {
                const MARGIN: f64 = 16.0;
                let w = (area.size.width - 2.0 * MARGIN).clamp(0.0, 620.0);
                let h = (area.size.height - 2.0 * MARGIN).clamp(0.0, 820.0);
                CGRect::new(
                    CGPoint::new(
                        (area.origin.x + (area.size.width - w) / 2.0).floor(),
                        (area.origin.y + (area.size.height - h) / 2.0).floor(),
                    ),
                    CGSize::new(w, h),
                )
            }
            // Where the chrome's dialog says; the content area until it has.
            Pane::Preview => {
                self.preview.get().map_or(area, |(x, y, w, h)| CGRect::new(CGPoint::new(x, y), CGSize::new(w, h)))
            }
        }
    }

    /// The chrome's rectangle for the History preview (window points from
    /// the top left; `crate::preview_rect` keeps it in the window). A
    /// preview on screen moves there.
    pub fn set_preview_frame(&mut self, frame: Option<(f64, f64, f64, f64)>) {
        let b = self.root.bounds().size;
        let frame = frame.and_then(|r| crate::preview_rect(r, b.width, b.height));
        if frame == self.preview.get() {
            return;
        }
        self.preview.set(frame);
        for v in self.views.values().filter(|v| v.pane.get() == Pane::Preview) {
            self.place(&v.container, self.pane_frame(Pane::Preview), v.scale.get());
        }
    }

    /// Shows the backdrop under the visible sheet, if any, and keeps the
    /// sheet above it.
    pub(super) fn sync_backdrop(&self) {
        let sheet = self.views.values().find(|v| v.pane.get() == Pane::Sheet && !v.container.isHidden());
        match sheet {
            Some(v) => {
                self.place(&self.backdrop, self.content_frame(), 1.0);
                self.root.bringSubviewToFront(&self.backdrop);
                self.root.bringSubviewToFront(&v.container);
                self.backdrop.setHidden(false);
            }
            None => self.backdrop.setHidden(true),
        }
    }

    /// Insets of the content area, in points, reported by the trusted
    /// chrome UI: `top` is the bottom edge of the toolbar, `left` the right
    /// edge of the docked tab sidebar (0 when collapsed), `right` the width
    /// of a docked right panel (site scripts, headers), `bottom` the height
    /// of the docked console (0 when closed). Content containers never
    /// extend over any of them. `holes` are where the chrome floats over
    /// the content area (see the module docs).
    /// With `animate`, the page frames move over 0.25 s (the sidebar's
    /// slide in the chrome takes as long).
    /// `animate`: seconds since the chrome began its slide (`None`: move
    /// at once).
    pub fn set_insets(&mut self, top: f64, left: f64, right: f64, bottom: f64, holes: Vec<Hole>, animate: Option<f64>) {
        let before = self.content_frame().size.width;
        self.top.set(top);
        self.left.set(left);
        self.right.set(right);
        self.bottom.set(bottom);
        let b = self.root.bounds();
        let holes = crate::sanitize_holes(&holes, b.size.width, b.size.height);
        // The cut-outs glide (with the frames) only between sets of the
        // same shape; e.g. the sidebar's pop-over and toggle tab, which the
        // chrome always reports, on- or off-screen. Each view is masked only
        // with the holes over it (`crate::holes_over`, 2026-11-23), so for a
        // slide every view first gets all the old holes (nothing visible
        // changes: the extra ones lie beside it), glides to all the new ones,
        // and is cut back to the holes over it when the slide ends.
        let glide = animate.filter(|_| holes.len() == self.holes.borrow().len());
        if glide.is_some() {
            let old = self.holes.borrow().clone();
            for v in self.views.values() {
                apply_mask(&v.container, &old, None);
            }
        }
        *self.holes.borrow_mut() = holes;
        let grew = self.content_frame().size.width > before + 0.5;
        let moves: Vec<(Retained<UIView>, CGRect, f64)> =
            self.views.values().map(|v| (v.container.clone(), self.pane_frame(v.pane.get()), v.scale.get())).collect();
        if let Some(lag) = animate {
            let holes = self.holes.borrow().clone();
            let after = (moves.clone(), holes.clone());
            let block = RcBlock::new(move || {
                for (view, frame, scale) in &moves {
                    place_view(view, *frame, &holes, glide, *scale);
                }
            });
            let done = RcBlock::new(move |_finished: Bool| {
                let (moves, holes) = &after;
                for (view, frame, _) in moves {
                    apply_mask(view, &crate::holes_over(holes, rect_tuple(*frame)), None);
                }
            });
            UIView::animateWithDuration_animations_completion(
                (GLIDE_SECONDS - lag).max(0.05),
                &block,
                Some(&done),
                self.mtm,
            );
        } else {
            for (view, frame, scale) in &moves {
                self.place(view, *frame, *scale);
            }
        }
        for v in self.views.values() {
            if grew {
                // WebKit re-lays out the page asynchronously; check once it has.
                unsafe {
                    let _: () = msg_send![&*v.container, performSelector: sel!(snorkleFitWidth), withObject: Option::<&AnyObject>::None, afterDelay: 0.35f64];
                }
            }
        }
        self.sync_backdrop();
    }

    /// Sets a container's frame (and its web view's, scaled by `scale`)
    /// and cuts the holes over it out of it.
    pub(super) fn place(&self, view: &UIView, frame: CGRect, scale: f64) {
        place_view(view, frame, &crate::holes_over(&self.holes.borrow(), rect_tuple(frame)), None, scale);
    }
}
