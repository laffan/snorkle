//! Glue between the core state machine, the content host, storage and the
//! chrome. No browser logic lives here: effects come out of
//! `snorkle_core::browser::Browser` and are routed to whoever executes them.
//!
//! Threading:
//! - `Browser` sits behind a `Mutex`. The lock is held only while the core
//!   computes effects; it is never held while UIKit/WebKit code runs.
//! - Content-host effects are applied on the main thread via
//!   `snorkle_content::dispatch_main`, which is always asynchronous on iOS,
//!   so WebKit delegate callbacks never re-enter the host.
//! - Disk writes happen on a dedicated storage thread.

use std::cell::RefCell;
use std::rc::Rc;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use snorkle_content::{ContentEvents, ContentHost};
use snorkle_core::browser::{Browser, ChromeState, Effect, NavErrorKind, NavKind, PopupStyle, Verdict};
use snorkle_core::console::{self, ConsoleLog};
use snorkle_core::policy::Frame;
use snorkle_core::storage::{Session, Store};
use snorkle_core::tabs::TabId;
use snorkle_core::viewmodes::{self, Heading, ViewModes};
use snorkle_core::Url;

/// Monotonic milliseconds since app start (for tab LRU/TTL bookkeeping).
pub fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

fn wall_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub enum StorageJob {
    History { url: String, title: String },
    Session(Session),
}

