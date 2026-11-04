//! Tab model and memory budget.
//!
//! A tab is either `Live` (it owns a WKWebView and therefore a WebContent
//! process) or `Unloaded` (no web view; we keep the URL, the title, and on
//! iOS the platform keeps WKWebView `interactionState` so back/forward and
//! scroll position survive a discard). Hidden live tabs are additionally
//! throttled by WebKit (`inactiveSchedulingPolicy = .suspend`), so the
//! states the user can observe are:
//!
//!   Active (visible, live) → Background (hidden, live, WebKit-suspended)
//!                          → Discarded (unloaded, restored on activation)

use serde::Serialize;
use url::Url;

use crate::policy::Source;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct TabId(pub u64);

/// Which website data store a tab uses. Each maps to a distinct
/// `WKWebsiteDataStore` on iOS (see `snorkle-content`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Persistent store `WKWebsiteDataStore(forIdentifier:)`.
    Default,
    /// Non-persistent store shared by all private tabs of the current
    /// private session; released when the last private tab closes.
    Private,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Life {
    Live,
    Unloaded,
}

/// Memory budget for live web views.
///
/// iOS gives no direct per-process memory signal for WebContent processes,
/// so the budget is a count of live web views plus a background TTL, plus
/// reactions to memory warnings and WebContent process termination.
/// The numbers are starting targets to be tuned on hardware (see
/// docs/PERFORMANCE.md); they have NOT been validated on a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct MemoryBudget {
    /// Maximum number of live web views including the active one.
    pub max_live: usize,
    /// Background live tabs not activated for this long are discarded.
    pub background_ttl_ms: u64,
}

impl MemoryBudget {
    pub const DEFAULT_TTL_MS: u64 = 10 * 60 * 1000;

    /// Picks a budget from physical RAM (`NSProcessInfo.physicalMemory`).
    pub fn for_physical_memory(bytes: u64) -> Self {
        const GIB: u64 = 1024 * 1024 * 1024;
        let max_live = match bytes {
            b if b < 4 * GIB => 2,
            b if b < 6 * GIB => 3,
            b if b < 8 * GIB => 4,
            b if b < 16 * GIB => 6,
            _ => 8,
        };
        MemoryBudget { max_live, background_ttl_ms: Self::DEFAULT_TTL_MS }
    }
}

/// HTTPS-first bookkeeping for the current main-frame navigation chain.
#[derive(Clone, Debug, Default)]
pub struct HttpsUpgrade {
    /// The original `http:` URL the user/page asked for.
    pub http_url: Option<Url>,
    /// Number of upgrades performed in this chain without a commit.
    pub upgrades: u8,
}

impl HttpsUpgrade {
    /// More upgrades than this without a commit is treated as an
    /// https→http redirect loop, i.e. "HTTPS not available".
    pub const MAX_UPGRADES: u8 = 3;
}

/// Something the chrome must show instead of (or on top of) page content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Notice {
    /// HTTPS-first: the secure version failed. The user may explicitly
    /// continue over HTTP; nothing happens silently.
    HttpsUnavailable {
        host: String,
        http_url: String,
    },
    Blocked {
        reason: String,
        url: String,
    },
    /// The address is on the user's block list (Settings → Blocked):
    /// `message` is theirs, shown in the middle of the tab.
    UserBlocked {
        message: String,
        url: String,
    },
    LoadError {
        url: String,
        message: String,
        certificate: bool,
    },
    /// A page asked to open another app; the user must confirm.
    ExternalApp {
        url: String,
        scheme: String,
    },
    /// The WebContent process crashed repeatedly.
    Crashed,
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: TabId,
    pub profile: Profile,
    pub life: Life,
    /// Last URL WebKit *committed* in the main frame. This is the only URL the
    /// address bar shows (provisional URLs are never displayed).
    pub committed_url: Option<Url>,
    /// True between didStartProvisionalNavigation and commit/fail.
    pub provisional: bool,
    /// URL we asked WebKit to load and expect to see in the policy callback,
    /// so that our own loads are classified as `Source::Typed`.
    pub expected_load: Option<(Url, Source)>,
    pub title: String,
    pub loading: bool,
    pub progress: u8,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// WebKit `hasOnlySecureContent`.
    pub secure_content: bool,
    pub last_active_ms: u64,
    pub https: HttpsUpgrade,
    pub notice: Option<Notice>,
    /// Whether the current commit has been written to history.
    pub history_recorded: bool,
    pub crash_times_ms: Vec<u64>,
    /// Tab whose page opened this one as a popup (`window.open`), keeping a
    /// `window.opener` relationship (OAuth / payment popups).
    pub opener: Option<TabId>,
    /// A popup window (`window.open` with a size) shown as a sheet over its
    /// opener instead of as a tab. Never saved in the session.
    pub sheet: bool,
    /// The pin or bookmark (by its stored URL) this tab belongs to. Such a
    /// tab is not listed with the other tabs; the pin or bookmark is shown
    /// as selected instead, and opening it again returns to this tab.
    pub launcher: Option<String>,
    /// View modes turned off for this tab ("Disable for this tab"); saved
    /// with the session.
    pub view_exempt: Vec<crate::viewmodes::Kind>,
    /// Page scale in percent (one of [`ZOOM_LEVELS`]); saved with the
    /// session. Below 100 the page gets a wider viewport (it is laid out
    /// for a larger window and drawn smaller), above 100 a narrower one.
    pub zoom: u16,
    /// The page shown (or about to be) is the saved offline copy of a pin
    /// or bookmark, loaded because the network failed.
    pub offline: Option<OfflineCopy>,
    /// An offline copy was tried since the last commit (it is not tried
    /// again if its own load fails).
    pub offline_tried: bool,
    /// While the load started by opening its pin or bookmark runs: that
    /// item's URL (its offline copy is saved under it, also when the site
    /// redirects). Cleared when that load finishes or fails.
    pub landing: Option<String>,
}

