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
use snorkle_core::viewmodes::{self, ViewModes};
use snorkle_core::Url;

mod events;
mod host;
mod shared;
pub mod timing;
mod worker;

pub use events::install_host;
pub use host::*;
use worker::*;

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
    /// A favicon was stored: the chrome asks for tab icons again.
    IconsChanged,
    /// A new console entry.
    Console(&'a console::Entry),
    /// Where a tab's text selection is (`None`: nothing selected).
    Selection(TabId, Option<snorkle_core::selection::SelectionRect>),
    /// The image menu for an image in a tab's page (its URL, and where).
    /// The pointer near the window's left edge (`None`: away).
    EdgePointer(Option<(f64, f64)>),
    /// ⌥ pressed or let go (macOS, `ContentEvents::option_key`).
    OptionKey(bool),
    /// `None`: close it (the page was touched or scrolled).
    ImageMenu(TabId, Option<(&'a str, snorkle_core::selection::SelectionRect)>),
    /// Console entries were removed (cleared, or the private session ended).
    ConsoleReset,
    /// A tab's page gained (`true`) or lost a focused text field (C19).
    PageEditing(TabId, bool),
    /// A notebook capture asked for in a tab's page (C23): ⌥-click on text
    /// or an image, ⌥-drag or a clip-mode drag, or clip mode left there.
    NotebookPick(TabId, snorkle_core::notebook::Report),
    /// Save for Later: how far saving a tab ("save") or adding pages to a
    /// saved one ("add") is, in percent.
    LaterProgress(TabId, &'static str, u8),
    /// The downloads list changed (started, progress, finished, removed).
    Downloads(&'a [DownloadItem]),
    /// A frame inside a tab's page wants to download a file: the user
    /// allows or blocks it (`answer_download_prompt`). `from` is the
    /// download's host (or "" for a file the page made).
    DownloadPrompt { ask: u64, tab: TabId, from: String },
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
    /// Effective keyboard shortcut bindings (one per `SHORTCUTS` entry).
    pub shortcuts: Mutex<Vec<snorkle_content::KeyBinding>>,
    /// Makes archived chat ids unique within one millisecond.
    pub chat_seq: std::sync::atomic::AtomicU64,
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
    /// Downloads stopped for going over the storage cap.
    pub capped_downloads: Mutex<HashSet<u64>>,
    /// When each tab last started downloads (rate limit).
    pub download_starts: Mutex<HashMap<TabId, Vec<Instant>>>,
}