/// Chrome notifications. `State` carries the full chrome state (small: one
/// entry per tab), `Toast` a short non-blocking message.
pub enum ChromeEvent<'a> {
    State(&'a ChromeState),
    Toast(&'a str),
    /// A native keyboard shortcut for the chrome to perform.
    Shortcut(&'a str),
    /// Pinned grid content (e.g. a favicon) changed.
    PinsChanged,
    /// A new console entry.
    Console(&'a console::Entry),
    /// A tab's page outline changed (headers view).
    Headings(TabId, &'a [Heading]),
    /// The heading a tab's reader has scrolled to (index into its outline).
    HeadingCurrent(TabId, u32),
    /// Console entries were removed (cleared, or the private session ended).
    ConsoleReset,
}

pub type Emitter = Box<dyn Fn(ChromeEvent<'_>) + Send + Sync>;

pub struct Shared {
    browser: Mutex<Browser>,
    pub store: Option<Arc<Store>>,
    storage_tx: Option<Mutex<Sender<StorageJob>>>,
    pub(crate) emit: Emitter,
    /// App-wide view modes. In memory only, so every launch starts with
    /// the defaults (black and white, media hidden).
    pub view_modes: Mutex<ViewModes>,
    /// Tabs exempted from view modes ("Disable for this tab"). In memory only.
    pub view_exempt: Mutex<HashMap<TabId, BTreeSet<viewmodes::Kind>>>,
    /// Latest outline per tab (headers view).
    pub headings: Mutex<HashMap<TabId, Vec<Heading>>>,
    /// Tabs with the headers view on (it is a per-tab mode).
    pub headers_tabs: Mutex<HashSet<TabId>>,
    /// "Detect Headers" requests waiting for their page's sample.
    pub pending_samples: Mutex<HashMap<TabId, mpsc::Sender<String>>>,
    /// In-app console (in memory only).
    pub console: Mutex<ConsoleLog>,
    /// Whether the console records (and pages get the console hook). On by
    /// default in debug builds only.
    pub console_capture: AtomicBool,
}

impl Shared {
    pub fn new(browser: Browser, store: Option<Store>, emit: Emitter) -> Arc<Self> {
        let store = store.map(Arc::new);
        let storage_tx = store.clone().map(|s| Mutex::new(spawn_storage_worker(s)));
        Arc::new(Shared {
            browser: Mutex::new(browser),
            store,
            storage_tx,
            emit,
            view_modes: Mutex::default(),
            view_exempt: Mutex::default(),
            headings: Mutex::default(),
            headers_tabs: Mutex::default(),
            pending_samples: Mutex::default(),
            console: Mutex::default(),
            console_capture: AtomicBool::new(cfg!(debug_assertions)),
        })
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

    pub fn headers_tabs(&self) -> MutexGuard<'_, HashSet<TabId>> {
        self.headers_tabs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The view modes in effect for `tab`: the app-wide ones minus the
    /// tab's exemptions, plus the headers view where the tab has it on.
    pub fn modes_for(&self, tab: TabId) -> ViewModes {
        let exempt = self.exempt().get(&tab).cloned().unwrap_or_default();
        let mut m = self.view_modes().for_tab(&exempt);
        m.headers = self.headers_tabs().contains(&tab);
        m
    }

    pub fn headings_of(&self) -> MutexGuard<'_, HashMap<TabId, Vec<Heading>>> {
        self.headings.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_headings(&self, tab: TabId, list: Vec<Heading>) {
        (self.emit)(ChromeEvent::Headings(tab, &list));
        self.headings_of().insert(tab, list);
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
    fn page_log(&self, tab: Option<TabId>, host: &str, main_frame: bool, body: &str) {
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
        for e in effects {
            match e {
                Effect::NotifyChrome => notify = true,
                Effect::RecordHistory { url, title } => {
                    self.storage(StorageJob::History { url: url.to_string(), title })
                }
                Effect::PersistSession(s) => self.storage(StorageJob::Session(s)),
                Effect::ReleasePrivateData => {
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
        }
    }

    fn storage(&self, job: StorageJob) {
        if let Some(tx) = &self.storage_tx {
            let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(job);
        }
    }
}

impl Shared {
    /// Logs what the core decided, so navigation can be followed in the
    /// in-app console (while capturing) and, in debug builds, the terminal
    /// running `tauri dev` / `tauri ios dev`. Entries about private tabs
    /// are dropped from the console when the private session ends.
    fn log_effects(&self, effects: &[Effect]) {
        if !cfg!(debug_assertions) && !self.capturing() {
            return;
        }
        let private: HashSet<TabId> = {
            let b = self.browser();
            b.view().tabs.iter().filter(|t| t.private).map(|t| t.id).collect()
        };
        for e in effects {
            let tab = match e {
                Effect::NotifyChrome | Effect::PersistSession(_) => continue,
                Effect::CreateView { tab, .. }
                | Effect::AdoptPopup { tab }
                | Effect::DestroyView { tab, .. }
                | Effect::ShowView { tab, .. }
                | Effect::Load { tab, .. } => Some(*tab),
                Effect::HideView(t) | Effect::GoBack(t) | Effect::GoForward(t) | Effect::Reload(t) | Effect::Stop(t) => {
                    Some(*t)
                }
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

/// Storage worker: history appends are written as they come; session
/// snapshots are coalesced (only the newest within a short window is
/// written), since every commit produces one.
fn spawn_storage_worker(store: Arc<Store>) -> Sender<StorageJob> {
    let (tx, rx) = mpsc::channel::<StorageJob>();
    std::thread::Builder::new()
        .name("snorkle-storage".into())
        .spawn(move || {
            let mut pending: Option<Session> = None;
            loop {
                let job = if pending.is_some() {
                    match rx.recv_timeout(Duration::from_millis(400)) {
                        Ok(j) => Some(j),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                } else {
                    match rx.recv() {
                        Ok(j) => Some(j),
                        Err(_) => break,
                    }
                };
                match job {
                    Some(StorageJob::History { url, title }) => {
                        let _ = store.add_history(&url, &title, wall_ms());
                    }
                    Some(StorageJob::Session(s)) => pending = Some(s),
                    None => {
                        if let Some(s) = pending.take() {
                            let _ = store.save_session(&s);
                        }
                    }
                }
            }
            if let Some(s) = pending.take() {
                let _ = store.save_session(&s);
            }
        })
        .expect("spawn storage thread");
    tx
}

thread_local! {
    /// The content host lives on the main thread only (UIKit objects).
    pub static HOST: RefCell<Option<ContentHost>> = const { RefCell::new(None) };
}

/// Runs `f` with the content host if it exists on this thread.
pub fn with_host(f: impl FnOnce(&mut ContentHost)) {
    HOST.with(|h| {
        if let Some(host) = h.borrow_mut().as_mut() {
            f(host)
        }
    })
}

/// WebKit → core bridge. Called on the main thread from delegate
/// callbacks; locks the core briefly and routes the resulting effects.
pub struct Events {
    pub shared: Arc<Shared>,
}

impl Events {
    fn apply(&self, f: impl FnOnce(&mut Browser) -> Vec<Effect>) {
        let effects = f(&mut self.shared.browser());
        self.shared.run(effects);
    }
}

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
    fn popup_requested(&self, opener: TabId, url: Option<Url>, style: PopupStyle) -> Option<TabId> {
        let (tab, effects) = self.shared.browser().open_popup(opener, url.as_ref(), style, now_ms());
        // A popup shares its opener's content controller, so it takes the
        // opener's view-mode exemptions (installed before it is mounted).
        if let Some(t) = tab {
            let inherited = self.shared.exempt().get(&opener).cloned();
            if let Some(set) = inherited {
                self.shared.exempt().insert(t, set);
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
        self.apply(|b| b.on_provisional_start(tab));
    }
    fn commit(&self, tab: TabId, url: Url) {
        // A new document: its outline comes from its own extractor.
        if self.shared.headings_of().contains_key(&tab) {
            self.shared.set_headings(tab, Vec::new());
        }
        self.apply(|b| b.on_commit(tab, url));
    }
    fn finish(&self, tab: TabId) {
        self.apply(|b| b.on_finish(tab));
    }
    fn fail(&self, tab: TabId, provisional: bool, kind: NavErrorKind, failing_url: Option<Url>, message: String) {
        self.apply(|b| b.on_fail(tab, provisional, kind, failing_url, &message));
    }
    fn url_changed(&self, tab: TabId, url: Url) {
        self.apply(|b| b.on_url_changed(tab, url));
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
    fn shortcut(&self, shortcut: snorkle_content::Shortcut) {
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
    fn headings(&self, tab: Option<TabId>, body: String) {
        let Some(tab) = tab else { return };
        match viewmodes::parse_report(&body) {
            Some(viewmodes::HeadingsReport::Outline(list)) => self.shared.set_headings(tab, list),
            Some(viewmodes::HeadingsReport::Current(i)) => (self.shared.emit)(ChromeEvent::HeadingCurrent(tab, i)),
            Some(viewmodes::HeadingsReport::Sample(sample)) => {
                let waiting = self.shared.pending_samples.lock().unwrap_or_else(|e| e.into_inner()).remove(&tab);
                if let Some(tx) = waiting {
                    let _ = tx.send(sample);
                }
            }
            None => self.shared.log(console::Level::Warn, Some(tab), "headers view: malformed report ignored"),
        }
    }
    fn reload_requested(&self, tab: TabId) {
        self.apply(|b| b.reload(tab));
    }
    fn memory_warning(&self) {
        self.apply(|b| b.on_memory_warning());
    }
    fn did_enter_background(&self) {
        self.apply(|b| b.tick(now_ms()));
    }
}

/// Creates the content host on the main thread, attached to the chrome
/// window's root view.
///
/// # Safety
/// `root_view` must be the `UIView*` of the main window (iOS) and this must
/// run on the main thread.
pub unsafe fn install_host(root_view: *mut std::ffi::c_void, shared: Arc<Shared>) {
    let events: Rc<dyn ContentEvents> = Rc::new(Events { shared });
    let host = ContentHost::new(root_view, events);
    HOST.with(|h| *h.borrow_mut() = Some(host));
}
