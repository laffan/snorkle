//! SECURITY-REVIEW: the browser state machine.
//!
//! `Browser` owns tabs, navigation policy decisions, HTTPS-first, the
//! address-bar URL, suspension/discard and what is persisted. Platform code
//! feeds WebKit events in and executes the returned [`Effect`]s; it holds no
//! browser logic of its own.
//!
//! Security assumptions / invariants (each has a test below or in
//! `tests/`):
//! - The address bar shows only the last *committed* main-frame URL. URL
//!   changes reported outside a commit are accepted only if same-origin
//!   (pushState / fragment), so a page cannot make the bar show a different
//!   origin than the one rendering.
//! - Nothing about a private tab is ever emitted in `RecordHistory` or
//!   `PersistSession`.
//! - `http:` main-frame loads are upgraded; if HTTPS fails the user sees an
//!   interstitial and must explicitly choose to continue. No silent
//!   downgrade.
//! - Interstitials, error pages and prompts are rendered by the trusted
//!   chrome with the content view hidden, never inside page content.
//! - On an unsupported OS no web view is ever created.

use std::collections::BTreeSet;

use serde::Serialize;
use url::Url;

use crate::blocklist::BlockList;
use crate::input::{self, InputError, Interpretation, SearchEngine};
use crate::os::OsSupport;
use crate::policy::{self, BlockReason, Decision, Frame, HttpExceptions, Request, Source};
use crate::storage::{Session, SessionTab};
use crate::tabs::{HttpsUpgrade, Life, MemoryBudget, Notice, Profile, Tab, TabId};
use crate::text::{sanitize_page_text, MAX_PAGE_TEXT_CHARS};
use crate::url_display::{self, DisplayUrl, Transport};

mod tabs;
mod items;
mod navigation;
mod events;
mod visibility;
mod spaces;
mod groups;
mod pins;
mod preview;

pub use groups::{clean_group_name, TabGroup, MAX_GROUP_NAME_CHARS};
pub use pins::{GESTURE_WINDOW_MS, MAX_DIVERTS};
pub use spaces::{clean_research_name, valid_research_id, Research, ResearchView, MAX_RESEARCH};

/// Work for the platform layer. Executed in order, on the main thread,
/// without holding the `Browser` lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    CreateView {
        tab: TabId,
        profile: Profile,
        then: AfterCreate,
    },
    /// Take ownership of the popup web view WebKit asked the platform to
    /// create for this tab (`createWebViewWithConfiguration`); it shares the
    /// opener's configuration and keeps `window.opener`.
    AdoptPopup {
        tab: TabId,
    },
    /// Destroy the web view. With `keep_state`, the platform first saves
    /// `interactionState` so a later `Restore` brings back history/scroll.
    DestroyView {
        tab: TabId,
        keep_state: bool,
    },
    /// Show the tab's web view in the given part of the content area.
    ShowView {
        tab: TabId,
        pane: Pane,
    },
    HideView(TabId),
    /// The share of the content area the first pane of the pair on screen
    /// takes, in permille (the divider was dragged, or another pair came
    /// on screen). Sent before the `ShowView`s that place its panes.
    SetSplitRatio(u16),
    /// Set the web view's page scale (`percent` / 100: the page is laid out
    /// for its pane divided by that and drawn scaled by it). Also follows
    /// `CreateView` for a tab whose scale is not 100 %.
    SetZoom {
        tab: TabId,
        percent: u16,
    },
    Load {
        tab: TabId,
        url: Url,
    },
    GoBack(TabId),
    GoForward(TabId),
    Reload(TabId),
    Stop(TabId),
    /// The last private tab closed: drop the non-persistent data store.
    ReleasePrivateData,
    /// The user confirmed opening another app for this URL.
    OpenExternal(Url),
    RecordHistory {
        url: Url,
        title: String,
    },
    /// A tab showing a Save for Later archive followed a link to another
    /// page the archive holds (or reloads): the platform loads that page
    /// from the archive and calls [`Browser::show_saved_copy`].
    OpenSavedPage {
        tab: TabId,
        item: String,
        url: Url,
    },
    PersistSession(Session),
    /// Chrome state changed; re-render from [`Browser::view`].
    NotifyChrome,
    /// Only a tab's load progress changed (2026-11-23): the chrome is sent
    /// that number, not the whole state, about twenty times a page load.
    NotifyProgress(TabId),
    /// Turns the pin guard on or off in the tab's page (2026-11-24): while
    /// on, the page's watcher sends a real click on a link to another page
    /// to the core (`Browser::on_pin_link`) instead of following it, and
    /// reports other real clicks (`Browser::on_pin_gesture`). Sent after
    /// each commit and finished load of a pin's tab, and when a tab stops
    /// being one.
    PinGuard {
        tab: TabId,
        on: bool,
    },
}

