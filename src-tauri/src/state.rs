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
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
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
    History {
        url: String,
        title: String,
    },
    Session(Session),
    /// Write a pending session now (the app is going to the background,
    /// where iOS may suspend or end it).
    Flush,
    /// Write a pending session now and report when done (the app is
    /// quitting; macOS).
    FlushNow(mpsc::Sender<()>),
    /// The text of a visited page (history's cached version).
    PageText {
        url: String,
        title: String,
        text: String,
    },
    /// An offline copy (web archive) of a pinned or bookmarked page.
    OfflineCopy {
        url: String,
        data: Vec<u8>,
    },
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
    /// Where a tab's text selection is (`None`: nothing selected).
    Selection(TabId, Option<snorkle_core::selection::SelectionRect>),
    /// The image menu for an image in a tab's page (its URL, and where).
    /// The pointer near the window's left edge (`None`: away).
    EdgePointer(Option<(f64, f64)>),
    /// `None`: close it (the page was touched or scrolled).
    ImageMenu(TabId, Option<(&'a str, snorkle_core::selection::SelectionRect)>),
    /// Console entries were removed (cleared, or the private session ended).
    ConsoleReset,
    /// The downloads list changed (started, progress, finished, removed).
    Downloads(&'a [DownloadItem]),
}

/// One download, as the chrome lists it (Downloads panel).
#[derive(Clone, Debug, serde::Serialize)]
pub struct DownloadItem {
    pub id: u64,
    /// File name in the staging folder (cleaned; shown with textContent).
    pub name: String,
    /// Where it came from (http(s) only; `None`: a file the page made).
    pub source: Option<snorkle_core::url_display::DisplayUrl>,
    #[serde(skip)]
    pub source_url: String,
    pub mime: String,
    pub received: u64,
    pub expected: Option<u64>,
    /// "active", "done", "failed" or "cancelled".
    pub state: &'static str,
    pub error: String,
    pub private: bool,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
}

/// Most downloads running at once; more are refused.
const MAX_ACTIVE_DOWNLOADS: usize = 20;
/// Downloads one tab may start per minute (pages can start downloads
/// without a click, e.g. through redirects); more are refused.
const MAX_DOWNLOADS_PER_TAB_MINUTE: usize = 5;

impl DownloadItem {
    fn from_record(r: &snorkle_core::storage::DownloadRecord) -> Self {
        use snorkle_core::storage::DownloadOutcome;
        DownloadItem {
            id: r.id,
            name: r.name.clone(),
            source: Url::parse(&r.source).ok().map(|u| snorkle_core::url_display::display(&u)),
            source_url: r.source.clone(),
            mime: r.mime.clone(),
            received: r.bytes,
            expected: Some(r.bytes),
            state: match r.outcome {
                DownloadOutcome::Done => "done",
                DownloadOutcome::Failed => "failed",
                DownloadOutcome::Cancelled => "cancelled",
            },
            error: r.error.clone(),
            private: false,
            started_ms: r.started_ms,
            finished_ms: Some(r.finished_ms),
        }
    }
}

pub type Emitter = Box<dyn Fn(ChromeEvent<'_>) + Send + Sync>;

pub struct Shared {
    browser: Mutex<Browser>,
    pub store: Option<Arc<Store>>,
    storage_tx: Option<Mutex<Sender<StorageJob>>>,
    pub(crate) emit: Emitter,
    /// App-wide view modes. Each launch starts from the stored launch
    /// modes (Settings → General); dark is stored when toggled, the others
    /// change for the session only.
    pub view_modes: Mutex<ViewModes>,
    /// Tabs exempted from view modes ("Disable for this tab"). Mirrors the
    /// core's `Tab::view_exempt`, which is saved with the session.
    pub view_exempt: Mutex<HashMap<TabId, BTreeSet<viewmodes::Kind>>>,
    /// Latest outline per tab (headers view).
    pub headings: Mutex<HashMap<TabId, Vec<Heading>>>,
    /// Tabs with the headers view on (it is a per-tab mode).
    pub headers_tabs: Mutex<HashSet<TabId>>,
    /// Effective keyboard shortcut bindings (one per `SHORTCUTS` entry).
    pub shortcuts: Mutex<Vec<snorkle_content::KeyBinding>>,
    /// Makes archived chat ids unique within one millisecond.
    pub chat_seq: std::sync::atomic::AtomicU64,
    /// "Detect Headers" requests waiting for their page's sample.
    pub pending_samples: Mutex<HashMap<TabId, mpsc::Sender<String>>>,
    /// In-app console (in memory only).
    pub console: Mutex<ConsoleLog>,
    /// Whether the console records (and pages get the console hook). On by
    /// default in debug builds only.
    pub console_capture: AtomicBool,
    /// Downloads, newest first: running ones, this session's private ones
    /// and the stored finished ones.
    pub downloads: Mutex<Vec<DownloadItem>>,
    /// Downloads the user cancelled (reported as failed by WebKit).
    pub cancelled_downloads: Mutex<HashSet<u64>>,
    /// When each tab last started downloads (rate limit).
    pub download_starts: Mutex<HashMap<TabId, Vec<Instant>>>,
}

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
            headings: Mutex::default(),
            headers_tabs: Mutex::default(),
            pending_samples: Mutex::default(),
            chat_seq: std::sync::atomic::AtomicU64::new(0),
            shortcuts: Mutex::new(snorkle_content::effective_bindings(&[]).unwrap_or_default()),
            console: Mutex::default(),
            console_capture: AtomicBool::new(cfg!(debug_assertions)),
            downloads: Mutex::new(downloads),
            cancelled_downloads: Mutex::default(),
            download_starts: Mutex::default(),
        })
    }

    pub fn downloads(&self) -> MutexGuard<'_, Vec<DownloadItem>> {
        self.downloads.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sends the downloads list to the chrome.
    pub fn emit_downloads(&self) {
        let list = self.downloads().clone();
        (self.emit)(ChromeEvent::Downloads(&list));
    }

    /// The private session ended: its downloads are cancelled and their
    /// files deleted (they were never listed in `downloads.json`).
    fn drop_private_downloads(&self) {
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

/// How long after a page finished loading its text is read for history
/// (pages often fill in after the load event).
const PAGE_TEXT_DELAY: Duration = Duration::from_millis(1500);

/// Longest a session snapshot waits before it is written.
const SESSION_DELAY: Duration = Duration::from_millis(400);

/// Storage worker: history appends are written as they come; session
/// snapshots are coalesced (only the newest within a short window is
/// written), since every commit produces one.
fn spawn_storage_worker(store: Arc<Store>) -> Sender<StorageJob> {
    let (tx, rx) = mpsc::channel::<StorageJob>();
    std::thread::Builder::new()
        .name("snorkle-storage".into())
        .spawn(move || {
            // Session snapshots are coalesced: the latest one is written at
            // most 400 ms after the first one that arrived since the last
            // write. A fixed deadline, so a steady stream of jobs (a busy
            // page's navigations and history) cannot postpone it forever.
            let mut pending: Option<(Session, Instant)> = None;
            loop {
                let job = match &pending {
                    Some((_, due)) => match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                        Ok(j) => Some(j),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    },
                    None => match rx.recv() {
                        Ok(j) => Some(j),
                        Err(_) => break,
                    },
                };
                match job {
                    Some(StorageJob::History { url, title }) => {
                        let _ = store.add_history(&url, &title, wall_ms());
                    }
                    Some(StorageJob::Session(s)) => {
                        let due = pending.take().map(|(_, d)| d).unwrap_or_else(|| Instant::now() + SESSION_DELAY);
                        pending = Some((s, due));
                    }
                    Some(StorageJob::OfflineCopy { url, data }) => {
                        let _ = store.save_offline_copy(&url, &data, wall_ms());
                    }
                    Some(StorageJob::PageText { url, title, text }) => {
                        let _ = store.save_page_text(&url, &title, &text, wall_ms());
                    }
                    Some(StorageJob::Flush) | None => {
                        if let Some((s, _)) = pending.take() {
                            let _ = store.save_session(&s);
                        }
                    }
                    Some(StorageJob::FlushNow(done)) => {
                        if let Some((s, _)) = pending.take() {
                            let _ = store.save_session(&s);
                        }
                        let _ = done.send(());
                    }
                }
                if pending.as_ref().is_some_and(|(_, due)| *due <= Instant::now()) {
                    if let Some((s, _)) = pending.take() {
                        let _ = store.save_session(&s);
                    }
                }
            }
            if let Some((s, _)) = pending.take() {
                let _ = store.save_session(&s);
            }
        })
        .expect("spawn storage thread");
    tx
}

