//! The macOS content host: creating it, and where page views go (content
//! area, split panes, the sheet backdrop, insets and holes).

use super::*;

impl ContentHost {
    /// # Safety
    /// `window` must be a valid `NSWindow*` (the Tauri main window, from
    /// `WebviewWindow::ns_window`) whose content view holds the chrome's
    /// web view, and this must be called on the main thread.
    pub unsafe fn new(window: *mut c_void, events: Rc<dyn ContentEvents>) -> Self {
        let mtm = MainThreadMarker::new().expect("ContentHost must be created on the main thread");
        let window = Retained::retain(window.cast::<NSWindow>()).expect("window must not be null");
        let content = window.contentView().expect("the window has a content view");
        let root = ContentRoot::new(mtm, content.bounds());
        root.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&root);
        // Pointer leaving the window (the edge tab hides).
        let area = NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::ZERO,
            NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveInKeyWindow
                | NSTrackingAreaOptions::InVisibleRect,
            Some(&root),
            None,
        );
        root.addTrackingArea(&area);

        let id = NSUUID::from_bytes(DEFAULT_PROFILE_STORE_ID);
        let default_store = WKWebsiteDataStore::dataStoreForIdentifier(&id, mtm);

        // Leaving the app (another app is active): what iPadOS does on
        // entering the background (discard idle views, write the session).
        let center = NSNotificationCenter::defaultCenter();
        let queue = NSOperationQueue::mainQueue();
        let ev = events.clone();
        let bg = RcBlock::new(move |_n: NonNull<NSNotification>| ev.did_enter_background());
        let observers = vec![center.addObserverForName_object_queue_usingBlock(
            Some(NSApplicationDidResignActiveNotification),
            None,
            Some(&queue),
            &bg,
        )];

        let monitors = keys::install(mtm, &window, &root, events.clone());
        let pressure = pressure::watch(events.clone());
        let holes = Holes::default();
        let backdrop = Container::new(mtm, None, holes.clone());
        if let Some(layer) = backdrop.layer() {
            let black = NSColor::colorWithWhite_alpha(0.0, 0.45);
            layer.setBackgroundColor(Some(&black.CGColor()));
        }
        backdrop.setHidden(true);
        root.addSubview(&backdrop);

        ContentHost {
            mtm,
            window,
            root,
            top: Cell::new(0.0),
            left: Cell::new(0.0),
            right: Cell::new(0.0),
            bottom: Cell::new(0.0),
            split_ratio: Cell::new(0.5),
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
            backdrop,
            holes,
            chrome: None,
            before_hover: None,
            sharing: None,
            z: Cell::new(0),
            _monitors: monitors,
            _pressure: pressure,
        }
    }

    pub fn live_views(&self) -> usize {
        self.views.len()
    }

    pub(super) fn height(&self) -> f64 {
        self.root.bounds().size.height
    }

    pub(super) fn content_frame(&self) -> Tl {
        let b = self.root.bounds().size;
        let top = clamp_inset(self.top.get(), b.height);
        let left = clamp_inset(self.left.get(), b.width);
        let right = clamp_inset(self.right.get(), b.width);
        let bottom = clamp_inset(self.bottom.get(), b.height);
        Tl { x: left, y: top, w: (b.width - left - right).max(0.0), h: (b.height - top - bottom).max(0.0) }
    }

    /// Frame of one pane, from the top left; the same layout as on iPadOS
    /// (`ios/mod.rs` `pane_frame`, `ui/chrome/sidebar.js` `splitSizes`).
    pub(super) fn pane_frame(&self, pane: Pane) -> Tl {
        let a = self.content_frame();
        let r = self.split_ratio.get();
        let sizes = |total: f64| {
            let first = ((total - SPLIT_GAP) * r).floor().max(0.0);
            (first, (total - SPLIT_GAP - first).max(0.0))
        };
        let (left_w, right_w) = sizes(a.w);
        let (top_h, bottom_h) = sizes(a.h);
        match pane {
            Pane::Full => a,
            Pane::Left => Tl { w: left_w, ..a },
            Pane::Right => Tl { x: a.x + a.w - right_w, w: right_w, ..a },
            Pane::Top => Tl { h: top_h, ..a },
            Pane::Bottom => Tl { y: a.y + a.h - bottom_h, h: bottom_h, ..a },
            Pane::Sheet => {
                const MARGIN: f64 = 16.0;
                let w = (a.w - 2.0 * MARGIN).clamp(0.0, 620.0);
                let h = (a.h - 2.0 * MARGIN).clamp(0.0, 820.0);
                Tl { x: (a.x + (a.w - w) / 2.0).floor(), y: (a.y + (a.h - h) / 2.0).floor(), w, h }
            }
        }
    }

    /// Moves `view` in front of the other containers without taking it out
    /// of the window (removing a web view from its window would pause it).
    pub(super) fn bring_to_front(&self, view: &NSView) {
        let n = self.z.get() + 1;
        self.z.set(n);
        Z.with(|z| z.borrow_mut().insert(view as *const NSView as usize, n));
        unsafe { self.root.sortSubviewsUsingFunction_context(by_z, std::ptr::null_mut()) };
    }

    /// Shows the backdrop under the visible sheet, if any, and keeps the
    /// sheet above it.
    pub(super) fn sync_backdrop(&self) {
        let sheet = self.views.values().find(|v| v.pane.get() == Pane::Sheet && !v.container.isHidden());
        match sheet {
            Some(v) => {
                self.place(&self.backdrop, self.content_frame());
                self.bring_to_front(&self.backdrop);
                self.bring_to_front(&v.container);
                self.backdrop.setHidden(false);
            }
            None => self.backdrop.setHidden(true),
        }
    }

    /// Insets of the content area reported by the chrome (see the iPadOS
    /// host for their meaning), the holes, and whether to animate (seconds
    /// since the chrome began its slide).
    ///
    /// Each page view is masked only with the holes over it
    /// (`crate::holes_over`). A slide animates the mask's path, which needs
    /// the same set of holes before and after, so for a slide every view
    /// first gets all the old holes (no visible change: the extra ones lie
    /// beside it), glides to all the new ones, and is cut back to the holes
    /// over it once the slide has ended.
    pub fn set_insets(&mut self, top: f64, left: f64, right: f64, bottom: f64, holes: Vec<Hole>, animate: Option<f64>) {
        self.top.set(top);
        self.left.set(left);
        self.right.set(right);
        self.bottom.set(bottom);
        let b = self.root.bounds().size;
        let holes = crate::sanitize_holes(&holes, b.width, b.height);
        let glide = animate.filter(|_| holes.len() == self.holes.borrow().len());
        if glide.is_some() {
            let old = self.holes.borrow().clone();
            for v in self.views.values() {
                views::apply_mask(&v.container, &old, None);
            }
        }
        *self.holes.borrow_mut() = holes;
        let moves: Vec<(Retained<Container>, Tl)> =
            self.views.values().map(|v| (v.container.clone(), self.pane_frame(v.pane.get()))).collect();
        let height = self.height();
        match animate {
            Some(lag) => {
                let holes = self.holes.borrow().clone();
                let after = (moves.clone(), holes.clone());
                let duration = (GLIDE_SECONDS - lag).max(0.05);
                let changes = RcBlock::new(move |ctx: NonNull<NSAnimationContext>| {
                    let ctx = unsafe { ctx.as_ref() };
                    ctx.setDuration(duration);
                    ctx.setTimingFunction(Some(&CAMediaTimingFunction::functionWithName(unsafe {
                        objc2_quartz_core::kCAMediaTimingFunctionEaseInEaseOut
                    })));
                    for (view, frame) in &moves {
                        view.ivars().placed.set(*frame);
                        let animator: Retained<AnyObject> = unsafe { msg_send![&**view, animator] };
                        let _: () = unsafe { msg_send![&*animator, setFrame: frame.ns(height)] };
                        views::apply_mask(view, &holes, glide);
                    }
                });
                // Once the slide has ended: only the holes over each view.
                let done = RcBlock::new(move || {
                    let (moves, holes) = &after;
                    for (view, frame) in moves {
                        views::apply_mask(view, &crate::holes_over(holes, (frame.x, frame.y, frame.w, frame.h)), None);
                    }
                });
                NSAnimationContext::runAnimationGroup_completionHandler(&changes, Some(&done));
            }
            None => {
                for (view, frame) in &moves {
                    self.place(view, *frame);
                }
            }
        }
        self.sync_backdrop();
    }

    /// Sets a container's frame and cuts the holes over it out of it.
    pub(super) fn place(&self, view: &Container, frame: Tl) {
        view.ivars().placed.set(frame);
        view.setFrame(frame.ns(self.height()));
        views::apply_mask(view, &crate::holes_over(&self.holes.borrow(), (frame.x, frame.y, frame.w, frame.h)), None);
    }
}
