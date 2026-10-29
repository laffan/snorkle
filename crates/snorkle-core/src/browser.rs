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

use serde::Serialize;
use url::Url;

use crate::input::{self, InputError, Interpretation, SearchEngine};
use crate::os::OsSupport;
use crate::policy::{self, BlockReason, Decision, Frame, HttpExceptions, Request, Source};
use crate::storage::{Session, SessionTab};
use crate::tabs::{HttpsUpgrade, Life, MemoryBudget, Notice, Profile, Tab, TabId};
use crate::text::{sanitize_page_text, MAX_PAGE_TEXT_CHARS};
use crate::url_display::{self, DisplayUrl, Transport};

/// Work for the platform layer. Executed in order, on the main thread,
/// without holding the `Browser` lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    CreateView {
        tab: TabId,
        profile: Profile,
        then: AfterCreate,
    },
    /// Destroy the web view. With `keep_state`, the platform first saves
    /// `interactionState` so a later `Restore` brings back history/scroll.
    DestroyView {
        tab: TabId,
        keep_state: bool,
    },
    ShowView(TabId),
    HideView(TabId),
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
    PersistSession(Session),
    /// Chrome state changed; re-render from [`Browser::view`].
    NotifyChrome,
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
}

#[derive(Clone, Debug, Serialize)]
pub struct ChromeState {
    pub tabs: Vec<TabView>,
    pub active: Option<TabId>,
    pub os: OsSupport,
    pub content_visible: bool,
    pub max_live_tabs: usize,
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
    shown: Option<TabId>,
}

impl Browser {
    pub fn new(budget: MemoryBudget, os: OsSupport) -> Self {
        Browser {
            tabs: Vec::new(),
            active: None,
            next_id: 1,
            budget,
            os,
            http_ok_default: HttpExceptions::default(),
            http_ok_private: HttpExceptions::default(),
            search: input::DEFAULT_SEARCH,
            chrome_overlay: false,
            shown: None,
        }
    }

    pub fn budget(&self) -> MemoryBudget {
        self.budget
    }

    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn active(&self) -> Option<TabId> {
        self.active
    }

    pub fn live_count(&self) -> usize {
        self.tabs.iter().filter(|t| t.life == Life::Live).count()
    }

    fn exceptions(&self, p: Profile) -> &HttpExceptions {
        match p {
            Profile::Default => &self.http_ok_default,
            Profile::Private => &self.http_ok_private,
        }
    }

    // ----- chrome-initiated actions -------------------------------------

    /// Restores non-private tabs from a saved session as unloaded tabs and
    /// activates the saved active tab (only that one gets a web view).
    pub fn restore_session(&mut self, session: &Session, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        let mut ids = Vec::new();
        for st in &session.tabs {
            let Ok(url) = Url::parse(&st.url) else { continue };
            if !matches!(url.scheme(), "http" | "https") {
                continue;
            }
            let id = self.alloc_id();
            let mut tab = Tab::new(id, Profile::Default, now);
            tab.committed_url = Some(url);
            tab.title = sanitize_page_text(&st.title, MAX_PAGE_TEXT_CHARS);
            self.tabs.push(tab);
            ids.push(id);
        }
        match session.active.and_then(|i| ids.get(i)).or(ids.last()).copied() {
            Some(id) => self.activate_into(id, now, &mut out),
            None => {
                self.new_tab_into(Profile::Default, now, &mut out);
            }
        }
        out.push(Effect::NotifyChrome);
        out
    }

    pub fn new_tab(&mut self, profile: Profile, now: u64) -> (TabId, Vec<Effect>) {
        let mut out = Vec::new();
        let id = self.new_tab_into(profile, now, &mut out);
        out.push(Effect::NotifyChrome);
        (id, out)
    }

    fn new_tab_into(&mut self, profile: Profile, now: u64, out: &mut Vec<Effect>) -> TabId {
        let id = self.alloc_id();
        let insert_at = self
            .active
            .and_then(|a| self.tabs.iter().position(|t| t.id == a))
            .map(|i| i + 1)
            .unwrap_or(self.tabs.len());
        self.tabs.insert(insert_at, Tab::new(id, profile, now));
        self.activate_into(id, now, out);
        id
    }

    fn alloc_id(&mut self) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn activate(&mut self, id: TabId, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        if self.tab(id).is_some() {
            self.activate_into(id, now, &mut out);
        }
        out.push(Effect::NotifyChrome);
        out
    }

