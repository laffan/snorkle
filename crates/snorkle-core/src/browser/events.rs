//! WebKit's reports (load progress, commits, failures, titles, crashes,
//! memory), the tick and the live-view budget, notices and the block list.

use super::*;

impl Browser {
    pub fn on_provisional_start(&mut self, id: TabId) -> Vec<Effect> {
        if let Some(t) = self.tab_mut(id) {
            t.provisional = true;
            t.loading = true;
            // Any load after an offline copy's own replaces it. (A saved
            // copy's tab stays one: see `Tab::saved`.)
            if t.offline.is_some_and(|o| o.committed) {
                t.offline = None;
            }
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
        t.offline_tried = false;
        if let Some(o) = t.offline.as_mut() {
            o.committed = true;
            // Not a visit: the copy is not written to history.
            t.history_recorded = true;
        }
        if let Some(c) = t.saved.as_mut() {
            c.committed = true;
            t.history_recorded = true;
        }
        let private = t.is_private();
        // Defence in depth: a blocked page that committed without passing
        // the navigation policy (a restored back/forward state) stays
        // hidden behind the notice.
        self.block_if_listed(id);
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
                let mut out = Vec::new();
                // Single-page apps change address without a navigation
                // (pushState): a blocked one is hidden behind the notice,
                // and the notice goes when the page leaves it (Back).
                if !self.block_if_listed(id) {
                    if let Some(t) = self.tab_mut(id).filter(|t| matches!(t.notice, Some(Notice::UserBlocked { .. }))) {
                        t.notice = None;
                    }
                }
                self.sync_visibility(&mut out);
                out.push(Effect::NotifyChrome);
                out
            }
            _ => Vec::new(),
        }
    }

    pub fn on_finish(&mut self, id: TabId) -> Vec<Effect> {
        let mut out = Vec::new();
        let Some(t) = self.tab_mut(id) else { return out };
        t.loading = false;
        t.landing = None;
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
        if provisional {
            t.landing = None;
        }
        // An offline copy that did not load: forget it (`offline_tried`
        // keeps it from being tried again).
        if provisional && t.offline.is_some_and(|o| !o.committed) {
            t.offline = None;
        }
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
        if self.is_protected(id) {
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
            self.tabs.iter().filter(|t| t.life == Life::Live && !self.is_protected(t.id)).map(|t| t.id).collect();
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

    pub(super) fn enforce_budget(&mut self, now: u64, out: &mut Vec<Effect>) {
        let mut background: Vec<(u64, TabId)> = self
            .tabs
            .iter()
            .filter(|t| t.life == Life::Live && !self.is_protected(t.id))
            .map(|t| (t.last_active_ms, t.id))
            .collect();
        background.sort_by_key(|b| std::cmp::Reverse(b.0)); // most recently used first
        let protected = self.tabs.iter().filter(|t| t.life == Life::Live && self.is_protected(t.id)).count().max(1);
        let allowed = self.budget.max_live.saturating_sub(protected);
        for (i, (last, id)) in background.into_iter().enumerate() {
            if i >= allowed || now.saturating_sub(last) > self.budget.background_ttl_ms {
                self.discard(id, out);
            }
        }
    }

    pub(super) fn discard(&mut self, id: TabId, out: &mut Vec<Effect>) {
        let Some(t) = self.tab_mut(id) else { return };
        if t.life != Life::Live {
            return;
        }
        t.life = Life::Unloaded;
        t.loading = false;
        t.provisional = false;
        t.expected_load = None;
        if self.shown.iter().any(|(t, _)| *t == id) {
            self.shown.retain(|(t, _)| *t != id);
            out.push(Effect::HideView(id));
        }
        out.push(Effect::DestroyView { tab: id, keep_state: true });
    }

    pub(super) fn set_notice(&mut self, id: TabId, notice: Notice) {
        if let Some(t) = self.tab_mut(id) {
            t.notice = Some(notice);
            t.loading = false;
        }
    }

    pub(super) fn showable(&self, id: TabId) -> bool {
        self.tab(id).is_some_and(|t| {
            t.life == Life::Live
                && t.notice.is_none()
                && t.committed_url.as_ref().is_some_and(|u| !self.blocklist.matches(u))
        })
    }

    /// Whether the tab's committed page is on the block list.
    pub(super) fn committed_blocked(&self, id: TabId) -> bool {
        self.tab(id).and_then(|t| t.committed_url.as_ref()).is_some_and(|u| self.blocklist.matches(u))
    }

    pub(super) fn user_blocked_notice(&self, u: &Url) -> Notice {
        Notice::UserBlocked { message: self.blocklist.message().to_string(), url: shown_url(u) }
    }

    /// Puts the block notice on a tab whose committed page is on the list
    /// (unless it already shows that notice). Returns whether it is blocked.
    pub(super) fn block_if_listed(&mut self, id: TabId) -> bool {
        let Some(url) = self.tab(id).and_then(|t| t.committed_url.clone()) else { return false };
        if !self.blocklist.matches(&url) {
            return false;
        }
        let notice = self.user_blocked_notice(&url);
        if let Some(t) = self.tab_mut(id) {
            if !matches!(t.notice, Some(Notice::UserBlocked { .. })) {
                t.notice = Some(notice);
            }
        }
        true
    }
}
