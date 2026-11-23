//! The browser's tabs: creating, restoring, activating, split views.

use super::*;

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
            shown: Vec::new(),
            splits: Vec::new(),
            shown_ratio: SPLIT_RATIO_DEFAULT,
            adblock_allowed: BTreeSet::new(),
            adblock_status: AdblockStatus::Off,
            closed: Vec::new(),
            blocklist: BlockList::default(),
        }
    }

    pub fn budget(&self) -> MemoryBudget {
        self.budget
    }

    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub(super) fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    /// Every tab, in sidebar order.
    pub fn tab_ids(&self) -> Vec<TabId> {
        self.tabs.iter().map(|t| t.id).collect()
    }

    pub fn active(&self) -> Option<TabId> {
        self.active
    }

    pub fn live_count(&self) -> usize {
        self.tabs.iter().filter(|t| t.life == Life::Live).count()
    }

    pub(super) fn exceptions(&self, p: Profile) -> &HttpExceptions {
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
        let ids = self.restore_tabs(session, now);
        match session.active.and_then(|i| ids.get(i)).or(ids.last()).copied() {
            Some(id) => self.activate_into(id, now, &mut out),
            None => {
                self.new_tab_into(Profile::Default, now, &mut out);
            }
        }
        out.push(Effect::NotifyChrome);
        out
    }

    /// A launch: restores the saved tabs like [`Browser::restore_session`],
    /// all unloaded, and greets the user with a new empty tab (the chrome's
    /// start page) placed after the saved active tab. No restored page is
    /// loaded until it is chosen.
    pub fn restore_session_with_start_tab(&mut self, session: &Session, now: u64) -> (TabId, Vec<Effect>) {
        let mut out = Vec::new();
        let ids = self.restore_tabs(session, now);
        let after = session.active.and_then(|i| ids.get(i)).or(ids.last()).copied();
        let at = after
            .and_then(|a| self.tabs.iter().position(|t| t.id == a))
            .map(|i| i + 1)
            .unwrap_or(self.tabs.len());
        let id = self.alloc_id();
        self.tabs.insert(at, Tab::new(id, Profile::Default, now));
        self.activate_into(id, now, &mut out);
        out.push(Effect::NotifyChrome);
        (id, out)
    }

    /// Adds a saved session's tabs, unloaded; their ids in session order.
    pub(super) fn restore_tabs(&mut self, session: &Session, now: u64) -> Vec<TabId> {
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
            tab.launcher = st.launcher.as_deref().and_then(crate::storage::storable_url);
            tab.view_exempt = clean_exempt(&st.exempt);
            tab.url_bar_attached = st.url_bar_attached;
            tab.zoom = st.zoom.map(crate::tabs::clean_zoom).unwrap_or(crate::tabs::DEFAULT_ZOOM);
            tab.name = st.name.as_deref().and_then(clean_tab_name);
            tab.emoji = st.emoji.as_deref().and_then(crate::storage::clean_emoji);
            self.tabs.push(tab);
            ids.push(id);
        }
        ids
    }

    pub fn new_tab(&mut self, profile: Profile, now: u64) -> (TabId, Vec<Effect>) {
        let mut out = Vec::new();
        let id = self.new_tab_into(profile, now, &mut out);
        out.push(Effect::NotifyChrome);
        (id, out)
    }

    pub(super) fn new_tab_into(&mut self, profile: Profile, now: u64, out: &mut Vec<Effect>) -> TabId {
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

    pub(super) fn alloc_id(&mut self) -> TabId {
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

    pub(super) fn activate_into(&mut self, id: TabId, now: u64, out: &mut Vec<Effect>) {
        // A tab with a popup sheet open is shown with the sheet in front.
        let mut id = id;
        for _ in 0..self.tabs.len() {
            match self.tabs.iter().rev().find(|t| t.sheet && t.opener == Some(id)) {
                Some(child) => id = child.id,
                None => break,
            }
        }
        if let Some(prev) = self.active {
            if let Some(t) = self.tab_mut(prev) {
                t.last_active_ms = now;
            }
        }
        if self.tab(id).is_none() {
            return;
        }
        self.active = Some(id);
        // Choosing a tab in a pair shows the pair, so its partner needs a
        // live view too; a sheet needs its opener underneath.
        if let Some(partner) = self.pair_of(id).map(|s| if s.left == id { s.right } else { s.left }) {
            self.restore_if_unloaded(partner, now, out);
        }
        if let Some((_, opener)) = self.front_sheet() {
            self.restore_if_unloaded(opener, now, out);
        }
        self.restore_if_unloaded(id, now, out);
        self.enforce_budget(now, out);
        self.sync_visibility(out);
    }

    /// Pairs `dragged` with `target` (target left, dragged right) and
    /// focuses `dragged`. Both get live web views. Any pair either tab was
    /// in ends. Tabs of different profiles, and popup sheets, are never
    /// paired; a popup tab whose opener is outside the pair only with a
    /// budget of at least 3 live views (its opener stays live as well).
    pub fn split(&mut self, dragged: TabId, target: TabId, now: u64) -> Vec<Effect> {
        self.split_oriented(dragged, target, SplitOrientation::Columns, now)
    }

    /// [`Browser::split`] with a layout.
    pub fn split_oriented(
        &mut self,
        dragged: TabId,
        target: TabId,
        orientation: SplitOrientation,
        now: u64,
    ) -> Vec<Effect> {
        let mut out = Vec::new();
        // Both panes need live web views; never exceed the memory budget.
        if dragged == target || self.budget.max_live < 2 {
            return out;
        }
        let (d_opener, t_opener) = match (self.tab(dragged), self.tab(target)) {
            // Popup sheets are not tabs (not listed, never paired).
            (Some(d), Some(t)) if d.profile == t.profile && !d.sheet && !t.sheet => (d.opener, t.opener),
            _ => return out,
        };
        // While a pane that is a popup is active, its opener is kept live
        // too (`is_protected`): three views, so only within a budget of 3.
        let outside_opener = [d_opener, t_opener].iter().flatten().any(|o| *o != dragged && *o != target);
        if outside_opener && self.budget.max_live < 3 {
            return out;
        }
        self.splits.retain(|s| ![s.left, s.right].iter().any(|t| *t == dragged || *t == target));
        self.splits.push(SplitView { left: target, right: dragged, orientation, ratio: SPLIT_RATIO_DEFAULT });
        self.activate_into(dragged, now, &mut out);
        out.push(Effect::NotifyChrome);
        out
    }

    /// ⌘S (columns) and ⌘⇧S (rows). With the active tab in a pair: lays
    /// the pair out that way (nothing if it already is; never a third
    /// pane). Otherwise: opens an empty tab of the active tab's profile and
    /// pairs it with the active tab, the new tab second (right or below)
    /// and focused. Returns the new tab. Not for popup sheets, nor where
    /// [`Browser::split`] would refuse the pair.
    pub fn split_new(&mut self, orientation: SplitOrientation, now: u64) -> (Option<TabId>, Vec<Effect>) {
        let mut out = Vec::new();
        let Some(active) = self.active else { return (None, out) };
        if let Some(i) = self.splits.iter().position(|s| s.left == active || s.right == active) {
            if self.splits[i].orientation != orientation {
                self.splits[i].orientation = orientation;
                self.sync_visibility(&mut out);
                out.push(Effect::NotifyChrome);
            }
            return (None, out);
        }
        let Some(t) = self.tab(active) else { return (None, out) };
        // As `split` checks: the active tab's opener stays live too.
        let needs = if t.opener.is_some() { 3 } else { 2 };
        if t.sheet || self.budget.max_live < needs || !self.os.allows_browsing() {
            return (None, out);
        }
        let profile = t.profile;
        let id = self.new_tab_into(profile, now, &mut out);
        out.extend(self.split_oriented(id, active, orientation, now));
        (Some(id), out)
    }

    /// The divider of the pair on screen was dragged: its first pane takes
    /// `permille` of the space (clamped to 20–80%).
    pub fn set_split_ratio(&mut self, permille: u16) -> Vec<Effect> {
        let permille = permille.clamp(SPLIT_RATIO_MIN, SPLIT_RATIO_MAX);
        let Some(active) = self.active else { return Vec::new() };
        let Some(pair) = self.splits.iter_mut().find(|s| s.left == active || s.right == active) else {
            return Vec::new();
        };
        if pair.ratio == permille {
            return Vec::new();
        }
        pair.ratio = permille;
        self.shown_ratio = permille;
        vec![Effect::SetSplitRatio(permille), Effect::NotifyChrome]
    }

    /// Ends the pair holding `id`, if any. Both tabs stay open and the
    /// active tab stays active (full width if its pair was on screen).
    pub fn unsplit(&mut self, id: TabId) -> Vec<Effect> {
        let mut out = Vec::new();
        let Some(pair) = self.pair_of(id) else { return out };
        self.splits.retain(|s| *s != pair);
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        out
    }

    /// The pair on screen: the one holding the active tab.
    pub fn split_view(&self) -> Option<SplitView> {
        self.active.and_then(|a| self.pair_of(a))
    }

    pub(super) fn pair_of(&self, id: TabId) -> Option<SplitView> {
        self.splits.iter().copied().find(|s| s.left == id || s.right == id)
    }

    pub(super) fn restore_if_unloaded(&mut self, id: TabId, now: u64, out: &mut Vec<Effect>) {
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
    }

    /// The active tab and its split partner are never discarded by the
    /// budget or memory warnings. Pairs not on screen can be discarded.
    pub(super) fn is_protected(&self, id: TabId) -> bool {
        Some(id) == self.active
            || self.split_view().is_some_and(|s| s.left == id || s.right == id)
            // The opener of the active popup stays alive so the popup can
            // hand its result back (OAuth).
            || self.active.and_then(|a| self.tab(a)).and_then(|t| t.opener) == Some(id)
    }
}
