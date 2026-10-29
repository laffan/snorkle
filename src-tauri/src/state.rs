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
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use snorkle_content::{ContentEvents, ContentHost};
use snorkle_core::browser::{Browser, ChromeState, Effect, NavErrorKind, NavKind, Verdict};
use snorkle_core::policy::Frame;
use snorkle_core::storage::{Session, Store};
use snorkle_core::tabs::TabId;
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
}

pub type Emitter = Box<dyn Fn(ChromeEvent<'_>) + Send + Sync>;

pub struct Shared {
    browser: Mutex<Browser>,
    pub store: Option<Arc<Store>>,
    storage_tx: Option<Mutex<Sender<StorageJob>>>,
    emit: Emitter,
}

impl Shared {
    pub fn new(browser: Browser, store: Option<Store>, emit: Emitter) -> Arc<Self> {
        let store = store.map(Arc::new);
        let storage_tx = store.clone().map(|s| Mutex::new(spawn_storage_worker(s)));
        Arc::new(Shared { browser: Mutex::new(browser), store, storage_tx, emit })
    }

    pub fn browser(&self) -> MutexGuard<'_, Browser> {
        self.browser.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn view(&self) -> ChromeState {
        self.browser().view()
    }

    pub fn toast(&self, msg: &str) {
        (self.emit)(ChromeEvent::Toast(msg));
    }

    /// Routes effects. Must be called WITHOUT the browser lock held.
    pub fn run(self: &Arc<Self>, effects: Vec<Effect>) {
        debug_log_effects(&effects);
        let mut host_effects = Vec::new();
        let mut notify = false;
        for e in effects {
            match e {
                Effect::NotifyChrome => notify = true,
                Effect::RecordHistory { url, title } => {
                    self.storage(StorageJob::History { url: url.to_string(), title })
                }
                Effect::PersistSession(s) => self.storage(StorageJob::Session(s)),
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

/// Debug builds only: prints what the core decided, so navigation can be
/// followed in the terminal running `tauri dev` / `tauri ios dev`. URLs of
/// private tabs are printed too; never enable this in release builds.
fn debug_log_effects(effects: &[Effect]) {
    if cfg!(debug_assertions) {
        for e in effects {
            match e {
                Effect::NotifyChrome | Effect::PersistSession(_) => {}
                other => eprintln!("[snorkle] effect {other:?}"),
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
        if cfg!(debug_assertions) {
            eprintln!("[snorkle] policy tab={} {frame:?} {kind:?} {url} -> {verdict:?}", tab.0);
        }
        self.shared.run(effects);
        verdict
    }
    fn new_window_request(&self, opener: TabId, url: Url) {
        self.apply(|b| b.on_new_window_request(opener, &url, now_ms()));
    }
    fn provisional_start(&self, tab: TabId) {
        self.apply(|b| b.on_provisional_start(tab));
    }
    fn commit(&self, tab: TabId, url: Url) {
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
    fn focus_requested(&self, tab: TabId) {
        // Only meaningful in split view; otherwise the tab is already active.
        if self.shared.browser().active() != Some(tab) {
            self.apply(|b| b.activate(tab, now_ms()));
        }
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