/// Where a visible content view is placed within the content area.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pane {
    Full,
    Left,
    Right,
    /// The halves of a pair split in rows (one above the other).
    Top,
    Bottom,
    /// A popup window centred over its (dimmed, still running) opener.
    Sheet,
    /// The History preview (2026-11-24): in the rectangle the chrome gives
    /// for its dialog's body.
    Preview,
}

/// How a page-opened window is presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupStyle {
    /// target=_blank, or `window.open` without a size: a new tab.
    Tab,
    /// `window.open` asking for a window size (sign-in, payment): a sheet
    /// over the opener, which stays visible so its scripts keep running.
    Sheet,
}

/// Two tabs shown together. A tab belongs to at most one pair; the pair
/// holding the active tab is the one on screen. `left` is the first pane
/// (left, or top in rows), `right` the second (right, or bottom).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SplitView {
    pub left: TabId,
    pub right: TabId,
    pub orientation: SplitOrientation,
    /// The first pane's share of the space, in permille
    /// ([`SPLIT_RATIO_MIN`]..=[`SPLIT_RATIO_MAX`]); the divider sets it.
    pub ratio: u16,
}

/// Default and limits of [`SplitView::ratio`].
pub const SPLIT_RATIO_DEFAULT: u16 = 500;
pub const SPLIT_RATIO_MIN: u16 = 200;
pub const SPLIT_RATIO_MAX: u16 = 800;

/// How a pair is laid out: side by side (a vertical divider; ⌘D and
/// dragging a tab onto another) or one above the other (⌘⇧S).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitOrientation {
    Columns,
    Rows,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AfterCreate {
    Load(Url),
    /// Restore saved interaction state if the platform has it, otherwise
    /// load `fallback`.
    Restore {
        fallback: Option<Url>,
    },
}