    fn activate_into(&mut self, id: TabId, now: u64, out: &mut Vec<Effect>) {
        if let Some(prev) = self.active {
            if let Some(t) = self.tab_mut(prev) {
                t.last_active_ms = now;
            }
        }
        self.active = Some(id);
        let restore = match self.tab_mut(id) {
            Some(t) => {
                t.last_active_ms = now;
                (t.life == Life::Unloaded).then(|| t.committed_url.clone()).flatten()
            }
            None => return,
        };
        if let Some(url) = restore {
            self.ensure_live(
                id,
                AfterCreate::Restore { fallback: Some(url.clone()) },
                Some((url, Source::History)),
                out,
            );
        }
        self.enforce_budget(now, out);
        self.sync_visibility(out);
    }

    pub fn close(&mut self, id: TabId, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        let Some(idx) = self.tabs.iter().position(|t| t.id == id) else { return out };
        let tab = self.tabs.remove(idx);
        if self.shown == Some(id) {
            self.shown = None;
        }
        if tab.life == Life::Live {
            out.push(Effect::DestroyView { tab: id, keep_state: false });
        }
        if tab.is_private() && !self.tabs.iter().any(|t| t.is_private()) {
            self.http_ok_private.clear();
            out.push(Effect::ReleasePrivateData);
        }
        if self.active == Some(id) {
            self.active = None;
            let next = self.tabs.get(idx).or_else(|| idx.checked_sub(1).and_then(|i| self.tabs.get(i)));
            match next.map(|t| t.id) {
                Some(n) => self.activate_into(n, now, &mut out),
                None => {
                    self.new_tab_into(Profile::Default, now, &mut out);
                }
            }
        }
        if !tab.is_private() {
            out.push(Effect::PersistSession(self.session()));
        }
        out.push(Effect::NotifyChrome);
        out
    }

    /// Address-bar submission.
    pub fn navigate_input(&mut self, id: TabId, text: &str, now: u64) -> Result<Vec<Effect>, InputError> {
        let target = match input::interpret(text, &self.search)? {
            Interpretation::Url(u) | Interpretation::Search(u) => u,
        };
        Ok(self.navigate_trusted(id, target, now))
    }

    /// Navigation initiated through trusted chrome (address bar, bookmark,
    /// history list). Still goes through policy with `Source::Typed`.
    pub fn navigate_trusted(&mut self, id: TabId, url: Url, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        let Some(tab) = self.tab_mut(id) else { return out };
        tab.notice = None;
        tab.https = HttpsUpgrade::default();
        let profile = tab.profile;
        let req = Request { url: &url, frame: Frame::Main, source: Source::Typed, initiator: None };
        match policy::decide(&req, self.exceptions(profile)) {
            Decision::Allow => self.load(id, url, Source::Typed, &mut out),
            Decision::UpgradeToHttps(https) => {
                if let Some(t) = self.tab_mut(id) {
                    t.https.http_url = Some(url);
                    t.https.upgrades = 1;
                }
                self.load(id, https, Source::Typed, &mut out);
            }
            Decision::External(u) => self.set_notice(id, external_notice(&u)),
            Decision::Block(r) => self.set_notice(id, blocked_notice(r, &url)),
        }
        self.enforce_budget(now, &mut out);
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        out
    }

    fn load(&mut self, id: TabId, url: Url, source: Source, out: &mut Vec<Effect>) {
        // No web view can exist (unsupported OS, desktop preview): do not
        // mark the tab as loading, or the chrome shows a progress bar that
        // never finishes.
        if !self.os.allows_browsing() {
            return;
        }
        let live = match self.tab_mut(id) {
            Some(t) => {
                t.loading = true;
                t.progress = 0;
                t.life == Life::Live
            }
            None => return,
        };
        if live {
            if let Some(t) = self.tab_mut(id) {
                t.expected_load = Some((url.clone(), source));
            }
            out.push(Effect::Load { tab: id, url });
        } else {
            self.ensure_live(id, AfterCreate::Load(url.clone()), Some((url, source)), out);
        }
    }

    fn ensure_live(&mut self, id: TabId, then: AfterCreate, expected: Option<(Url, Source)>, out: &mut Vec<Effect>) {
        if !self.os.allows_browsing() {
            return;
        }
        let Some(t) = self.tab_mut(id) else { return };
        if t.life == Life::Live {
            return;
        }
        t.life = Life::Live;
        t.expected_load = expected;
        t.loading = true;
        let profile = t.profile;
        out.push(Effect::CreateView { tab: id, profile, then });
    }

