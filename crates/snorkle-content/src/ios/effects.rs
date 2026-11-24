//! The iPadOS content host: applying the core's effects, data stores, and
//! creating and mounting page views.

use super::*;

impl ContentHost {
    pub fn apply(&mut self, effect: &Effect) {
        unsafe {
            match effect {
                Effect::CreateView { tab, profile, then } => {
                    if self.views.contains_key(tab) {
                        return;
                    }
                    let view = self.create_view(*tab, *profile);
                    match then {
                        AfterCreate::Load(url) => load(&view.web, url),
                        AfterCreate::Restore { fallback } => match self.saved.remove(tab) {
                            Some(state) => view.web.setInteractionState(Some(&state)),
                            None => {
                                if let Some(url) = fallback {
                                    load(&view.web, url)
                                }
                            }
                        },
                    }
                    self.views.insert(*tab, view);
                }
                Effect::AdoptPopup { tab } => {
                    if let Some(parts) = PENDING_POPUPS.with(|p| p.borrow_mut().remove(tab)) {
                        let view = self.mount(*tab, parts);
                        self.views.insert(*tab, view);
                    }
                }
                Effect::DestroyView { tab, keep_state } => {
                    if let Some(v) = self.views.remove(tab) {
                        if *keep_state {
                            if let Some(state) = v.web.interactionState() {
                                self.saved.insert(*tab, state);
                            }
                        }
                        rules::unregister(*tab);
                        drop(v);
                        self.sync_backdrop();
                        self.refocus_soon();
                    }
                    if !*keep_state {
                        self.saved.remove(tab);
                    }
                }
                Effect::ShowView { tab, pane } => {
                    if let Some(v) = self.views.get(tab) {
                        v.pane.set(*pane);
                        self.place(&v.container, self.pane_frame(*pane), v.scale.get());
                        self.root.bringSubviewToFront(&v.container);
                        v.container.setHidden(false);
                        set_corner_radius(
                            &v.container,
                            match pane {
                                Pane::Sheet => 12.0,
                                Pane::Preview => crate::PREVIEW_RADIUS,
                                _ => 0.0,
                            },
                        );
                    }
                    self.sync_backdrop();
                }
                Effect::HideView(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.container.setHidden(true);
                    }
                    self.sync_backdrop();
                    self.refocus_soon();
                }
                Effect::Load { tab, url } => {
                    if let Some(v) = self.views.get(tab) {
                        load(&v.web, url);
                    }
                }
                Effect::SetSplitRatio(permille) => {
                    self.split_ratio.set(f64::from((*permille).clamp(200, 800)) / 1000.0);
                    for v in self.views.values().filter(|v| !v.container.isHidden()) {
                        self.place(&v.container, self.pane_frame(v.pane.get()), v.scale.get());
                    }
                }
                Effect::SetZoom { tab, percent } => {
                    if let Some(v) = self.views.get(tab) {
                        // The core only sends levels from 50 to 150 %.
                        v.scale.set(f64::from((*percent).clamp(50, 150)) / 100.0);
                        self.place(&v.container, self.pane_frame(v.pane.get()), v.scale.get());
                    }
                }
                Effect::GoBack(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.web.goBack();
                    }
                }
                Effect::GoForward(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.web.goForward();
                    }
                }
                Effect::Reload(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.web.stopLoading();
                        v.web.reloadFromOrigin();
                    }
                }
                Effect::Stop(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.web.stopLoading();
                    }
                }
                Effect::ReleasePrivateData => self.release_private_store(),
                Effect::OpenExternal(url) => open_external(self.mtm, url),
                // A pin's tab keeps its page (C25): constant code, one boolean.
                Effect::PinGuard { tab, on } => {
                    if let Some(v) = self.views.get(tab) {
                        crate::webkit::injection::run_now(self.mtm, &v.web, &snorkle_core::pinguard::set_js(*on));
                    }
                }
                // Handled by the app layer, not the content host.
                Effect::RecordHistory { .. }
                | Effect::PersistSession(_)
                | Effect::NotifyChrome
                | Effect::NotifyProgress(_)
                | Effect::OpenSavedPage { .. } => {}
            }
        }
    }

    pub(super) unsafe fn store_for(&mut self, profile: Profile) -> Retained<WKWebsiteDataStore> {
        match profile {
            Profile::Default => self.default_store.clone(),
            Profile::Private => {
                self.private_store.get_or_insert_with(|| WKWebsiteDataStore::nonPersistentDataStore(self.mtm)).clone()
            }
        }
    }

    pub(super) unsafe fn release_private_store(&mut self) {
        // Saved interaction states of private tabs were dropped with their
        // tabs (DestroyView keep_state=false). Wipe the store explicitly
        // before releasing it, in case WebKit keeps it alive internally.
        if let Some(store) = self.private_store.take() {
            stores::wipe(self.mtm, &store);
        }
    }

    pub(super) unsafe fn create_view(&mut self, tab: TabId, profile: Profile) -> View {
        let mtm = self.mtm;
        let config = stores::content_configuration(mtm, &self.store_for(profile));
        let frame = self.content_frame();
        let parts = build_web_view(mtm, tab, &config, CGRect::new(CGPoint::new(0.0, 0.0), frame.size), &self.events);
        self.mount(tab, parts)
    }

    /// Puts a web view into a clipped container in the content area and
    /// wires injection, content blocking and tap-to-focus. Used for views we
    /// create and for popups WebKit asked us to create.
    pub(super) unsafe fn mount(&mut self, tab: TabId, parts: WebViewParts) -> View {
        let mtm = self.mtm;
        let WebViewParts { web, nav, ui, kvo } = parts;
        let controller = web.configuration().userContentController();
        let frame = self.content_frame();
        let container = keys::KeyContainer::new_view(mtm, frame, self.events.clone(), self.holes.clone());
        container.setClipsToBounds(true);
        container.setAutoresizingMask(UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight);
        container.setHidden(true);
        web.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), frame.size));

        container.addSubview(&web);
        apply_mask(&container, &crate::holes_over(&self.holes.borrow(), rect_tuple(frame)), None);
        let view = self.injections.view_for(tab);
        set_dark(&web, view.dark, view.night);
        self.root.addSubview(&container);
        injection::install(mtm, &controller, self.scripts_on(tab, &controller));
        rules::register(tab, controller.clone());
        WEB_VIEW_TABS.with(|m| m.borrow_mut().insert(Retained::as_ptr(&web) as usize, tab));

        let tap = TapFocus::new(mtm, TabIvars::new(tab, self.events.clone()));
        let recognizer = UITapGestureRecognizer::initWithTarget_action(
            UITapGestureRecognizer::alloc(mtm),
            Some(&tap),
            Some(sel!(tapped:)),
        );
        recognizer.setCancelsTouchesInView(false);
        recognizer.setDelegate(Some(ProtocolObject::from_ref(&*tap)));
        container.addGestureRecognizer(&recognizer);

        // Pull down to refresh.
        let refresh = RefreshTarget::new(mtm, TabIvars::new(tab, self.events.clone()));
        let control = UIRefreshControl::new(mtm);
        control.addTarget_action_forControlEvents(Some(&refresh), sel!(refresh:), UIControlEvents::ValueChanged);
        if crate::pull_to_refresh() {
            web.scrollView().setRefreshControl(Some(&control));
        }

        // Debug builds: note each scroll gesture in the console (OPEN_QUESTIONS 109).
        let probe = if cfg!(debug_assertions) {
            let p = ScrollProbe::new(mtm, TabIvars::new(tab, self.events.clone()));
            let pan: Retained<objc2_ui_kit::UIGestureRecognizer> = msg_send![&*web.scrollView(), panGestureRecognizer];
            pan.addTarget_action(&p, sel!(panned:));
            Some((p, pan))
        } else {
            None
        };

        View {
            container,
            web,
            controller,
            pane: Cell::new(Pane::Full),
            scale: Cell::new(1.0),
            _tap: tap,
            _tap_recognizer: recognizer,
            _refresh: refresh,
            refresh_control: control,
            probe,
            _nav: nav,
            _ui: ui,
            kvo,
        }
    }

    /// Installs content-blocking rule lists for `key` into every content
    /// web view (now and future), reusing WebKit's compiled cache when it
    /// has a complete set for `key`; otherwise `make` runs on a background
    /// thread and WebKit compiles its output. `done` runs on the main thread.
    pub fn install_rules(&mut self, key: String, make: crate::MakeRuleLists, done: crate::RulesDone) {
        rules::install(self.mtm, key, make, done);
    }
}