/// An offline copy in a tab: when it was saved, and whether its load has
/// committed (until then the next provisional start is its own).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfflineCopy {
    pub saved_ms: u64,
    pub committed: bool,
}

/// Page scale levels, in percent (the steps of the ⋯ menu's − and +).
pub const ZOOM_LEVELS: [u16; 9] = [50, 67, 75, 80, 90, 100, 110, 125, 150];
pub const DEFAULT_ZOOM: u16 = 100;

/// The zoom level nearest to `percent` (for values read from disk).
pub fn clean_zoom(percent: u16) -> u16 {
    ZOOM_LEVELS.iter().copied().min_by_key(|z| z.abs_diff(percent)).unwrap_or(DEFAULT_ZOOM)
}

/// A change of page scale from the ⋯ menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoomStep {
    In,
    Out,
    Reset,
}

impl ZoomStep {
    /// The level after `from`; stays at the ends.
    pub fn apply(self, from: u16) -> u16 {
        let from = clean_zoom(from);
        let i = ZOOM_LEVELS.iter().position(|z| *z == from).unwrap_or(0);
        match self {
            ZoomStep::In => ZOOM_LEVELS[(i + 1).min(ZOOM_LEVELS.len() - 1)],
            ZoomStep::Out => ZOOM_LEVELS[i.saturating_sub(1)],
            ZoomStep::Reset => DEFAULT_ZOOM,
        }
    }
}

impl Tab {
    pub fn new(id: TabId, profile: Profile, now_ms: u64) -> Self {
        Tab {
            id,
            profile,
            life: Life::Unloaded,
            committed_url: None,
            provisional: false,
            expected_load: None,
            title: String::new(),
            loading: false,
            progress: 0,
            can_go_back: false,
            can_go_forward: false,
            secure_content: false,
            last_active_ms: now_ms,
            https: HttpsUpgrade::default(),
            notice: None,
            history_recorded: false,
            crash_times_ms: Vec::new(),
            opener: None,
            sheet: false,
            view_exempt: Vec::new(),
            zoom: DEFAULT_ZOOM,
            offline: None,
            offline_tried: false,
            landing: None,
            launcher: None,
        }
    }

    pub fn is_private(&self) -> bool {
        self.profile == Profile::Private
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_scales_with_ram() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(MemoryBudget::for_physical_memory(3 * GIB).max_live, 2);
        assert_eq!(MemoryBudget::for_physical_memory(8 * GIB).max_live, 6);
        assert_eq!(MemoryBudget::for_physical_memory(16 * GIB).max_live, 8);
    }

    #[test]
    fn zoom_steps() {
        assert_eq!(ZoomStep::In.apply(100), 110);
        assert_eq!(ZoomStep::Out.apply(100), 90);
        assert_eq!(ZoomStep::Out.apply(50), 50, "stays at the smallest");
        assert_eq!(ZoomStep::In.apply(150), 150, "stays at the largest");
        assert_eq!(ZoomStep::Reset.apply(75), 100);
        assert_eq!(clean_zoom(0), 50);
        assert_eq!(clean_zoom(9999), 150);
        assert_eq!(clean_zoom(118), 125);
        assert_eq!(ZoomStep::Out.apply(118), 110, "an off-level value snaps first");
    }
}
