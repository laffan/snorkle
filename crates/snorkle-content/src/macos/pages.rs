//! The macOS content host: operations on one page (injections, snapshots,
//! clips, the notebook, page text, offline copies and archives, the image
//! menu, files, downloads, settings, sharing, Define, find, the summary,
//! scripts, favicons, website data).

use super::*;

impl ContentHost {
    /// Replaces everything injected; see the iPadOS host.
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

    /// Turns page console capture on or off (from the next page load).
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

    /// Save for Later: a JPEG thumbnail of what the tab's view shows now
    /// (its visible bounds, `THUMB_WIDTH` points wide), or `None` without a
    /// live view or if WebKit cannot draw it.
    pub fn thumbnail(&mut self, tab: TabId, done: crate::ThumbDone) {
        let Some(v) = self.views.get(&tab) else {
            done(None);
            return;
        };
        let done = RefCell::new(Some(done));
        unsafe {
            let cfg = objc2_web_kit::WKSnapshotConfiguration::new(self.mtm);
            cfg.setSnapshotWidth(Some(&objc2_foundation::NSNumber::new_f64(crate::THUMB_WIDTH)));
            let block = block2::RcBlock::new(move |image: *mut NSImage, _err: *mut NSError| {
                let jpeg = image.as_ref().and_then(thumbnail_jpeg);
                if let Some(f) = done.borrow_mut().take() {
                    f(jpeg);
                }
            });
            v.web.takeSnapshotWithConfiguration_completionHandler(Some(&cfg), &block);
        }
    }