/// `WKNavigationType`, as reported by WebKit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKind {
    LinkActivated,
    FormSubmitted,
    BackForward,
    Reload,
    FormResubmitted,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavErrorKind {
    /// NSURLErrorCancelled / frame load interrupted by policy change.
    Cancelled,
    /// Any TLS/certificate failure. Never bypassable.
    Certificate,
    Network,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Security {
    /// https and WebKit reports only secure content.
    Secure,
    /// https but some content was loaded insecurely.
    Mixed,
    NotSecure,
    None,
}

#[derive(Clone, Debug, Serialize)]
pub struct TabView {
    pub id: TabId,
    pub private: bool,
    pub title: String,
    pub url: Option<DisplayUrl>,
    pub loading: bool,
    pub progress: u8,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub security: Security,
    pub live: bool,
    pub notice: Option<Notice>,
    /// Ad-blocking state for the committed site, if it is an http(s) page.
    pub adblock: Option<SiteAdblock>,
    /// Set for a popup shown as a sheet: the tab it covers. Sheets are not
    /// listed as tabs.
    pub sheet_over: Option<TabId>,
    /// The pin or bookmark (URL) this tab belongs to; not listed as a tab.
    pub launcher: Option<String>,
    /// Page zoom in percent.
    pub zoom: u16,
    pub url_bar_attached: bool,
    /// Set while the tab shows a saved offline copy: when it was saved
    /// (Unix ms).
    pub offline_saved_ms: Option<u64>,
    /// The user's own name for the tab (shown instead of `title`).
    pub name: Option<String>,
    /// An emoji shown instead of the favicon.
    pub emoji: Option<String>,
    /// Set while the tab shows a Save for Later archive.
    pub saved: Option<SavedView>,
    /// The research session the tab belongs to (`None`: everyday).
    pub space: Option<String>,
    /// The tab group it is in.
    pub group: Option<u64>,
}

/// A Save for Later archive in a tab, as the chrome shows it.
#[derive(Clone, Debug, Serialize)]
pub struct SavedView {
    pub item: String,
    pub saved_ms: u64,
    /// Pages the archive holds.
    pub pages: usize,
    /// Adding pages: the chosen links (path and query, cleaned), or `None`
    /// when not choosing.
    pub picking: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SiteAdblock {
    /// Registrable domain the per-site toggle applies to.
    pub site: String,
    /// False when the user turned blocking off for this site.
    pub blocking: bool,
    /// Private tabs cannot change the (persistent) per-site setting.
    pub can_toggle: bool,
}

/// State of the content-blocking rule lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AdblockStatus {
    Off,
    Compiling,
    Active { lists: usize },
    Failed { message: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct ChromeState {
    pub tabs: Vec<TabView>,
    pub active: Option<TabId>,
    pub os: OsSupport,
    pub content_visible: bool,
    /// The pair on screen (the one holding the active tab), if any.
    pub split: Option<SplitView>,
    /// Every pair, shown or not.
    pub splits: Vec<SplitView>,
    /// The popup sheet in front (the active tab), if any.
    pub sheet: Option<TabId>,
    pub max_live_tabs: usize,
    pub adblock: AdblockStatus,
    /// The research session shown (the active tab's; `None`: everyday
    /// browsing, 2026-11-24).
    pub space: Option<String>,
    /// Every research session, oldest first.
    pub research: Vec<ResearchView>,
    /// Every tab group (of every session; tabs name theirs).
    pub groups: Vec<TabGroup>,
    /// The History preview's tab, while one is open (2026-11-24; it is not
    /// in `tabs`).
    pub preview: Option<TabId>,
}

const CRASH_WINDOW_MS: u64 = 60_000;
const MAX_CRASH_RELOADS: usize = 2;

pub struct Browser {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    next_id: u64,
    budget: MemoryBudget,
    os: OsSupport,
    http_ok_default: HttpExceptions,
    http_ok_private: HttpExceptions,
    search: SearchEngine,
    chrome_overlay: bool,
    shown: Vec<(TabId, Pane)>,
    splits: Vec<SplitView>,
    /// The ratio last sent to the host (`Effect::SetSplitRatio`).
    shown_ratio: u16,
    adblock_allowed: BTreeSet<String>,
    adblock_status: AdblockStatus,
    /// Pages of recently closed non-private tabs, oldest first (⌘⇧T), in
    /// memory only. Private tabs and popup sheets are never listed.
    closed: Vec<Url>,
    /// The user's block list (Settings → Blocked).
    blocklist: BlockList,
    /// The pins' stored URLs (2026-11-24): their tabs keep their page and
    /// send the pages the user goes to into new tabs (`pins.rs`).
    pins: BTreeSet<String>,
    /// Tab groups (`groups.rs`), and the next group id.
    groups: Vec<TabGroup>,
    next_group: u64,
    /// Research sessions (`spaces.rs`), oldest first.
    research: Vec<Research>,
}

/// Most closed tabs remembered for ⌘⇧T.
pub const MAX_CLOSED_TABS: usize = 50;


fn classify(kind: NavKind) -> Source {
    match kind {
        NavKind::LinkActivated => Source::Page { user_gesture: true },
        NavKind::FormSubmitted | NavKind::FormResubmitted => Source::Page { user_gesture: false },
        NavKind::BackForward | NavKind::Reload => Source::History,
        // `.other` covers redirects and script navigations alike; treat it as
        // the most restrictive page-initiated source.
        NavKind::Other => Source::Page { user_gesture: false },
    }
}

fn external_notice(u: &Url) -> Notice {
    Notice::ExternalApp { url: u.to_string(), scheme: u.scheme().to_string() }
}

fn blocked_notice(r: BlockReason, u: &Url) -> Notice {
    let reason = match r {
        BlockReason::InternalScheme => "This address is reserved by the browser.",
        BlockReason::File => "Local files cannot be opened.",
        BlockReason::TopLevelData => "data: URLs cannot be opened as pages.",
        BlockReason::TypedJavascript => "javascript: URLs cannot be run from the address bar.",
        BlockReason::ForeignBlob => "This blob: URL was not created by the current site.",
        BlockReason::Credentials => "Links containing a username or password are blocked.",
        BlockReason::ExternalWithoutGesture => "The page tried to open another app without your action.",
        BlockReason::UnsupportedAbout => "This about: page is not supported.",
    };
    Notice::Blocked { reason: reason.to_string(), url: shown_url(u) }
}

/// Show only the scheme and display host, never the raw (possibly huge
/// or deceptive) URL.
fn shown_url(u: &Url) -> String {
    let d = url_display::display(u);
    if d.registrable.is_empty() {
        format!("{}:", u.scheme())
    } else {
        d.full.chars().take(200).collect()
    }
}

fn tab_view(t: &Tab, adblock_allowed: &BTreeSet<String>) -> TabView {
    let display = t.committed_url.as_ref().map(url_display::display);
    let security = match display.as_ref().map(|d| d.transport) {
        Some(Transport::Https) if t.secure_content => Security::Secure,
        Some(Transport::Https) => Security::Mixed,
        Some(Transport::Http) => Security::NotSecure,
        _ => Security::None,
    };
    TabView {
        id: t.id,
        private: t.is_private(),
        title: t.title.clone(),
        url: display,
        loading: t.loading,
        progress: t.progress,
        can_go_back: t.can_go_back || t.notice.is_some() || t.back_to.is_some(),
        can_go_forward: t.can_go_forward,
        security,
        live: t.life == Life::Live,
        notice: t.notice.clone(),
        adblock: t.committed_url.as_ref().and_then(crate::blocking::site_key).map(|site| SiteAdblock {
            blocking: !adblock_allowed.contains(&site),
            site,
            can_toggle: !t.is_private(),
        }),
        sheet_over: t.opener.filter(|_| t.sheet),
        launcher: t.launcher.clone(),
        zoom: t.zoom,
        url_bar_attached: t.url_bar_attached,
        offline_saved_ms: t.offline.filter(|o| o.committed).map(|o| o.saved_ms),
        name: t.name.clone(),
        emoji: t.emoji.clone(),
        saved: t.saved.as_ref().map(|c| SavedView {
            item: c.item.clone(),
            saved_ms: c.saved_ms,
            pages: c.pages.len(),
            picking: c.picking.as_ref().map(|p| p.iter().map(link_label).collect()),
        }),
        space: t.space.clone(),
        group: t.group,
    }
}

/// A blank tab (the start page): no page committed or on its way, no
/// notice, and not a pin's, bookmark's, popup or saved copy's tab.
pub(super) fn is_blank(t: &Tab) -> bool {
    t.committed_url.is_none()
        && !t.loading
        && !t.provisional
        && t.notice.is_none()
        && t.expected_load.is_none()
        && t.saved.is_none()
        && !t.sheet
        && t.launcher.is_none()
        && t.opener.is_none()
        && t.life == Life::Unloaded
}

/// How a chosen link is listed while adding pages: its path and query.
fn link_label(u: &Url) -> String {
    let mut s = u.path().to_string();
    if let Some(q) = u.query() {
        s.push('?');
        s.push_str(q);
    }
    sanitize_page_text(&s, 120)
}

/// `u` without its fragment (archive pages are kept without one).
pub fn without_fragment(u: &Url) -> Url {
    let mut u = u.clone();
    u.set_fragment(None);
    u
}

/// A tab name from the Edit dialog or the session: cleaned like a page
/// title; empty means none.
fn clean_tab_name(name: &str) -> Option<String> {
    let n = sanitize_page_text(name, crate::tabs::MAX_TAB_NAME_CHARS);
    (!n.trim().is_empty()).then(|| n.trim().to_string())
}

/// Exemptions as stored: sorted and unique.
fn clean_exempt(kinds: &[crate::viewmodes::Kind]) -> Vec<crate::viewmodes::Kind> {
    let mut v: Vec<_> = kinds.to_vec();
    v.sort();
    v.dedup();
    v
}