    pub fn go_back(&mut self, id: TabId) -> Vec<Effect> {
        self.simple(id, Effect::GoBack(id), true)
    }
    pub fn go_forward(&mut self, id: TabId) -> Vec<Effect> {
        self.simple(id, Effect::GoForward(id), false)
    }
    pub fn reload(&mut self, id: TabId) -> Vec<Effect> {
        self.simple(id, Effect::Reload(id), false)
    }
    pub fn stop(&mut self, id: TabId) -> Vec<Effect> {
        if let Some(t) = self.tab_mut(id) {
            t.https = HttpsUpgrade::default();
        }
        self.simple(id, Effect::Stop(id), false)
    }

    fn simple(&mut self, id: TabId, effect: Effect, back_dismisses_notice: bool) -> Vec<Effect> {
        let mut out = Vec::new();
        let Some(t) = self.tab_mut(id) else { return out };
        if back_dismisses_notice && t.notice.is_some() {
            // The previous committed page is still loaded under the notice.
            t.notice = None;
        } else if t.life == Life::Live {
            out.push(effect);
        } else if let (Effect::Reload(_), Some(url)) = (&effect, t.committed_url.clone()) {
            self.ensure_live(id, AfterCreate::Load(url.clone()), Some((url, Source::History)), &mut out);
        }
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        out
    }

    /// The user chose "Continue to HTTP" on the HTTPS-first interstitial.
    pub fn proceed_http(&mut self, id: TabId, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        let (profile, http_url) = match self.tab(id) {
            Some(Tab { profile, notice: Some(Notice::HttpsUnavailable { http_url, .. }), .. }) => {
                (*profile, http_url.clone())
            }
            _ => return out,
        };
        let Ok(url) = Url::parse(&http_url) else { return out };
        if let Some(host) = url.host_str() {
            match profile {
                Profile::Default => self.http_ok_default.allow(host),
                Profile::Private => self.http_ok_private.allow(host),
            }
        }
        out.extend(self.navigate_trusted(id, url, now));
        out
    }

    /// The user confirmed the "open in another app" prompt.
    pub fn confirm_external(&mut self, id: TabId) -> Vec<Effect> {
        let mut out = Vec::new();
        let url = match self.tab(id).and_then(|t| t.notice.as_ref()) {
            Some(Notice::ExternalApp { url, .. }) => Url::parse(url).ok(),
            _ => None,
        };
        if let Some(t) = self.tab_mut(id) {
            t.notice = None;
        }
        if let Some(u) = url {
            out.push(Effect::OpenExternal(u));
        }
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        out
    }

    pub fn dismiss_notice(&mut self, id: TabId) -> Vec<Effect> {
        let mut out = Vec::new();
        if let Some(t) = self.tab_mut(id) {
            t.notice = None;
        }
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        out
    }

    /// The chrome shows a full overlay (tab overview, menus) and the content
    /// view must be hidden so it cannot cover it.
    pub fn set_chrome_overlay(&mut self, on: bool) -> Vec<Effect> {
        let mut out = Vec::new();
        self.chrome_overlay = on;
        self.sync_visibility(&mut out);
        out
    }

    // ----- WebKit events -------------------------------------------------

