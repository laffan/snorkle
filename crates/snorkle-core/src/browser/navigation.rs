//! Navigation: typed and trusted loads, back/forward/reload, the policy
//! decision for every WebKit navigation, popups and background tabs.

use super::*;

impl Browser {
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
        // A saved copy's tab only shows its archive: an address typed there
        // opens in a new ordinary tab (except the live loads of pages being
        // added to it).
        if self.saved_copy(id).is_some_and(|c| !c.adding) {
            let profile = self.tab(id).map_or(Profile::Default, |t| t.profile);
            let fresh = self.new_tab_into(profile, now, &mut out);
            out.extend(self.navigate_trusted(fresh, url, now));
            return out;
        }
        let Some(tab) = self.tab_mut(id) else { return out };
        tab.notice = None;
        tab.https = HttpsUpgrade::default();
        let profile = tab.profile;
        let req = Request { url: &url, frame: Frame::Main, source: Source::Typed, initiator: None };
        match policy::decide(&req, self.exceptions(profile)) {
            Decision::Allow | Decision::UpgradeToHttps(_) if self.blocklist.matches(&url) => {
                let notice = self.user_blocked_notice(&url);
                self.set_notice(id, notice);
            }
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

    pub(super) fn load(&mut self, id: TabId, url: Url, source: Source, out: &mut Vec<Effect>) {
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

    pub(super) fn ensure_live(&mut self, id: TabId, then: AfterCreate, expected: Option<(Url, Source)>, out: &mut Vec<Effect>) {
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
        let (profile, zoom) = (t.profile, t.zoom);
        out.push(Effect::CreateView { tab: id, profile, then });
        if zoom != crate::tabs::DEFAULT_ZOOM {
            out.push(Effect::SetZoom { tab: id, percent: zoom });
        }
    }

    /// Back. A ⌘-clicked tab with no back history of its own goes to the
    /// page it was opened from (`Tab::back_to`), once.
    pub fn go_back(&mut self, id: TabId, now: u64) -> Vec<Effect> {
        let fallback = self.tab(id).filter(|t| !t.can_go_back && t.notice.is_none()).and_then(|t| t.back_to.clone());
        if let Some(url) = fallback {
            if let Some(t) = self.tab_mut(id) {
                t.back_to = None;
            }
            return self.navigate_trusted(id, url, now);
        }
        self.simple(id, Effect::GoBack(id), true)
    }
    pub fn go_forward(&mut self, id: TabId) -> Vec<Effect> {
        self.simple(id, Effect::GoForward(id), false)
    }
    pub fn set_url_bar_attached(&mut self, id: TabId, attached: bool) -> Vec<Effect> {
        let Some(tab) = self.tab_mut(id) else { return Vec::new() };
        tab.url_bar_attached = attached;
        vec![Effect::PersistSession(self.session()), Effect::NotifyChrome]
    }

    pub fn reload(&mut self, id: TabId) -> Vec<Effect> {
        // A saved archive reloads from the archive, not from the network.
        if let Some(c) = self.saved_copy(id).filter(|c| c.committed && c.picking.is_none()) {
            return vec![Effect::OpenSavedPage { tab: id, item: c.item.clone(), url: c.page.clone() }];
        }
        self.simple(id, Effect::Reload(id), false)
    }
    /// Reload as the user asks for it (the reload button, ⌘R, the error
    /// notice's Try Again; owner report 2026-11-16): a page that failed to
    /// load (no connection, a name that did not resolve, a time-out) was
    /// never committed by the web view, whose own reload repeats the page
    /// before it, or nothing in a new tab. Its address (http or https, as
    /// the notice shows it) is loaded again instead, through policy like
    /// a typed address. Certificate failures and everything else reload as
    /// [`Browser::reload`] does.
    pub fn reload_by_user(&mut self, id: TabId, now: u64) -> Vec<Effect> {
        let failed = match self.tab(id).and_then(|t| t.notice.as_ref()) {
            Some(Notice::LoadError { url, certificate: false, .. }) => {
                Url::parse(url).ok().filter(|u| matches!(u.scheme(), "http" | "https"))
            }
            _ => None,
        };
        match failed {
            Some(url) => self.navigate_trusted(id, url, now),
            None => self.reload(id),
        }
    }
    pub fn stop(&mut self, id: TabId) -> Vec<Effect> {
        if let Some(t) = self.tab_mut(id) {
            t.https = HttpsUpgrade::default();
        }
        self.simple(id, Effect::Stop(id), false)
    }

    pub(super) fn simple(&mut self, id: TabId, effect: Effect, back_dismisses_notice: bool) -> Vec<Effect> {
        let mut out = Vec::new();
        // A blocked page itself is under the notice (a single-page app moved
        // to a blocked address, or the list changed): Back really goes back.
        let page_blocked = self.committed_blocked(id);
        let Some(t) = self.tab_mut(id) else { return out };
        if back_dismisses_notice && t.notice.is_some() && !page_blocked {
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
        let (source, own_load) = match (frame, tab.expected_load.take()) {
            (Frame::Main, Some((expected, src))) if &expected == url => (src, true),
            (_, other) => {
                tab.expected_load = other;
                (classify(kind), false)
            }
        };
        // A tab showing a saved archive: links to its other pages open from
        // the archive; while adding pages, link taps choose pages.
        if frame == Frame::Main && !own_load {
            if let Some(handled) = self.saved_copy_navigation(id, url, kind, now) {
                return handled;
            }
        }
        let Some(tab) = self.tab_mut(id) else { return (Verdict::Cancel, out) };
        let profile = tab.profile;
        let initiator = tab.committed_url.clone();
        let req = Request { url, frame, source, initiator: initiator.as_ref() };
        let decision = policy::decide(&req, self.exceptions(profile));
        let verdict = match (decision, frame) {
            // The user's block list: main frames only, whatever started
            // the navigation (typed, link, redirect, script, history).
            (Decision::Allow | Decision::UpgradeToHttps(_), Frame::Main) if self.blocklist.matches(url) => {
                let notice = self.user_blocked_notice(url);
                let tab = self.tab_mut(id).expect("tab exists");
                tab.https = HttpsUpgrade::default();
                tab.expected_load = None;
                self.set_notice(id, notice);
                Verdict::Cancel
            }
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
    /// SECURITY-REVIEW: `window.open` / target=_blank. Called synchronously
    /// from `createWebViewWithConfiguration`; returns the new tab when the
    /// platform should create a real popup web view (which keeps a
    /// `window.opener` relationship, needed by OAuth and payment flows).
    /// WebKit applies `noopener` itself where the page asked for it (and to
    /// `target=_blank` links by default). The popup's own navigations go
    /// through `decide_navigation` like any other tab; the address bar keeps
    /// showing each tab's committed URL, so an opener navigated by its popup
    /// is visible.
    pub fn open_popup(
        &mut self,
        opener: TabId,
        url: Option<&Url>,
        style: PopupStyle,
        now: u64,
    ) -> (Option<TabId>, Vec<Effect>) {
        let mut out = Vec::new();
        let Some(op) = self.tab(opener) else { return (None, out) };
        // The popup and its opener both stay live; never exceed the budget.
        if !self.os.allows_browsing() || self.budget.max_live < 2 {
            return (None, out);
        }
        let profile = op.profile;
        let initiator = op.committed_url.clone();
        if let Some(url) = url {
            let req = Request {
                url,
                frame: Frame::Main,
                source: Source::Page { user_gesture: true },
                initiator: initiator.as_ref(),
            };
            match policy::decide(&req, self.exceptions(profile)) {
                // http is upgraded by the popup's own navigation policy.
                Decision::Allow | Decision::UpgradeToHttps(_) => {}
                Decision::External(u) => {
                    self.set_notice(opener, external_notice(&u));
                    out.push(Effect::NotifyChrome);
                    return (None, out);
                }
                Decision::Block(_) => return (None, out),
            }
        }
        let id = self.alloc_id();
        let insert_at = self.tabs.iter().position(|t| t.id == opener).map(|i| i + 1).unwrap_or(self.tabs.len());
        let mut tab = Tab::new(id, profile, now);
        tab.life = Life::Live;
        tab.loading = true;
        tab.opener = Some(opener);
        tab.sheet = style == PopupStyle::Sheet;
        self.tabs.insert(insert_at, tab);
        out.push(Effect::AdoptPopup { tab: id });
        self.activate_into(id, now, &mut out);
        out.push(Effect::NotifyChrome);
        (Some(id), out)
    }

    /// `webViewDidClose` (the page called `window.close()`). Only honoured
    /// for popups, which return focus to their opener.
    pub fn on_close_requested(&mut self, id: TabId, now: u64) -> Vec<Effect> {
        match self.tab(id) {
            Some(t) if t.opener.is_some() => self.close(id, now),
            _ => Vec::new(),
        }
    }

    /// ⌘-click on a link: opens `url` in a new tab right after `opener`, in
    /// the opener's profile, without switching to it. Only http(s) URLs;
    /// they go through the navigation policy as a page link with a user
    /// gesture (blocked links open nothing; http is upgraded). The new tab
    /// loads in the background, within the live-view budget.
    pub fn on_background_tab_request(&mut self, opener: TabId, url: &Url, now: u64) -> (Option<TabId>, Vec<Effect>) {
        let mut out = Vec::new();
        let Some(op) = self.tab(opener) else { return (None, out) };
        // Only web pages: `javascript:` and friends run in the page that
        // asked, never in a new tab.
        if !self.os.allows_browsing() || !matches!(url.scheme(), "http" | "https") {
            return (None, out);
        }
        let profile = op.profile;
        let initiator = op.committed_url.clone();
        let source = Source::Page { user_gesture: true };
        let req = Request { url, frame: Frame::Main, source, initiator: initiator.as_ref() };
        let (target, upgraded) = match policy::decide(&req, self.exceptions(profile)) {
            Decision::Allow => (url.clone(), false),
            Decision::UpgradeToHttps(u) => (u, true),
            Decision::External(_) | Decision::Block(_) => return (None, out),
        };
        let id = self.alloc_id();
        let insert_at = self.tabs.iter().position(|t| t.id == opener).map(|i| i + 1).unwrap_or(self.tabs.len());
        // `last_active_ms = now`: most recently used after the shown tabs,
        // so the budget keeps it live (it discards older background tabs).
        let mut tab = Tab::new(id, profile, now);
        if upgraded {
            tab.https.http_url = Some(url.clone());
            tab.https.upgrades = 1;
        }
        // Back from the new tab returns to the page the link was on.
        tab.back_to = initiator.filter(|u| matches!(u.scheme(), "http" | "https"));
        self.tabs.insert(insert_at, tab);
        self.load(id, target, source, &mut out);
        self.enforce_budget(now, &mut out);
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        (Some(id), out)
    }

    /// Save for Later from the history sidebar (owner request 2026-11-23):
    /// a new tab of the default profile at the end of the list, never
    /// shown, that loads `url` as an address typed in the bar would
    /// (navigation policy, Settings → Blocked, HTTPS-first). It loads in the
    /// background within the live-view budget, as a ⌘-clicked link's tab
    /// does; the caller saves it, which closes it. Only http(s) URLs.
    pub fn open_in_background(&mut self, url: Url, now: u64) -> (Option<TabId>, Vec<Effect>) {
        let mut out = Vec::new();
        if !self.os.allows_browsing() || !matches!(url.scheme(), "http" | "https") {
            return (None, out);
        }
        let id = self.alloc_id();
        self.tabs.push(Tab::new(id, Profile::Default, now));
        out.extend(self.navigate_trusted(id, url, now));
        self.enforce_budget(now, &mut out);
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        (Some(id), out)
    }

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
}
