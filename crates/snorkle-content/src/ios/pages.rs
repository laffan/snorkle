//! The iPadOS content host: operations on one page (injections, snapshots,
//! clips, the notebook, page text, offline copies and archives, the image
//! menu, files, downloads, settings, sharing, Define, find, the summary,
//! scripts, favicons, website data).

use super::*;

impl ContentHost {
    /// Replaces everything injected (user scripts, view modes with per-tab
    /// overrides) in every content view; it applies from the next load.
    /// With `run_now`, each loaded page is also brought in line with its
    /// view modes immediately.
    pub fn set_injections(&mut self, injections: crate::Injections, run_now: bool) {
        self.injections = injections;
        self.compile_scripts();
        for (tab, v) in &self.views {
            injection::install(self.mtm, &v.controller, self.scripts_on(*tab, &v.controller));
            let view = self.injections.view_for(*tab);
            set_dark(&v.web, view.dark, view.night);
            if run_now {
                injection::run_now(self.mtm, &v.web, &self.injections.view_for(*tab).apply_js);
            }
        }
    }

    /// Turns page console capture on or off for every content view. Takes
    /// effect from the next page load.
    pub fn set_console_capture(&mut self, on: bool) {
        self.console = on;
        self.compile_scripts();
        for (tab, v) in &self.views {
            injection::install(self.mtm, &v.controller, self.scripts_on(*tab, &v.controller));
        }
    }

    /// Save for Later's thumbnail (2026-11-09): shows the tab's loaded
    /// page in light mode, in colour and with its images (`on`), or as its
    /// view modes say again. Only that page changes: nothing installed for
    /// later loads, and no other tab (a popup shares its opener's content
    /// controller), is touched.
    pub fn force_plain(&mut self, tab: TabId, on: bool) {
        let Some(v) = self.views.get(&tab) else { return };
        if on {
            injection::run_now(self.mtm, &v.web, &snorkle_core::viewmodes::plain_js());
            // Night vision stays on (`plain_js` leaves it): the page is on
            // screen meanwhile.
            set_light(&v.web, self.injections.view_for(tab).night);
        } else {
            let view = self.injections.view_for(tab);
            injection::run_now(self.mtm, &v.web, &view.apply_js);
            set_dark(&v.web, view.dark, view.night);
        }
    }

    /// Reads `tab`'s page for the AI chat (title, visible text, selection;
    /// `snorkle_core::ai::PAGE_TEXT_JS`). `done` runs on the main thread.
    /// Save for Later: a JPEG thumbnail of what the tab's view shows now
    /// (its visible bounds, `THUMB_WIDTH` points wide at the screen's
    /// scale, as the view is drawn now: `saved::save` turns view modes off and
    /// draws it light first), or `None` without a live view or if
    /// WebKit cannot draw it (e.g. a view that is not on screen).
    pub fn thumbnail(&mut self, tab: TabId, done: crate::ThumbDone) {
        let Some(v) = self.views.get(&tab) else {
            done(None);
            return;
        };
        let done = RefCell::new(Some(done));
        unsafe {
            let cfg = objc2_web_kit::WKSnapshotConfiguration::new(self.mtm);
            cfg.setSnapshotWidth(Some(&objc2_foundation::NSNumber::new_f64(crate::THUMB_WIDTH)));
            let block = RcBlock::new(move |image: *mut UIImage, _err: *mut NSError| {
                let jpeg = image.as_ref().and_then(|i| i.jpeg_representation(crate::THUMB_QUALITY)).map(|d| d.to_vec());
                if let Some(f) = done.borrow_mut().take() {
                    f(jpeg);
                }
            });
            v.web.takeSnapshotWithConfiguration_completionHandler(Some(&cfg), &block);
        }
    }