    /// `decidePolicyForNavigationAction`. Must be fast: main thread.
    pub fn decide_navigation(
        &mut self,
        id: TabId,
        url: &Url,
        frame: Frame,
        kind: NavKind,
        now: u64,
    ) -> (Verdict, Vec<Effect>) {
        let mut out = Vec::new();
        let Some(tab) = self.tab_mut(id) else { return (Verdict::Cancel, out) };
        // Our own loads (address bar, upgrade, restore) are recognised by
        // URL equality and keep the source they were issued with; anything
        // else is classified from WebKit's navigation type.
        let source = match (frame, tab.expected_load.take()) {
            (Frame::Main, Some((expected, src))) if &expected == url => src,
            (_, other) => {
                tab.expected_load = other;
                classify(kind)
            }
        };
        let profile = tab.profile;
        let initiator = tab.committed_url.clone();
        let req = Request { url, frame, source, initiator: initiator.as_ref() };
        let decision = policy::decide(&req, self.exceptions(profile));
        let verdict = match (decision, frame) {
            (Decision::Allow, _) => Verdict::Allow,
            (Decision::UpgradeToHttps(https), Frame::Main) => {
                let tab = self.tab_mut(id).expect("tab exists");
                if tab.https.upgrades >= HttpsUpgrade::MAX_UPGRADES {
                    let http_url = tab.https.http_url.clone().unwrap_or_else(|| url.clone());
                    tab.https = HttpsUpgrade::default();
                    tab.loading = false;
                    let host = http_url.host_str().unwrap_or("").to_string();
                    tab.notice = Some(Notice::HttpsUnavailable { host, http_url: http_url.to_string() });
                } else {
                    if tab.https.http_url.is_none() {
                        tab.https.http_url = Some(url.clone());
                    }
                    tab.https.upgrades += 1;
                    tab.expected_load = Some((https.clone(), source));
                    out.push(Effect::Load { tab: id, url: https });
                }
                Verdict::Cancel
            }
            (Decision::UpgradeToHttps(_), Frame::Sub) => Verdict::Cancel,
            (Decision::External(u), Frame::Main) => {
                self.set_notice(id, external_notice(&u));
                Verdict::Cancel
            }
            (Decision::Block(r), Frame::Main) => {
                self.set_notice(id, blocked_notice(r, url));
                Verdict::Cancel
            }
            (Decision::External(_) | Decision::Block(_), Frame::Sub) => Verdict::Cancel,
        };
        if verdict == Verdict::Cancel && !out.is_empty() {
            self.enforce_budget(now, &mut out);
        }
        if verdict == Verdict::Cancel && frame == Frame::Main {
            self.sync_visibility(&mut out);
            out.push(Effect::NotifyChrome);
        }
        (verdict, out)
    }

    /// `webView:createWebViewWithConfiguration:…` (target=_blank, window.open
    /// with a user gesture). The page gets no `window.opener` reference.
    pub fn on_new_window_request(&mut self, opener: TabId, url: &Url, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        let Some(op) = self.tab(opener) else { return out };
        let profile = op.profile;
        let initiator = op.committed_url.clone();
        let source = Source::Page { user_gesture: true };
        let req = Request { url, frame: Frame::Main, source, initiator: initiator.as_ref() };
        let target = match policy::decide(&req, self.exceptions(profile)) {
            Decision::Allow => Some((url.clone(), false)),
            Decision::UpgradeToHttps(u) => Some((u, true)),
            Decision::External(u) => {
                self.set_notice(opener, external_notice(&u));
                None
            }
            Decision::Block(_) => None,
        };
        if let Some((target, upgraded)) = target {
            let id = self.new_tab_into(profile, now, &mut out);
            if upgraded {
                if let Some(t) = self.tab_mut(id) {
                    t.https.http_url = Some(url.clone());
                    t.https.upgrades = 1;
                }
            }
            self.load(id, target, source, &mut out);
            self.enforce_budget(now, &mut out);
            self.sync_visibility(&mut out);
        }
        out.push(Effect::NotifyChrome);
        out
    }

    pub fn on_provisional_start(&mut self, id: TabId) -> Vec<Effect> {
        if let Some(t) = self.tab_mut(id) {
            t.provisional = true;
            t.loading = true;
        }
        vec![Effect::NotifyChrome]
    }

    /// `didCommitNavigation` for the main frame. `url` is `WKWebView.URL`
    /// read at commit time.
    pub fn on_commit(&mut self, id: TabId, url: Url) -> Vec<Effect> {
        let mut out = Vec::new();
        let Some(t) = self.tab_mut(id) else { return out };
        t.committed_url = Some(url);
        t.provisional = false;
        t.https = HttpsUpgrade::default();
        t.notice = None;
        t.history_recorded = false;
        t.expected_load = None;
        let private = t.is_private();
        if !private {
            out.push(Effect::PersistSession(self.session()));
        }
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        out
    }

    /// KVO on `WKWebView.URL` outside of a commit (pushState, fragment).
    /// Accepted only when same-origin with the committed URL.
    pub fn on_url_changed(&mut self, id: TabId, url: Url) -> Vec<Effect> {
        let Some(t) = self.tab_mut(id) else { return Vec::new() };
        if t.provisional {
            return Vec::new();
        }
        match &t.committed_url {
            Some(c) if url_display::same_origin(c, &url) && *c != url => {
                t.committed_url = Some(url);
                vec![Effect::NotifyChrome]
            }
            _ => Vec::new(),
        }
    }