thread_local! {
    /// The content host lives on the main thread only (UIKit/AppKit objects).
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

impl Events {
    /// After a page of a pin or bookmark finished loading: saves its
    /// offline copy (under the pin's or bookmark's URL when the tab was
    /// opened from it, else under the page's URL if that is pinned or
    /// bookmarked), at most every `storage::OFFLINE_REFRESH_MS`.
    fn save_offline_copy(&self, tab: TabId, landing: Option<String>, page: Option<String>) {
        let Some(store) = self.shared.store.clone() else { return };
        let now = wall_ms();
        let Some(key) = landing.into_iter().chain(page).find(|k| store.offline_copy_wanted(k, now)) else { return };
        let shared = self.shared.clone();
        snorkle_content::dispatch_main(move || {
            with_host(|h| {
                h.save_offline(
                    tab,
                    Box::new(move |data| match data {
                        Some(data) => {
                            shared.log(
                                console::Level::Info,
                                Some(tab),
                                &format!("offline copy saved: {key} ({} KB)", data.len() / 1024),
                            );
                            shared.storage(StorageJob::OfflineCopy { url: key, data });
                        }
                        None => shared.log(console::Level::Info, Some(tab), &format!("offline copy not saved: {key}")),
                    }),
                )
            })
        });
    }

    /// After a non-private page finished loading: reads its text (the
    /// constant `ai::PAGE_TEXT_JS`, channel C17) once it has had
    /// `PAGE_TEXT_DELAY` to fill in, and saves it for history if the tab
    /// still shows that page and is still not private.
    fn save_page_text(&self, tab: TabId, page: String) {
        if !page.starts_with("http://") && !page.starts_with("https://") {
            return;
        }
        let shared = self.shared.clone();
        let _ = std::thread::Builder::new().name("snorkle-page-text".into()).spawn(move || {
            std::thread::sleep(PAGE_TEXT_DELAY);
            snorkle_content::dispatch_main(move || {
                with_host(|h| {
                    h.read_page_text(
                        tab,
                        Box::new(move |text| {
                            let still = {
                                let b = shared.browser();
                                b.tab(tab).is_some_and(|t| {
                                    !t.is_private() && t.committed_url.as_ref().map(|u| u.to_string()) == Some(page.clone())
                                }) && !b.shows_offline_copy(tab)
                            };
                            let parsed = text.and_then(|t| snorkle_core::ai::parse_page_text(&t, &page));
                            if let (true, Some(p)) = (still, parsed) {
                                shared.storage(StorageJob::PageText { url: page, title: p.title, text: p.text });
                            }
                        }),
                    )
                })
            });
        });
    }

    /// The network failed for `url`: if a pin's or bookmark's offline copy
    /// exists for it (or for the item the tab was opened from), shows the
    /// copy instead of the error.
    fn show_offline_copy(&self, tab: TabId, url: Url, landing: Option<String>) {
        let Some(store) = self.shared.store.clone() else { return };
        let Some((data, saved_ms)) =
            std::iter::once(url.to_string()).chain(landing).find_map(|k| store.offline_copy(&k))
        else {
            return;
        };
        let (ok, effects) = self.shared.browser().use_offline_copy(tab, &url, saved_ms);
        self.shared.run(effects);
        if ok {
            self.shared.log(console::Level::Info, Some(tab), &format!("offline: showing the saved copy of {url}"));
            snorkle_content::dispatch_main(move || with_host(|h| h.load_offline(tab, &url, data)));
        }
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
    fn selection(&self, tab: Option<TabId>, body: String) {
        let Some(tab) = tab else { return };
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
        let (landing, page, skip) = {
            let b = self.shared.browser();
            let t = b.tab(tab);
            let page = t.and_then(|t| t.committed_url.as_ref()).map(|u| u.to_string());
            let skip = t.is_none_or(|t| t.is_private() || t.notice.is_some()) || b.shows_offline_copy(tab);
            (b.landing(tab), page, skip)
        };
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
        self.apply(|b| b.on_fail(tab, provisional, kind, failing_url, &message));
        if offline {
            if let Some(url) = url {
                self.show_offline_copy(tab, url, landing);
            }
        }
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
    fn may_download(&self, tab: TabId, url: &Url) -> bool {
        let shared = &self.shared;
        if shared.store.is_none() || shared.browser().tab(tab).is_none() || !snorkle_core::policy::may_download(url) {
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
        shared.toast(&format!("Downloading “{name}”…"));
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
            self.shared.emit_downloads();
        }
    }
    fn download_finished(&self, id: u64, error: Option<String>) {
        use snorkle_core::storage::{DownloadOutcome, DownloadRecord};
        let shared = &self.shared;
        let cancelled = shared.cancelled_downloads.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        let item = {
            let mut d = shared.downloads();
            let Some(x) = d.iter_mut().find(|x| x.id == id) else { return };
            x.finished_ms = Some(wall_ms());
            x.state = match (&error, cancelled) {
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
        match item.state {
            "done" => shared.toast(&format!("Downloaded “{}”", item.name)),
            "failed" => shared.toast(&format!("Download failed: {}", item.name)),
            _ => {}
        }
        shared.emit_downloads();
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
