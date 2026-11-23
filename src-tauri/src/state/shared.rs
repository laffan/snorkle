//! `Shared`: running the core's effects, the chrome's events, logging, toasts.

use super::*;

impl Shared {
    pub fn new(browser: Browser, store: Option<Store>, emit: Emitter) -> Arc<Self> {
        let store = store.map(Arc::new);
        let downloads: Vec<DownloadItem> =
            store.as_ref().map(|st| st.downloads().iter().map(DownloadItem::from_record).collect()).unwrap_or_default();
        let modes = store.as_ref().map(|s| s.launch_modes().modes()).unwrap_or_default();
        let storage_tx = store.clone().map(|s| Mutex::new(spawn_storage_worker(s)));
        Arc::new(Shared {
            browser: Mutex::new(browser),
            store,
            storage_tx,
            emit,
            view_modes: Mutex::new(modes),
            view_exempt: Mutex::default(),
            chat_seq: std::sync::atomic::AtomicU64::new(0),
            shortcuts: Mutex::new(snorkle_content::effective_bindings(&[]).unwrap_or_default()),
            console: Mutex::default(),
            console_capture: AtomicBool::new(cfg!(debug_assertions)),
            downloads: Mutex::new(downloads),
            cancelled_downloads: Mutex::default(),
            capped_downloads: Mutex::default(),
            download_starts: Mutex::default(),
        })
    }

