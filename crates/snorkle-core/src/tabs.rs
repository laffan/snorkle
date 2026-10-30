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
}