    /// Notebook (2026-11-15): a JPEG of a region of `tab`'s page as its
    /// view draws it now (see the iPadOS host; `crate::clip_rect`). The
    /// web view is flipped (top-left origin), as the rectangle is.
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
            cfg.setRect(objc2_foundation::NSRect::new(objc2_foundation::NSPoint::new(x, y), objc2_foundation::NSSize::new(w, h)));
            cfg.setSnapshotWidth(Some(&objc2_foundation::NSNumber::new_f64(w.min(crate::CLIP_MAX_WIDTH))));
            let block = block2::RcBlock::new(move |image: *mut NSImage, _err: *mut NSError| {
                let jpeg = image.as_ref().and_then(|i| crate::clip_jpeg(|q| jpeg_data(i, q)));
                if let Some(f) = done.borrow_mut().take() {
                    f(jpeg);
                }
            });
            v.web.takeSnapshotWithConfiguration_completionHandler(Some(&cfg), &block);
        }
    }

    /// Runs constant notebook code in `tab`'s page (see the iPadOS host).
    pub fn notebook_run(&mut self, tab: TabId, source: &str) {
        if let Some(v) = self.views.get(&tab) {
            injection::run_now(self.mtm, &v.web, source);
        }
    }

    /// Reads the sentence under the notebook's quote button (C23).
    pub fn notebook_read(&mut self, tab: TabId, source: &str, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, source, done),
            None => done(None),
        }
    }

    /// A page shown without its view modes, or with them again, for a
    /// notebook clip (see the iPadOS host).
    pub fn plain_page(&mut self, tab: TabId, on: bool) {
        let Some(v) = self.views.get(&tab) else { return };
        if on {
            injection::run_now(self.mtm, &v.web, &snorkle_core::viewmodes::plain_js());
        } else {
            let view = self.injections.view_for(tab);
            injection::run_now(self.mtm, &v.web, &view.apply_js);
        }
    }

    /// ⋯ → Open in App is an iPadOS feature (AppKit has no "universal
    /// links only" open); the chrome does not offer it here.
    pub fn open_in_app(&mut self, _url: &Url, done: crate::OpenedDone) {
        done(false);
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
    /// `view` must be a valid `NSView*` that outlives the host, or null.
    pub unsafe fn set_chrome_view(&mut self, view: *mut c_void) {
        self.chrome = unsafe { Retained::retain(view.cast::<NSView>()) };
    }

    /// Gives the chrome's web view keyboard focus.
    pub fn focus_chrome(&mut self) {
        if let Some(v) = self.chrome.as_ref() {
            self.window.makeFirstResponder(Some(v));
        }
    }

    /// Keyboard focus follows the pointer over the notebook (owner request
    /// 2026-11-16): `on`, the chrome's web view becomes first responder and
    /// what had focus (a page) is remembered; off, that is given focus
    /// again if the chrome still has it (no click elsewhere meanwhile) and
    /// it is still in the window. Returns whether the chrome has focus.
    pub fn hover_focus(&mut self, on: bool) -> bool {
        let Some(chrome) = self.chrome.clone() else { return false };
        let chrome_has = |w: &NSWindow| {
            w.firstResponder().is_some_and(|r| r.downcast_ref::<NSView>().is_some_and(|v| v.isDescendantOf(&chrome)))
        };
        if on {
            if !chrome_has(&self.window) {
                self.before_hover = self.window.firstResponder();
                self.window.makeFirstResponder(Some(&chrome));
            }
            return chrome_has(&self.window);
        }
        if let Some(prev) = self.before_hover.take() {
            let in_window = prev
                .downcast_ref::<NSView>()
                .is_some_and(|v| v.window().is_some_and(|w| Retained::as_ptr(&w) == Retained::as_ptr(&self.window)));
            if in_window && chrome_has(&self.window) {
                self.window.makeFirstResponder(Some(&prev));
            }
        }
        chrome_has(&self.window)
    }

    /// Saves the tab's page as a web archive (offline copy).
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

    /// Shows an offline copy (web archive), JavaScript off for that load.
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

    /// Image menu, once the image was fetched: Copy puts it on the
    /// pasteboard; Share opens the share menu with it (as a file, from
    /// `from`, window points from the top left); Download asks where to
    /// save it (a save panel attached to the window). Files for Share are
    /// written to a temporary folder that is emptied on the next use.
    pub fn image_result(
        &mut self,
        action: snorkle_core::imagemenu::ImageAction,
        url: &Url,
        data: Vec<u8>,
        mime: &str,
        from: (f64, f64, f64, f64),
    ) -> Result<(), String> {
        use snorkle_core::imagemenu::{download_name, ImageAction};
        let bytes = NSData::with_bytes(&data);
        match action {
            ImageAction::Copy => {
                let image =
                    NSImage::initWithData(NSImage::alloc(), &bytes).ok_or("This kind of image cannot be copied.")?;
                let pasteboard = NSPasteboard::generalPasteboard();
                pasteboard.clearContents();
                let items = NSArray::from_retained_slice(&[image]);
                let ok: bool = unsafe { msg_send![&*pasteboard, writeObjects: &*items] };
                if ok {
                    Ok(())
                } else {
                    Err("The image could not be copied.".into())
                }
            }
            ImageAction::Download => {
                let dir = std::env::temp_dir().join("snorkle-images");
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let path = dir.join(download_name(url, mime));
                std::fs::write(&path, &data).map_err(|e| e.to_string())?;
                self.export_file(&path)
            }
            ImageAction::Share => {
                let dir = std::env::temp_dir().join("snorkle-images");
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let path = dir.join(download_name(url, mime));
                std::fs::write(&path, &data).map_err(|e| e.to_string())?;
                self.share_file(&path, from)
            }
        }
    }

    /// Asks where to save a copy of `path` (a save-panel sheet on the
    /// window) and copies it there with its attributes (the quarantine
    /// mark of a download goes with it).
    pub fn export_file(&mut self, path: &std::path::Path) -> Result<(), String> {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "download".into());
        let source = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let panel = NSSavePanel::savePanel(self.mtm);
        panel.setNameFieldStringValue(&NSString::from_str(&name));
        let p = panel.clone();
        let done = RcBlock::new(move |response: isize| {
            if response != NSModalResponseOK {
                return;
            }
            let Some(dest) = p.URL() else { return };
            let fm = NSFileManager::defaultManager();
            // The panel already asked whether to replace an existing file.
            let _ = fm.removeItemAtURL_error(&dest);
            let _ = fm.copyItemAtURL_toURL_error(&source, &dest);
        });
        panel.beginSheetModalForWindow_completionHandler(&self.window, &done);
        Ok(())
    }

    /// The share menu for the file at `path`, pointing at `from`.
    pub fn share_file(&mut self, path: &std::path::Path, from: (f64, f64, f64, f64)) -> Result<(), String> {
        let file = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        self.show_sharing(Retained::into_super(Retained::into_super(file)), from);
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

    /// The share menu for one item, pointing at `from` (window points from
    /// the top left).
    pub(super) fn show_sharing(&mut self, item: Retained<AnyObject>, from: (f64, f64, f64, f64)) {
        let items: Retained<NSArray> = NSArray::from_retained_slice(&[item]);
        let picker = unsafe { NSSharingServicePicker::initWithItems(NSSharingServicePicker::alloc(), &items) };
        let rect = rect(from).ns(self.height());
        picker.showRelativeToRect_ofView_preferredEdge(rect, &self.root, NSRectEdge::MaxY);
        self.sharing = Some(picker);
    }

    /// macOS has no pull to refresh.
    pub fn set_pull_to_refresh(&mut self, _on: bool) {}

    /// Settings → General → Swipe between pages (also future views, through
    /// `crate::swipe_navigation`).
    pub fn set_swipe_navigation(&mut self, on: bool) {
        for v in self.views.values() {
            unsafe { v.web.setAllowsBackForwardNavigationGestures(on && CONTENT_VIEW_SPEC.allows_back_forward_gestures) };
        }
    }

    /// Light, dark or the system's appearance for the window: the chrome
    /// and the system UI it presents. Content views keep their own
    /// appearance (Aqua, or Dark Aqua in dark pages mode; `set_dark`).
    pub fn set_appearance(&mut self, appearance: crate::Appearance) {
        let named = |name: &NSString| NSAppearance::appearanceNamed(name);
        let a = match appearance {
            crate::Appearance::System => None,
            crate::Appearance::Light => named(unsafe { NSAppearanceNameAqua }),
            crate::Appearance::Dark => named(unsafe { NSAppearanceNameDarkAqua }),
        };
        let _: () = unsafe { msg_send![&*self.window, setAppearance: a.as_deref()] };
    }

    /// Keyboard shortcut bindings and whether they are suspended (the
    /// editor is recording a key).
    pub fn set_shortcuts(&mut self, bindings: Vec<crate::KeyBinding>, suspend: bool) {
        keys::set_bindings(bindings, suspend);
    }

    /// Text editing began or ended: the key monitor reads
    /// `crate::text_editing` on each key press, so nothing to do here.
    pub fn text_editing_changed(&mut self) {}

    /// The system share menu for `url` (an http(s) page URL from the core),
    /// pointing at `from` (the share button).
    pub fn share(&mut self, url: &str, from: (f64, f64, f64, f64)) {
        let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url)) else { return };
        self.show_sharing(Retained::into_super(Retained::into_super(nsurl)), from);
    }

    /// The system dictionary ("Look Up") for `term`, at the selection.
    pub fn define(&mut self, term: &str, from: (f64, f64, f64, f64)) {
        let r = rect(from).ns(self.height());
        let text = NSAttributedString::from_nsstring(&NSString::from_str(term));
        self.root.showDefinitionForAttributedString_atPoint(Some(&text), NSPoint::new(r.origin.x, r.origin.y));
    }

    pub fn read_selection(&mut self, tab: TabId, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, snorkle_core::selection::TEXT_JS, done),
            None => done(None),
        }
    }

    pub fn find(&mut self, tab: TabId, query: String, step: snorkle_core::find::FindStep, done: crate::TextDone) {
        match self.views.get(&tab) {
            Some(v) => injection::read_now(self.mtm, &v.web, &snorkle_core::find::find_js(&query, step), done),
            None => done(None),
        }
    }

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

    pub fn fetch_favicon(&mut self, host: String, done: crate::FaviconDone) {
        favicon::fetch(self.mtm, &self.root, host, done);
    }

    pub fn clear_data(&mut self, kind: DataKind) {
        unsafe { stores::clear_data(std::iter::once(&*self.default_store).chain(self.private_store.as_deref()), kind) }
    }
}
