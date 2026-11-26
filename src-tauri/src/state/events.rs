//! `ContentEvents` for `Events`: WebKit's reports turned into core calls and
//! chrome events.

use super::*;

impl ContentEvents for Events {
    fn decide_navigation(&self, tab: TabId, url: &Url, frame: Frame, kind: NavKind) -> Verdict {
        let (verdict, effects) = self.shared.browser().decide_navigation(tab, url, frame, kind, now_ms());
        let level = if verdict == Verdict::Cancel { console::Level::Warn } else { console::Level::Debug };
        self.shared.log(level, Some(tab), &format!("policy {frame:?} {kind:?} {url} -> {verdict:?}"));
        self.shared.run(effects);
        verdict
    }
    fn new_window_request(&self, opener: TabId, url: Url) {
        self.apply(|b| b.on_new_window_request(opener, &url, now_ms()));
    }
    fn selection(&self, tab: Option<TabId>, body: String) {
        let Some(tab) = tab else { return };
        if let Some(on) = snorkle_core::selection::parse_edit_report(&body) {
            (self.shared.emit)(ChromeEvent::PageEditing(tab, on));
            return;
        }
        // The pin guard's watcher (C25) posts to the same handler.
        if snorkle_core::pinguard::is_report(&body) {
            match snorkle_core::pinguard::parse_report(&body) {
                Some(snorkle_core::pinguard::PinReport::Tap) => self.shared.browser().on_pin_gesture(tab, now_ms()),
                Some(snorkle_core::pinguard::PinReport::Link(url)) => {
                    self.shared.log(console::Level::Info, Some(tab), &format!("pin: link {url} -> a new tab"));
                    self.apply(|b| b.on_pin_link(tab, &url, now_ms()));
                }
                None => self.shared.log(console::Level::Warn, Some(tab), "pin guard: malformed report ignored"),
            }
            return;
        }
        // The notebook's watcher (C23) posts to the same handler.
        if snorkle_core::notebook::is_report(&body) {
            match snorkle_core::notebook::parse_report(&body) {
                Some(r) => (self.shared.emit)(ChromeEvent::NotebookPick(tab, r)),
                None => self.shared.log(console::Level::Warn, Some(tab), "notebook: malformed report ignored"),
            }
            return;
        }
        // The image watcher (C21) posts to the same handler.
        if snorkle_core::imagemenu::is_image_report(&body) {
            if snorkle_core::imagemenu::is_hide_report(&body) {
                (self.shared.emit)(ChromeEvent::ImageMenu(tab, None));
                return;
            }
            match snorkle_core::imagemenu::parse_report(&body) {
                Some(r) => (self.shared.emit)(ChromeEvent::ImageMenu(tab, Some((r.url.as_str(), r.at)))),
                None => self.shared.log(console::Level::Warn, Some(tab), "image menu: malformed report ignored"),
            }
            return;
        }
        match snorkle_core::selection::parse_report(&body) {
            Some(rect) => (self.shared.emit)(ChromeEvent::Selection(tab, rect)),
            None => self.shared.log(console::Level::Warn, Some(tab), "selection menu: malformed report ignored"),
        }
    }
    fn background_tab_requested(&self, opener: TabId, url: Url) {
        let (tab, effects) = self.shared.browser().on_background_tab_request(opener, &url, now_ms());
        let result = tab.map_or("refused".to_string(), |t| format!("tab {}", t.0));
        self.shared.log(console::Level::Info, Some(opener), &format!("background tab {url} -> {result}"));
        self.shared.run(effects);
    }
    fn popup_requested(&self, opener: TabId, url: Option<Url>, style: PopupStyle) -> Option<TabId> {
        let (tab, effects) = self.shared.browser().open_popup(opener, url.as_ref(), style, now_ms());
        // A popup shares its opener's content controller, so it takes the
        // opener's view-mode exemptions (installed before it is mounted).
        if let Some(t) = tab {
            let inherited = self.shared.exempt().get(&opener).cloned();
            if let Some(set) = inherited {
                let kinds: Vec<_> = set.iter().copied().collect();
                self.shared.exempt().insert(t, set);
                let effects = self.shared.browser().set_view_exempt(t, &kinds);
                self.shared.run(effects);
                crate::scripts::apply(&self.shared, false);
            }
        }
        let shown = url.as_ref().map_or("about:blank".to_string(), |u| u.to_string());
        let result = tab.map_or("refused".to_string(), |t| format!("tab {}", t.0));
        self.shared.log(console::Level::Info, Some(opener), &format!("popup {style:?} {shown} -> {result}"));
        self.shared.run(effects);
        tab
    }
    fn close_requested(&self, tab: TabId) {
        self.apply(|b| b.on_close_requested(tab, now_ms()));
    }
    fn provisional_start(&self, tab: TabId) {
        timing::stage(&self.shared, tab, "load started", false);
        self.apply(|b| b.on_provisional_start(tab));
    }
    fn commit(&self, tab: TabId, url: Url) {
        timing::first(&self.shared, "first page committed");
        timing::stage(&self.shared, tab, "committed", false);
        self.apply(|b| b.on_commit(tab, url));
    }
    fn finish(&self, tab: TabId) {
        let (landing, page, skip) = {
            let b = self.shared.browser();
            let t = b.tab(tab);
            let page = t.and_then(|t| t.committed_url.as_ref()).map(|u| u.to_string());
            let skip = t.is_none_or(|t| t.is_private() || t.notice.is_some()) || b.shows_offline_copy(tab);
            (b.landing(tab), page, skip)
        };
        timing::stage(&self.shared, tab, "finished", true);
        self.apply(|b| b.on_finish(tab));
        if !skip {
            self.save_offline_copy(tab, landing, page.clone());
            if let Some(page) = page {
                self.save_page_text(tab, page);
            }
        }
    }
    fn fail(&self, tab: TabId, provisional: bool, kind: NavErrorKind, failing_url: Option<Url>, message: String) {
        let landing = self.shared.browser().landing(tab);
        let offline = provisional && kind == NavErrorKind::Network;
        let url = failing_url.clone();
        timing::stage(&self.shared, tab, "failed", true);
        self.apply(|b| b.on_fail(tab, provisional, kind, failing_url, &message));
        if offline {
            if let Some(url) = url {
                self.show_offline_copy(tab, url, landing);
            }
        }
    }
    fn url_changed(&self, tab: TabId, url: Url) {
        self.apply(|b| b.on_url_changed(tab, url, now_ms()));
    }
    fn title_changed(&self, tab: TabId, title: String) {
        self.apply(|b| b.on_title(tab, &title));
    }
    fn progress(&self, tab: TabId, fraction: f64) {
        self.apply(|b| b.on_progress(tab, fraction));
    }
    fn history_state(&self, tab: TabId, can_go_back: bool, can_go_forward: bool) {
        self.apply(|b| b.on_history_state(tab, can_go_back, can_go_forward));
    }
    fn secure_content(&self, tab: TabId, only_secure: bool) {
        self.apply(|b| b.on_secure_content(tab, only_secure));
    }
    fn process_terminated(&self, tab: TabId) {
        self.apply(|b| b.on_process_terminated(tab, now_ms()));
    }
    fn dialog_suppressed(&self, tab: TabId, origin_host: String) {
        if self.shared.browser().active() == Some(tab) {
            let host = snorkle_core::text::sanitize_page_text(&origin_host, 80);
            self.shared.toast(&format!("{host} tried to show a dialog. Dialogs are not supported yet."));
        }
    }
    fn key_recorded(&self, key: Option<(String, u8)>) {
        let name = match key {
            Some((k, mods)) => format!("record:{mods}:{k}"),
            None => "record:cancel".to_string(),
        };
        (self.shared.emit)(ChromeEvent::Shortcut(&name));
    }
    fn edge_pointer(&self, at: Option<(f64, f64)>) {
        (self.shared.emit)(ChromeEvent::EdgePointer(at));
    }
    fn chrome_pointer(&self, over: bool) {
        (self.shared.emit)(ChromeEvent::ChromePointer(over));
    }
    fn option_key(&self, down: bool) {
        (self.shared.emit)(ChromeEvent::OptionKey(down));
        // The pages on screen learn it too (2026-11-25): a page hears of ⌥
        // only from its own key events (when it has keyboard focus) or the
        // next pointer move, so its outline was often missing.
        let tabs: Vec<TabId> = {
            let b = self.shared.browser();
            match b.split_view() {
                Some(s) => vec![s.left, s.right],
                None => b.active().into_iter().collect(),
            }
        };
        let js = snorkle_core::notebook::alt_js(down);
        snorkle_content::dispatch_main(move || {
            with_host(|h| {
                for tab in tabs {
                    h.notebook_run(tab, &js);
                }
            })
        });
    }
    fn note(&self, text: &str) {
        self.shared.log(console::Level::Info, None, text);
    }
    fn shortcut(&self, shortcut: snorkle_content::Shortcut) {
        self.shared.log(console::Level::Debug, None, &format!("shortcut {}", shortcut.name()));
        (self.shared.emit)(ChromeEvent::Shortcut(&shortcut.name()));
    }
    fn focus_requested(&self, tab: TabId) {
        // Only meaningful in split view; otherwise the tab is already active.
        if self.shared.browser().active() != Some(tab) {
            self.apply(|b| b.activate(tab, now_ms()));
        }
    }
    fn console_message(&self, tab: Option<TabId>, origin_host: String, main_frame: bool, body: String) {
        self.shared.page_log(tab, &origin_host, main_frame, &body);
    }
    fn reload_requested(&self, tab: TabId) {
        self.apply(|b| b.reload_by_user(tab, now_ms()));
    }
    fn may_download(&self, tab: TabId, url: &Url) -> bool {
        let shared = &self.shared;
        if shared.store.is_none() || shared.browser().tab(tab).is_none() || !snorkle_core::policy::may_download(url) {
            return false;
        }
        if shared.download_bytes() >= snorkle_core::storage::MAX_DOWNLOADS_TOTAL_BYTES {
            shared.toast("Downloads are full (500 MB). Delete some in Downloads to make room.");
            return false;
        }
        if shared.downloads().iter().filter(|d| d.state == "active").count() >= MAX_ACTIVE_DOWNLOADS {
            shared.toast("Too many downloads at once. Wait for some to finish.");
            return false;
        }
        let now = Instant::now();
        let mut starts = shared.download_starts.lock().unwrap_or_else(|e| e.into_inner());
        let recent = starts.entry(tab).or_default();
        recent.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        if recent.len() >= MAX_DOWNLOADS_PER_TAB_MINUTE {
            drop(starts);
            shared.log(console::Level::Warn, Some(tab), &format!("download refused (too many from this tab): {url}"));
            shared.toast("This page is starting many downloads; further ones were blocked.");
            return false;
        }
        recent.push(now);
        true
    }
    fn download_destination(
        &self,
        tab: TabId,
        source: Option<Url>,
        suggested: String,
        mime: String,
        expected: Option<u64>,
    ) -> Option<(u64, std::path::PathBuf)> {
        let shared = &self.shared;
        let store = shared.store.clone()?;
        if expected.is_some_and(|n| shared.download_bytes().saturating_add(n) > snorkle_core::storage::MAX_DOWNLOADS_TOTAL_BYTES) {
            shared.toast("That file does not fit in Downloads (500 MB in all). Delete some to make room.");
            return None;
        }
        let private = shared.browser().tab(tab).is_none_or(|t| t.is_private());
        let (id, name, path) = store.new_download(&suggested).ok()?;
        let web = source.filter(|u| matches!(u.scheme(), "http" | "https"));
        let item = DownloadItem {
            id,
            name: name.clone(),
            source: web.as_ref().map(snorkle_core::url_display::display),
            source_url: web.as_ref().map(|u| u.to_string()).unwrap_or_default(),
            mime: snorkle_core::text::sanitize_page_text(&mime, 100),
            received: 0,
            expected,
            state: "active",
            error: String::new(),
            private,
            started_ms: wall_ms(),
            finished_ms: None,
        };
        shared.downloads().insert(0, item);
        shared.log(console::Level::Info, Some(tab), &format!("download {id} started: {name}"));
        // The chrome shows the progress, and the end, from `downloads`.
        shared.emit_downloads();
        Some((id, path))
    }
    fn download_progress(&self, id: u64, received: u64, expected: Option<u64>) {
        let changed = {
            let mut d = self.shared.downloads();
            match d.iter_mut().find(|x| x.id == id && x.state == "active") {
                Some(x) if x.received != received || (expected.is_some() && x.expected != expected) => {
                    x.received = received;
                    if expected.is_some() {
                        x.expected = expected;
                    }
                    true
                }
                _ => false,
            }
        };
        if changed {
            let over = self.shared.download_bytes() > snorkle_core::storage::MAX_DOWNLOADS_TOTAL_BYTES;
            if over && self.shared.capped_downloads.lock().unwrap_or_else(|e| e.into_inner()).insert(id) {
                snorkle_content::dispatch_main(move || with_host(|h| h.cancel_download(id)));
            }
            self.shared.emit_downloads();
        }
    }
    fn frame_download_requested(&self, tab: TabId, ask: u64, url: &Url) {
        let from = match url.scheme() {
            "http" | "https" => snorkle_core::url_display::display(url).registrable,
            _ => String::new(),
        };
        self.shared.log(console::Level::Info, Some(tab), &format!("download from a frame asks the user: {url}"));
        (self.shared.emit)(ChromeEvent::DownloadPrompt { ask, tab, from });
    }
    fn download_finished(&self, id: u64, error: Option<String>) {
        use snorkle_core::storage::{DownloadOutcome, DownloadRecord};
        let shared = &self.shared;
        let cancelled = shared.cancelled_downloads.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        let capped = shared.capped_downloads.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        let error = if capped && error.is_some() {
            Some("Stopped: downloads are limited to 500 MB in all.".to_string())
        } else {
            error
        };
        let item = {
            let mut d = shared.downloads();
            let Some(x) = d.iter_mut().find(|x| x.id == id) else { return };
            x.finished_ms = Some(wall_ms());
            x.state = match (&error, cancelled && !capped) {
                (None, _) => "done",
                (Some(_), true) => "cancelled",
                (Some(_), false) => "failed",
            };
            if x.state == "done" {
                x.expected = Some(x.received.max(x.expected.unwrap_or(0)));
            }
            x.error = if x.state == "failed" { error.clone().unwrap_or_default() } else { String::new() };
            x.clone()
        };
        if let Some(store) = &shared.store {
            // A partial file is of no use; the folder goes.
            if item.state != "done" {
                let _ = store.delete_download(id);
            }
            if !item.private {
                let size = store.download_path(id, &item.name).and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len());
                let rec = DownloadRecord {
                    id,
                    name: item.name.clone(),
                    source: item.source_url.clone(),
                    mime: item.mime.clone(),
                    bytes: size.unwrap_or(item.received),
                    outcome: match item.state {
                        "done" => DownloadOutcome::Done,
                        "cancelled" => DownloadOutcome::Cancelled,
                        _ => DownloadOutcome::Failed,
                    },
                    error: item.error.clone(),
                    started_ms: item.started_ms,
                    finished_ms: item.finished_ms.unwrap_or(0),
                };
                if let Some(bytes) = size {
                    if let Some(x) = shared.downloads().iter_mut().find(|x| x.id == id) {
                        x.received = bytes;
                        x.expected = Some(bytes);
                    }
                }
                let _ = store.record_download(rec);
            }
        }
        shared.log(console::Level::Info, None, &format!("download {id} {}: {}", item.state, item.name));
        shared.emit_downloads();
    }
    fn app_active(&self) {
        crate::open_shared_links(&self.shared);
    }
    fn memory_warning(&self) {
        self.apply(|b| b.on_memory_warning());
    }
    fn did_enter_background(&self) {
        self.apply(|b| b.tick(now_ms()));
        self.shared.storage(StorageJob::Flush);
    }
}

/// Creates the content host on the main thread, attached to the chrome
/// window's root view.
///
/// # Safety
/// `root_view` must be the `UIView*` of the main window (iOS), its
/// `NSWindow*` (macOS) or null (other platforms), and this must run on the
/// main thread.
pub unsafe fn install_host(root_view: *mut std::ffi::c_void, shared: Arc<Shared>) {
    let events: Rc<dyn ContentEvents> = Rc::new(Events { shared });
    let host = ContentHost::new(root_view, events);
    HOST.with(|h| *h.borrow_mut() = Some(host));
}
