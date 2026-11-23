//! The macOS content host: applying the core's effects, data stores, and
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
                        Z.with(|z| z.borrow_mut().remove(&(Retained::as_ptr(&v.container) as usize)));
                        drop(v);
                        self.sync_backdrop();
                    }
                    if !*keep_state {
                        self.saved.remove(tab);
                    }
                }
                Effect::ShowView { tab, pane } => {
                    if let Some(v) = self.views.get(tab) {
                        v.pane.set(*pane);
                        self.place(&v.container, self.pane_frame(*pane));
                        self.bring_to_front(&v.container);
                        v.container.setHidden(false);
                        v.container.set_corner_radius(if *pane == Pane::Sheet { 12.0 } else { 0.0 });
                    }
                    self.sync_backdrop();
                }
                Effect::HideView(tab) => {
                    if let Some(v) = self.views.get(tab) {
                        v.container.setHidden(true);
                    }
                    self.sync_backdrop();
                }
                Effect::Load { tab, url } => {
                    if let Some(v) = self.views.get(tab) {
                        load(&v.web, url);
                    }
                }
                Effect::SetSplitRatio(permille) => {
                    self.split_ratio.set(f64::from((*permille).clamp(200, 800)) / 1000.0);
                    for v in self.views.values().filter(|v| !v.container.isHidden()) {
                        self.place(&v.container, self.pane_frame(v.pane.get()));
                    }
                }
                Effect::SetZoom { tab, percent } => {
                    if let Some(v) = self.views.get(tab) {
                        // The core only sends levels from 50 to 150 %.
                        v.web.setPageZoom(f64::from((*percent).clamp(50, 150)) / 100.0);
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
                Effect::ReleasePrivateData => {
                    if let Some(store) = self.private_store.take() {
                        stores::wipe(self.mtm, &store);
                    }
                }
                Effect::OpenExternal(url) => open_external(url),
                // Handled by the app layer, not the content host.
                Effect::RecordHistory { .. }
                | Effect::PersistSession(_)
                | Effect::NotifyChrome
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

    pub(super) unsafe fn create_view(&mut self, tab: TabId, profile: Profile) -> View {
        let config = stores::content_configuration(self.mtm, &self.store_for(profile));
        let size = self.content_frame();
        let parts = build_web_view(self.mtm, tab, &config, NSSize::new(size.w, size.h), &self.events);
        self.mount(tab, parts)
    }

    /// Puts a web view into a clipped container in the content area and
    /// wires injection and content blocking. Used for views we create and
    /// for popups WebKit asked us to create.
    pub(super) unsafe fn mount(&mut self, tab: TabId, parts: WebViewParts) -> View {
        let mtm = self.mtm;
        let WebViewParts { web, nav, ui, kvo } = parts;
        let controller = web.configuration().userContentController();
        let container = Container::new(mtm, Some(tab), self.holes.clone());
        container.setHidden(true);
        self.root.addSubview(&container);
        self.place(&container, self.content_frame());
        web.setFrame(container.bounds());
        web.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        container.addSubview(&web);
        let view = self.injections.view_for(tab);
        set_dark(&web, view.dark, view.night);
        injection::install(mtm, &controller, self.scripts_on(tab, &controller));
        rules::register(tab, controller.clone());
        WEB_VIEW_TABS.with(|m| m.borrow_mut().insert(Retained::as_ptr(&web) as usize, tab));
        View { container, web, controller, pane: Cell::new(Pane::Full), _nav: nav, _ui: ui, kvo }
    }

    pub fn install_rules(&mut self, key: String, make: crate::MakeRuleLists, done: crate::RulesDone) {
        rules::install(self.mtm, key, make, done);
    }
}