thread_local! {
    /// A keyboard-focus check is queued (see `refocus_soon`).
    static REFOCUS_QUEUED: Cell<bool> = const { Cell::new(false) };
}

impl ContentHost {
    /// After a page view is destroyed or hidden. If it had keyboard focus,
    /// nothing has it now, and iPadOS then delivers no key commands (⌘W,
    /// ⌘T, …) until the next tap: closing a tab with ⌘W worked once and the
    /// next ⌘W did nothing (owner report 2026-11-24). Once the effects being
    /// applied are done (queued on the main queue), if nothing has focus,
    /// the page in front takes it, else the chrome.
    fn refocus_soon(&self) {
        if REFOCUS_QUEUED.with(|q| q.replace(true)) {
            return;
        }
        let root = self.root.clone();
        let chrome = self.chrome.borrow().clone();
        let block = RcBlock::new(move || {
            REFOCUS_QUEUED.with(|q| q.set(false));
            let Some(mtm) = MainThreadMarker::new() else { return };
            if menu::responder_present(mtm) {
                return;
            }
            // Containers are brought to the front when shown: the last
            // visible one is the page in front.
            use objc2::ClassType;
            let containers: Vec<Retained<UIView>> = root.subviews().iter().collect();
            let page = containers
                .iter()
                .rev()
                .filter(|c| !c.isHidden() && c.downcast_ref::<keys::KeyContainer>().is_some())
                .find_map(|c| c.subviews().iter().find(|s| s.isKindOfClass(WKWebView::class())));
            let took = page.is_some_and(|w| w.becomeFirstResponder());
            if !took {
                if let Some(c) = chrome.as_deref() {
                    c.becomeFirstResponder();
                }
            }
        });
        unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
    }
}