    pub fn on_finish(&mut self, id: TabId) -> Vec<Effect> {
        let mut out = Vec::new();
        let Some(t) = self.tab_mut(id) else { return out };
        t.loading = false;
        t.provisional = false;
        t.progress = 100;
        if !t.is_private() && !t.history_recorded {
            if let Some(u) = t.committed_url.clone().filter(|u| matches!(u.scheme(), "http" | "https")) {
                t.history_recorded = true;
                out.push(Effect::RecordHistory { url: u, title: t.title.clone() });
            }
        }
        out.push(Effect::NotifyChrome);
        out
    }

    pub fn on_fail(
        &mut self,
        id: TabId,
        provisional: bool,
        kind: NavErrorKind,
        failing_url: Option<Url>,
        message: &str,
    ) -> Vec<Effect> {
        let mut out = Vec::new();
        let Some(t) = self.tab_mut(id) else { return out };
        t.loading = false;
        if !provisional {
            out.push(Effect::NotifyChrome);
            return out;
        }
        t.provisional = false;
        if kind == NavErrorKind::Cancelled {
            out.push(Effect::NotifyChrome);
            return out;
        }
        if let Some(http_url) = t.https.http_url.take() {
            // HTTPS-first fallback: ask, never downgrade silently. This also
            // applies to certificate errors on an *upgraded* load, because
            // the user never asked for https in the first place.
            t.https = HttpsUpgrade::default();
            let host = http_url.host_str().unwrap_or("").to_string();
            t.notice = Some(Notice::HttpsUnavailable { host, http_url: http_url.to_string() });
        } else {
            t.notice = Some(Notice::LoadError {
                url: failing_url.map(|u| u.to_string()).unwrap_or_default(),
                message: sanitize_page_text(message, MAX_PAGE_TEXT_CHARS),
                certificate: kind == NavErrorKind::Certificate,
            });
        }
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        out
    }

    pub fn on_title(&mut self, id: TabId, title: &str) -> Vec<Effect> {
        match self.tab_mut(id) {
            Some(t) => {
                let clean = sanitize_page_text(title, MAX_PAGE_TEXT_CHARS);
                if clean == t.title {
                    return Vec::new();
                }
                t.title = clean;
                vec![Effect::NotifyChrome]
            }
            None => Vec::new(),
        }
    }

    /// Progress is coalesced to 5% steps to keep chrome updates cheap.
    pub fn on_progress(&mut self, id: TabId, fraction: f64) -> Vec<Effect> {
        let Some(t) = self.tab_mut(id) else { return Vec::new() };
        let p = (fraction.clamp(0.0, 1.0) * 100.0) as u8;
        if p / 5 == t.progress / 5 {
            return Vec::new();
        }
        t.progress = p;
        vec![Effect::NotifyChrome]
    }

    pub fn on_history_state(&mut self, id: TabId, back: bool, forward: bool) -> Vec<Effect> {
        let Some(t) = self.tab_mut(id) else { return Vec::new() };
        if (t.can_go_back, t.can_go_forward) == (back, forward) {
            return Vec::new();
        }
        t.can_go_back = back;
        t.can_go_forward = forward;
        vec![Effect::NotifyChrome]
    }

    pub fn on_secure_content(&mut self, id: TabId, only_secure: bool) -> Vec<Effect> {
        let Some(t) = self.tab_mut(id) else { return Vec::new() };
        if t.secure_content == only_secure {
            return Vec::new();
        }
        t.secure_content = only_secure;
        vec![Effect::NotifyChrome]
    }

    /// `webViewWebContentProcessDidTerminate`. iOS kills WebContent
    /// processes under memory pressure; reload the active tab a bounded
    /// number of times, discard background tabs.
    pub fn on_process_terminated(&mut self, id: TabId, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        if self.active == Some(id) {
            let Some(t) = self.tab_mut(id) else { return out };
            t.crash_times_ms.retain(|&c| now.saturating_sub(c) < CRASH_WINDOW_MS);
            t.crash_times_ms.push(now);
            if t.crash_times_ms.len() <= MAX_CRASH_RELOADS {
                out.push(Effect::Reload(id));
            } else {
                t.notice = Some(Notice::Crashed);
                t.loading = false;
                self.sync_visibility(&mut out);
            }
        } else {
            self.discard(id, &mut out);
        }
        out.push(Effect::NotifyChrome);
        out
    }