    /// Notebook (2026-11-15): a JPEG of a region of `tab`'s page as its
    /// view draws it now (`rect` in the page's visual-viewport CSS pixels,
    /// `vw` that viewport's width, from a C23 report; `crate::clip_rect`),
    /// drawn by the app from its own web view, at most `CLIP_MAX_WIDTH`
    /// points wide. `None` without a live view or if WebKit cannot draw it.
    pub fn clip_region(&mut self, tab: TabId, rect: (f64, f64, f64, f64), vw: f64, done: crate::ThumbDone) {
        let Some(v) = self.views.get(&tab) else {
            done(None);
            return;
        };
        let b = v.web.bounds();
        let Some((x, y, w, h)) = crate::clip_rect(rect, vw, (b.size.width, b.size.height)) else {
            done(None);
            return;
        };
        let done = RefCell::new(Some(done));
        unsafe {
            let cfg = objc2_web_kit::WKSnapshotConfiguration::new(self.mtm);
            cfg.setRect(CGRect::new(CGPoint::new(x, y), CGSize::new(w, h)));
            cfg.setSnapshotWidth(Some(&objc2_foundation::NSNumber::new_f64(w.min(crate::CLIP_MAX_WIDTH))));
            let block = RcBlock::new(move |image: *mut UIImage, _err: *mut NSError| {
                let jpeg = image
                    .as_ref()
                    .and_then(|i| i.jpeg_representation(crate::CLIP_QUALITY))
                    .map(|d| d.to_vec())
                    .filter(|d| d.len() <= crate::CLIP_MAX_BYTES);
                if let Some(f) = done.borrow_mut().take() {
                    f(jpeg);
                }
            });
            v.web.takeSnapshotWithConfiguration_completionHandler(Some(&cfg), &block);
        }
    }

    /// Runs constant notebook code (`snorkle_core::notebook`: clip mode,
    /// the quote button's marker) in `tab`'s page, client world, main frame.
    pub fn notebook_run(&mut self, tab: TabId, source: &str) {
        if let Some(v) = self.views.get(&tab) {
            injection::run_now(self.mtm, &v.web, source);
        }
    }