    pub fn downloads(&self) -> MutexGuard<'_, Vec<DownloadItem>> {
        self.downloads.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Bytes all downloads take (finished files and what running ones
    /// have received), against `storage::MAX_DOWNLOADS_TOTAL_BYTES`.
    pub fn download_bytes(&self) -> u64 {
        self.downloads()
            .iter()
            .filter(|d| matches!(d.state, "active" | "done"))
            .map(|d| d.received.max(if d.state == "done" { d.expected.unwrap_or(0) } else { 0 }))
            .sum()
    }

    /// Sends the downloads list to the chrome.
    pub fn emit_downloads(&self) {
        let list = self.downloads().clone();
        (self.emit)(ChromeEvent::Downloads(&list));
    }

    /// The private session ended: its downloads are cancelled and their
    /// files deleted (they were never listed in `downloads.json`).
    pub(super) fn drop_private_downloads(&self) {
        let gone: Vec<(u64, bool)> = {
            let mut d = self.downloads();
            let gone = d.iter().filter(|x| x.private).map(|x| (x.id, x.state == "active")).collect();
            d.retain(|x| !x.private);
            gone
        };
        if gone.is_empty() {
            return;
        }
        for (id, active) in &gone {
            if *active {
                let id = *id;
                self.cancelled_downloads.lock().unwrap_or_else(|e| e.into_inner()).insert(id);
                snorkle_content::dispatch_main(move || with_host(|h| h.cancel_download(id)));
            }
            if let Some(store) = &self.store {
                let _ = store.delete_download(*id);
            }
        }
        self.emit_downloads();
    }

    pub fn browser(&self) -> MutexGuard<'_, Browser> {
        self.browser.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn view_modes(&self) -> ViewModes {
        *self.view_modes.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn exempt(&self) -> MutexGuard<'_, HashMap<TabId, BTreeSet<viewmodes::Kind>>> {
        self.view_exempt.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The view modes in effect for `tab`: the app-wide ones minus the
    /// tab's exemptions.
    pub fn modes_for(&self, tab: TabId) -> ViewModes {
        let exempt = self.exempt().get(&tab).cloned().unwrap_or_default();
        self.view_modes().for_tab(&exempt)
    }

    pub fn view(&self) -> ChromeState {
        self.browser().view()
    }

    pub fn toast(&self, msg: &str) {
        (self.emit)(ChromeEvent::Toast(msg));
    }

    pub fn console_log(&self) -> MutexGuard<'_, ConsoleLog> {
        self.console.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn capturing(&self) -> bool {
        self.console_capture.load(Ordering::Relaxed)
    }

    /// Records an app message in the console (if capturing). Must be called
    /// WITHOUT the browser lock held (it checks whether `tab` is private).
    pub fn log(&self, level: console::Level, tab: Option<TabId>, text: &str) {
        if cfg!(debug_assertions) {
            eprintln!("[snorkle] {text}");
        }
        if !self.capturing() {
            return;
        }
        let private = tab.is_some_and(|t| self.browser().tab(t).is_some_and(|t| t.is_private()));
        let entry = self.console_log().app(now_ms(), level, tab, private, text).clone();
        (self.emit)(ChromeEvent::Console(&entry));
    }

    /// Records what a page wrote to its console (see `ContentEvents`).
    pub(super) fn page_log(&self, tab: Option<TabId>, host: &str, main_frame: bool, body: &str) {
        if !self.capturing() {
            return;
        }
        let private = tab.is_some_and(|t| self.browser().tab(t).is_some_and(|t| t.is_private()));
        let entry = self.console_log().page(now_ms(), tab, private, host, main_frame, body).cloned();
        if let Some(entry) = entry {
            (self.emit)(ChromeEvent::Console(&entry));
        }
    }

    /// Routes effects. Must be called WITHOUT the browser lock held.
    pub fn run(self: &Arc<Self>, effects: Vec<Effect>) {
        self.log_effects(&effects);
        let mut host_effects = Vec::new();
        let mut notify = false;
        let mut progress = Vec::new();
        for e in effects {
            match e {
                Effect::NotifyChrome => notify = true,
                Effect::NotifyProgress(tab) => progress.push(tab),
                Effect::RecordHistory { url, title } => {
                    self.storage(StorageJob::History { url: url.to_string(), title })
                }
                Effect::PersistSession(s) => self.storage(StorageJob::Session(s)),
                Effect::OpenSavedPage { tab, item, url } => crate::saved::open_page(self.clone(), tab, item, url),
                Effect::ReleasePrivateData => {
                    self.drop_private_downloads();
                    self.console_log().drop_private();
                    (self.emit)(ChromeEvent::ConsoleReset);
                    host_effects.push(Effect::ReleasePrivateData);
                }
                other => host_effects.push(other),
            }
        }
        if !host_effects.is_empty() {
            snorkle_content::dispatch_main(move || {
                HOST.with(|h| {
                    if let Some(host) = h.borrow_mut().as_mut() {
                        for e in &host_effects {
                            host.apply(e);
                        }
                    }
                })
            });
        }
        if notify {
            let state = self.view();
            (self.emit)(ChromeEvent::State(&state));
        } else {
            // Progress alone: the tab's numbers, not the whole state.
            for tab in progress {
                let b = self.browser();
                if let Some(t) = b.tab(tab) {
                    let (loading, percent) = (t.loading, t.progress);
                    drop(b);
                    (self.emit)(ChromeEvent::Progress(tab, loading, percent));
                }
            }
        }
    }

    pub(crate) fn storage(&self, job: StorageJob) {
        if let Some(tx) = &self.storage_tx {
            let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(job);
        }
    }

    /// Writes a pending session before the app quits, waiting at most
    /// `limit` for the storage thread.
    pub fn flush_blocking(&self, limit: Duration) {
        let (tx, rx) = mpsc::channel();
        self.storage(StorageJob::FlushNow(tx));
        let _ = rx.recv_timeout(limit);
    }
}

impl Shared {
    /// Logs what the core decided, so navigation can be followed in the
    /// in-app console (while capturing) and, in debug builds, the terminal
    /// running `tauri dev` / `tauri ios dev`. Entries about private tabs
    /// are dropped from the console when the private session ends.
    pub(super) fn log_effects(&self, effects: &[Effect]) {
        if !cfg!(debug_assertions) && !self.capturing() {
            return;
        }
        let private: HashSet<TabId> = {
            let b = self.browser();
            b.view().tabs.iter().filter(|t| t.private).map(|t| t.id).collect()
        };
        for e in effects {
            let tab = match e {
                Effect::NotifyChrome | Effect::NotifyProgress(_) | Effect::PersistSession(_) => continue,
                Effect::CreateView { tab, .. }
                | Effect::AdoptPopup { tab }
                | Effect::DestroyView { tab, .. }
                | Effect::ShowView { tab, .. }
                | Effect::SetZoom { tab, .. }
                | Effect::Load { tab, .. } => Some(*tab),
                Effect::HideView(t)
                | Effect::GoBack(t)
                | Effect::GoForward(t)
                | Effect::Reload(t)
                | Effect::Stop(t) => Some(*t),
                _ => None,
            };
            if cfg!(debug_assertions) {
                eprintln!("[snorkle] effect {e:?}");
            }
            if self.capturing() {
                let text = format!("effect {e:?}");
                let p = tab.is_some_and(|t| private.contains(&t));
                let entry = self.console_log().app(now_ms(), console::Level::Debug, tab, p, &text).clone();
                (self.emit)(ChromeEvent::Console(&entry));
            }
        }
    }
}