    /// `UIApplicationDidReceiveMemoryWarningNotification`: keep only the
    /// active web view.
    pub fn on_memory_warning(&mut self) -> Vec<Effect> {
        let mut out = Vec::new();
        let victims: Vec<TabId> =
            self.tabs.iter().filter(|t| t.life == Life::Live && Some(t.id) != self.active).map(|t| t.id).collect();
        for id in victims {
            self.discard(id, &mut out);
        }
        out.push(Effect::NotifyChrome);
        out
    }

    /// Periodic timer (e.g. every 30 s) and app-backgrounding hook.
    pub fn tick(&mut self, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        self.enforce_budget(now, &mut out);
        if !out.is_empty() {
            out.push(Effect::NotifyChrome);
        }
        out
    }

    // ----- internals -----------------------------------------------------

    fn enforce_budget(&mut self, now: u64, out: &mut Vec<Effect>) {
        let mut background: Vec<(u64, TabId)> = self
            .tabs
            .iter()
            .filter(|t| t.life == Life::Live && Some(t.id) != self.active)
            .map(|t| (t.last_active_ms, t.id))
            .collect();
        background.sort_by_key(|b| std::cmp::Reverse(b.0)); // most recently used first
        let allowed = self.budget.max_live.saturating_sub(1);
        for (i, (last, id)) in background.into_iter().enumerate() {
            if i >= allowed || now.saturating_sub(last) > self.budget.background_ttl_ms {
                self.discard(id, out);
            }
        }
    }

    fn discard(&mut self, id: TabId, out: &mut Vec<Effect>) {
        let Some(t) = self.tab_mut(id) else { return };
        if t.life != Life::Live {
            return;
        }
        t.life = Life::Unloaded;
        t.loading = false;
        t.provisional = false;
        t.expected_load = None;
        if self.shown == Some(id) {
            self.shown = None;
            out.push(Effect::HideView(id));
        }
        out.push(Effect::DestroyView { tab: id, keep_state: true });
    }

    fn set_notice(&mut self, id: TabId, notice: Notice) {
        if let Some(t) = self.tab_mut(id) {
            t.notice = Some(notice);
            t.loading = false;
        }
    }

    fn desired_shown(&self) -> Option<TabId> {
        if self.chrome_overlay {
            return None;
        }
        let t = self.tab(self.active?)?;
        (t.life == Life::Live && t.notice.is_none() && t.committed_url.is_some()).then_some(t.id)
    }

    fn sync_visibility(&mut self, out: &mut Vec<Effect>) {
        let want = self.desired_shown();
        if want == self.shown {
            return;
        }
        if let Some(old) = self.shown {
            out.push(Effect::HideView(old));
        }
        if let Some(new) = want {
            out.push(Effect::ShowView(new));
        }
        self.shown = want;
    }

    /// Non-private tabs only, by construction.
    pub fn session(&self) -> Session {
        let persistable: Vec<&Tab> = self
            .tabs
            .iter()
            .filter(|t| !t.is_private())
            .filter(|t| t.committed_url.as_ref().map(|u| matches!(u.scheme(), "http" | "https")).unwrap_or(false))
            .collect();
        let active = self.active.and_then(|a| persistable.iter().position(|t| t.id == a));
        Session {
            tabs: persistable
                .iter()
                .map(|t| SessionTab {
                    url: t.committed_url.as_ref().map(|u| u.to_string()).unwrap_or_default(),
                    title: t.title.clone(),
                })
                .collect(),
            active,
        }
    }

    pub fn view(&self) -> ChromeState {
        ChromeState {
            tabs: self.tabs.iter().map(tab_view).collect(),
            active: self.active,
            os: self.os,
            content_visible: self.shown.is_some(),
            max_live_tabs: self.budget.max_live,
        }
    }
}

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
    // Show only the scheme and display host, never the raw (possibly huge
    // or deceptive) URL.
    let d = url_display::display(u);
    let shown = if d.registrable.is_empty() { format!("{}:", u.scheme()) } else { d.full.chars().take(200).collect() };
    Notice::Blocked { reason: reason.to_string(), url: shown }
}

fn tab_view(t: &Tab) -> TabView {
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
        can_go_back: t.can_go_back || t.notice.is_some(),
        can_go_forward: t.can_go_forward,
        security,
        live: t.life == Life::Live,
        notice: t.notice.clone(),
    }
}