    /// Reads the sentence under the notebook's quote button when it is let
    /// go (`snorkle_core::notebook::drop_js`, C23).
    pub fn notebook_read(&mut self, tab: TabId, source: &str, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, source, done),
            None => done(None),
        }
    }

    /// A page shown without its view modes (`on`), or with them again, for
    /// a notebook clip: the styles only, its appearance left as it is (so
    /// a dark page is not hidden while its detector waits for it).
    pub fn plain_page(&mut self, tab: TabId, on: bool) {
        let Some(v) = self.views.get(&tab) else { return };
        if on {
            injection::run_now(self.mtm, &v.web, &snorkle_core::viewmodes::plain_js());
        } else {
            let view = self.injections.view_for(tab);
            injection::run_now(self.mtm, &v.web, &view.apply_js);
        }
    }

    /// ⋯ → Open in App: offers the page's address (http/https, from the
    /// core) to the app that claims it, with UIKit's "universal links only"
    /// option, so nothing opens if no installed app claims it (no browser,
    /// no other scheme). `done` gets whether an app opened.
    pub fn open_in_app(&mut self, url: &Url, done: crate::OpenedDone) {
        if !matches!(url.scheme(), "http" | "https") {
            done(false);
            return;
        }
        let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url.as_str())) else {
            done(false);
            return;
        };
        let done = RefCell::new(Some(done));
        let block = RcBlock::new(move |ok: objc2::runtime::Bool| {
            if let Some(f) = done.borrow_mut().take() {
                f(ok.as_bool());
            }
        });
        unsafe {
            let key: &objc2_foundation::NSString = objc2_ui_kit::UIApplicationOpenURLOptionUniversalLinksOnly;
            let yes = objc2_foundation::NSNumber::new_bool(true);
            let options: Retained<NSDictionary<objc2_foundation::NSString, AnyObject>> =
                NSDictionary::from_retained_objects(&[key], &[Retained::into_super(Retained::into_super(Retained::into_super(yes)))]);
            UIApplication::sharedApplication(self.mtm).openURL_options_completionHandler(&nsurl, &options, Some(&block));
        }
    }

    pub fn read_page_text(&mut self, tab: TabId, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, snorkle_core::ai::PAGE_TEXT_JS, done),
            None => done(None),
        }
    }

    /// Remembers the chrome's web view (a `WKWebView*` from Tauri).
    ///
    /// # Safety
    /// `view` must be a valid `UIView*` that outlives the host, or null.
    pub unsafe fn set_chrome_view(&mut self, view: *mut c_void) {
        *self.chrome.borrow_mut() = unsafe { Retained::retain(view.cast::<UIView>()) };
    }

    /// Gives the chrome's web view keyboard focus, so an input the chrome
    /// focuses after a shortcut (new-tab palette, find) receives typing
    /// even when a page, or nothing, was first responder.
    pub fn focus_chrome(&mut self) {
        if let Some(v) = self.chrome.borrow().as_ref() {
            v.becomeFirstResponder();
        }
    }

    /// Keyboard focus following the pointer over the notebook is a macOS
    /// feature (UIKit gives no way to find the first responder to give
    /// focus back to): nothing changes here.
    pub fn hover_focus(&mut self, _on: bool) -> bool {
        false
    }

    /// Saves the tab's page as a web archive (an offline copy of a pinned
    /// or bookmarked page); `done` gets its bytes, or `None` (failed, or
    /// larger than `storage::MAX_OFFLINE_COPY_BYTES`).
    pub fn save_offline(&mut self, tab: TabId, done: crate::DataDone) {
        self.web_archive(tab, snorkle_core::storage::MAX_OFFLINE_COPY_BYTES, done);
    }

    /// WebKit's web archive of the tab's page as it is now
    /// (`createWebArchiveData`), at most `max` bytes; `None` otherwise.
    /// Save for Later reads the resources WebKit holds for the page from it.
    pub fn web_archive(&mut self, tab: TabId, max: usize, done: crate::DataDone) {
        let Some(v) = self.views.get(&tab) else { return done(None) };
        let done = Cell::new(Some(done));
        let block = RcBlock::new(move |data: *mut NSData, _error: *mut NSError| {
            let Some(done) = done.take() else { return };
            let data = unsafe { data.as_ref() };
            done(data.filter(|d| d.length() <= max).map(|d| d.to_vec()));
        });
        unsafe { v.web.createWebArchiveDataWithCompletionHandler(&block) };
    }

    /// Save for Later's capture (channel C22): runs the constant capture
    /// code (`snorkle_core::later`) in the page's client world, main frame,
    /// awaiting its promise, and hands its string result (at most `max`
    /// bytes) to `done`; `None` otherwise.
    pub fn run_capture(&mut self, tab: TabId, body: &str, max: usize, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::call_async(self.mtm, &v.web, body, max, done),
            None => done(None),
        }
    }

    /// Shows an offline copy (web archive) in the tab, with `url` as its
    /// base URL, and JavaScript off for that load. The core has already
    /// accepted it (`Browser::use_offline_copy`).
    pub fn load_offline(&mut self, tab: TabId, url: &Url, data: Vec<u8>) {
        self.load_archive(tab, url, data, false);
    }

    /// Loads a web archive in the tab with `url` as its base URL: an
    /// offline copy (JavaScript off) or a Save for Later archive (its
    /// page's scripts run, JavaScript off only for a copy of the live
    /// document or while choosing links). The core has already accepted it
    /// (`Browser::use_offline_copy`, `Browser::show_saved_copy`).
    pub fn load_archive(&mut self, tab: TabId, url: &Url, data: Vec<u8>, scripts: bool) {
        let Some(v) = self.views.get(&tab) else { return };
        let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url.as_str())) else { return };
        OFFLINE_LOADS.with(|s| {
            let key = Retained::as_ptr(&v.web) as usize;
            if scripts {
                s.borrow_mut().remove(&key);
            } else {
                s.borrow_mut().insert(key);
            }
        });
        let data = NSData::with_bytes(&data);
        unsafe {
            v.web.loadData_MIMEType_characterEncodingName_baseURL(
                &data,
                &NSString::from_str("application/x-webarchive"),
                &NSString::from_str("utf-8"),
                &nsurl,
            );
        }
    }

    /// Image menu, once the image was fetched (`http::fetch_image`): Copy
    /// puts it on the pasteboard; Share opens the share sheet with it (as a
    /// file, from `from`, window points); Download opens the Files export
    /// picker for a copy. Files are written to a temporary folder that is
    /// emptied on the next use.
    pub fn image_result(
        &mut self,
        action: snorkle_core::imagemenu::ImageAction,
        url: &Url,
        data: Vec<u8>,
        mime: &str,
        from: (f64, f64, f64, f64),
    ) -> Result<(), String> {
        use snorkle_core::imagemenu::{download_name, ImageAction};
        if action == ImageAction::Copy {
            let image =
                UIImage::imageWithData(&NSData::with_bytes(&data)).ok_or("This kind of image cannot be copied.")?;
            unsafe { UIPasteboard::generalPasteboard().setImage(Some(&image)) };
            return Ok(());
        }
        let dir = std::env::temp_dir().join("snorkle-images");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(download_name(url, mime));
        std::fs::write(&path, &data).map_err(|e| e.to_string())?;
        if action == ImageAction::Download {
            self.export_file(&path)
        } else {
            self.share_file(&path, from)
        }
    }

    /// Opens the Files export picker for a copy of `path` (a downloaded or
    /// fetched file in the app's own folders).
    pub fn export_file(&mut self, path: &std::path::Path) -> Result<(), String> {
        let file = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let top = self.top_controller().ok_or("The file could not be shown.")?;
        let urls = NSArray::from_retained_slice(&[file]);
        let picker =
            UIDocumentPickerViewController::initForExportingURLs_asCopy(UIDocumentPickerViewController::alloc(self.mtm), &urls, true);
        top.presentViewController_animated_completion(&picker, true, None);
        Ok(())
    }

    /// The share sheet for the file at `path`, as a popover from `from`
    /// (window points).
    pub fn share_file(&mut self, path: &std::path::Path, from: (f64, f64, f64, f64)) -> Result<(), String> {
        let file = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let top = self.top_controller().ok_or("The file could not be shown.")?;
        unsafe {
            let items: Retained<NSArray> = NSArray::from_retained_slice(&[Retained::into_super(Retained::into_super(file))]);
            let vc = UIActivityViewController::initWithActivityItems_applicationActivities(
                UIActivityViewController::alloc(self.mtm),
                &items,
                None,
            );
            let Some(pop) = vc.popoverPresentationController() else { return Err("The share sheet is unavailable.".into()) };
            pop.setSourceView(Some(&self.root));
            pop.setSourceRect(rect(from));
            top.presentViewController_animated_completion(&vc, true, None);
        }
        Ok(())
    }

    /// Cancels a running download.
    pub fn cancel_download(&mut self, id: u64) {
        crate::webkit::downloads::cancel(id);
    }

    /// The user's answer to a download from a frame inside a page.
    pub fn answer_frame_download(&mut self, ask: u64, allow: bool) {
        crate::webkit::downloads::answer_frame(ask, allow);
    }

    /// Light, dark or the system's appearance for the window: the chrome
    /// and the system UI it presents. Content views keep their own
    /// override (light, or dark in dark pages mode; `set_dark`).
    /// Settings → General → Swipe between pages (also future views, through
    /// `crate::swipe_navigation`).
    pub fn set_swipe_navigation(&mut self, on: bool) {
        for v in self.views.values() {
            unsafe { v.web.setAllowsBackForwardNavigationGestures(on && CONTENT_VIEW_SPEC.allows_back_forward_gestures) };
        }
    }

    /// Settings → General → Pull to refresh (also future views, through
    /// `crate::pull_to_refresh`).
    pub fn set_pull_to_refresh(&mut self, on: bool) {
        for v in self.views.values() {
            let scroll = unsafe { v.web.scrollView() };
            if on {
                scroll.setRefreshControl(Some(&v.refresh_control));
            } else {
                if v.refresh_control.isRefreshing() {
                    v.refresh_control.endRefreshing();
                }
                scroll.setRefreshControl(None);
            }
        }
    }

    pub fn set_appearance(&mut self, appearance: crate::Appearance) {
        if let Some(window) = self.root.window() {
            window.setOverrideUserInterfaceStyle(match appearance {
                crate::Appearance::System => UIUserInterfaceStyle::Unspecified,
                crate::Appearance::Light => UIUserInterfaceStyle::Light,
                crate::Appearance::Dark => UIUserInterfaceStyle::Dark,
            });
        }
    }

    /// Keyboard shortcut bindings (validated, one per `SHORTCUTS` entry)
    /// and whether they are suspended (the editor is recording a key).
    pub fn set_shortcuts(&mut self, bindings: Vec<crate::KeyBinding>, suspend: bool) {
        menu::set_bindings(self.mtm, bindings, suspend);
    }

    /// Text editing began or ended (`crate::set_text_editing`, already
    /// set): the arrow-key shortcuts are offered again or withdrawn.
    pub fn text_editing_changed(&mut self) {
        menu::text_editing_changed(self.mtm);
    }

    /// The view controller to present system UI from: the window's root,
    /// or whatever it is already presenting.
    pub(super) fn top_controller(&self) -> Option<Retained<UIViewController>> {
        let mut vc = self.root.window()?.rootViewController()?;
        while let Some(next) = vc.presentedViewController() {
            vc = next;
        }
        Some(vc)
    }

    /// The system share sheet for `url` (an http(s) page URL from the
    /// core), as a popover pointing at `from` (window coordinates; the
    /// share button). On iPad a popover needs a source, or UIKit throws.
    pub fn share(&mut self, url: &str, from: (f64, f64, f64, f64)) {
        let from = rect(from);
        let Some(top) = self.top_controller() else { return };
        let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url)) else { return };
        unsafe {
            let items: Retained<NSArray> =
                NSArray::from_retained_slice(&[Retained::into_super(Retained::into_super(nsurl))]);
            let vc = UIActivityViewController::initWithActivityItems_applicationActivities(
                UIActivityViewController::alloc(self.mtm),
                &items,
                None,
            );
            let Some(pop) = vc.popoverPresentationController() else { return };
            pop.setSourceView(Some(&self.root));
            pop.setSourceRect(from);
            top.presentViewController_animated_completion(&vc, true, None);
        }
    }

    /// The system dictionary ("Define") for `term`.
    pub fn define(&mut self, term: &str, from: (f64, f64, f64, f64)) {
        let from = rect(from);
        let Some(top) = self.top_controller() else { return };
        let vc = UIReferenceLibraryViewController::initWithTerm(
            UIReferenceLibraryViewController::alloc(self.mtm),
            &NSString::from_str(term),
        );
        if let Some(pop) = vc.popoverPresentationController() {
            pop.setSourceView(Some(&self.root));
            pop.setSourceRect(from);
        }
        top.presentViewController_animated_completion(&vc, true, None);
    }

    /// Reads the selected text of `tab`'s page (C20; `selection::TEXT_JS`).
    pub fn read_selection(&mut self, tab: TabId, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, snorkle_core::selection::TEXT_JS, done),
            None => done(None),
        }
    }

    /// Find in page (⌘F): the constant `find::find_js` highlights the
    /// matches of `query`, scrolls to the current one and returns its
    /// position, "index/total" (channel C18; `find::parse_position`).
    pub fn find(&mut self, tab: TabId, query: String, step: snorkle_core::find::FindStep, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, &snorkle_core::find::find_js(&query, step), done),
            None => done(None),
        }
    }

    /// Removes the find highlights (closing the find bar).
    pub fn end_find(&mut self, tab: TabId) {
        if let Some(v) = self.views.get(&tab) {
            injection::run_now(self.mtm, &v.web, snorkle_core::find::CLEAR_JS);
        }
    }

    /// Summary view (C24): reads `tab`'s page as numbered blocks of text
    /// (constant code, read back like the chat's page text, C17).
    pub fn read_page_blocks(&mut self, tab: TabId, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, &snorkle_core::summary::blocks_js(), done),
            None => done(None),
        }
    }

    /// Summary view: scrolls `tab`'s page to the blocks a summary point
    /// was drawn from (constant code taking only their indices).
    pub fn scroll_to_summary(&mut self, tab: TabId, blocks: &[u32]) {
        if let Some(v) = self.views.get(&tab) {
            injection::run_now(self.mtm, &v.web, &snorkle_core::summary::scroll_js(blocks));
        }
    }

    pub(super) fn compile_scripts(&mut self) {
        let console = self.console.then_some(&self.events);
        let build = |view: &crate::ViewInjections| {
            let all: Vec<Injection> = self.injections.user.iter().chain(&view.sources).cloned().collect();
            injection::make_scripts(self.mtm, &all, console, Some(&self.events))
        };
        self.scripts = build(&self.injections.view);
        self.tab_scripts = self.injections.tabs.iter().map(|(t, v)| (*t, build(v))).collect();
    }

    pub(super) fn scripts_for(&self, tab: TabId) -> &injection::Scripts {
        self.tab_scripts.get(&tab).unwrap_or(&self.scripts)
    }

    /// The scripts to install on `controller` for `tab`. A popup's web view
    /// shares its opener's content controller, so one install serves every
    /// tab on it: a tab's own scripts (modes turned off for it) only when
    /// every other tab on that controller has the same modes, otherwise the
    /// app-wide ones. Before 2026-11-09 the last tab installed won, in hash
    /// order, so a page's next load could lose hidden media (or keep it)
    /// depending on its sibling. Per-tab modes still reach each loaded page
    /// through its `apply_js`.
    pub(super) fn scripts_on(&self, tab: TabId, controller: &WKUserContentController) -> &injection::Scripts {
        let own = self.injections.view_for(tab);
        let shared_with_other = self.views.iter().any(|(t, v)| {
            *t != tab && std::ptr::eq(&*v.controller, controller) && self.injections.view_for(*t) != own
        });
        if shared_with_other { &self.scripts } else { self.scripts_for(tab) }
    }

    /// Fetches a favicon for `host` in a sandboxed, throwaway web view (see
    /// `favicon.rs`). `done` runs on the main thread.
    pub fn fetch_favicon(&mut self, host: String, done: crate::FaviconDone) {
        favicon::fetch(self.mtm, &self.root, host, done);
    }

    /// Deletes one kind of website data from every profile's store.
    pub fn clear_data(&mut self, kind: DataKind) {
        unsafe { stores::clear_data(std::iter::once(&*self.default_store).chain(self.private_store.as_deref()), kind) }
    }
}
