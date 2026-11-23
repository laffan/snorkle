//! What is shown where, the session to save, block and ad-block lists, and
//! the chrome's view of the state.

use super::*;

impl Browser {
    pub(super) fn desired_shown(&self) -> Vec<(TabId, Pane)> {
        if self.chrome_overlay {
            return Vec::new();
        }
        // A popup sheet in front: its opener underneath, full width, and the
        // sheet once it has something to show. While the sheet shows a
        // notice, nothing is shown, so the chrome can draw the notice.
        if let Some((sheet, opener)) = self.front_sheet() {
            if self.tab(sheet).is_some_and(|t| t.notice.is_some()) {
                return Vec::new();
            }
            return [(opener, Pane::Full), (sheet, Pane::Sheet)]
                .into_iter()
                .filter(|(t, _)| self.showable(*t))
                .collect();
        }
        let wanted = match (self.split_view(), self.active) {
            (Some(s), _) => match s.orientation {
                SplitOrientation::Columns => vec![(s.left, Pane::Left), (s.right, Pane::Right)],
                SplitOrientation::Rows => vec![(s.left, Pane::Top), (s.right, Pane::Bottom)],
            },
            (None, Some(a)) => vec![(a, Pane::Full)],
            (None, None) => Vec::new(),
        };
        wanted.into_iter().filter(|(t, _)| self.showable(*t)).collect()
    }

    /// The active tab and its opener, if the active tab is a popup sheet.
    pub(super) fn front_sheet(&self) -> Option<(TabId, TabId)> {
        let t = self.tab(self.active?)?;
        let opener = t.opener.filter(|_| t.sheet).and_then(|o| self.tab(o))?;
        Some((t.id, opener.id))
    }

    pub(super) fn sync_visibility(&mut self, out: &mut Vec<Effect>) {
        // The host lays out a pair's panes with its ratio: send it first.
        if let Some(s) = self.split_view().filter(|_| self.front_sheet().is_none()) {
            if s.ratio != self.shown_ratio {
                self.shown_ratio = s.ratio;
                out.push(Effect::SetSplitRatio(s.ratio));
            }
        }
        let want = self.desired_shown();
        if want == self.shown {
            return;
        }
        for (old, _) in &self.shown {
            if !want.iter().any(|(t, _)| t == old) {
                out.push(Effect::HideView(*old));
            }
        }
        let mut raised = false;
        for (tab, pane) in &want {
            if !self.shown.contains(&(*tab, *pane)) || (raised && *pane == Pane::Sheet) {
                // Showing a view raises it, so a sheet already on screen is
                // raised again to stay above a pane shown under it.
                out.push(Effect::ShowView { tab: *tab, pane: *pane });
                raised = true;
            }
        }
        self.shown = want;
    }

    /// Non-private tabs only, by construction.
    pub fn session(&self) -> Session {
        let persistable: Vec<&Tab> = self
            .tabs
            .iter()
            .filter(|t| !t.is_private() && !t.sheet && t.saved.is_none())
            .filter(|t| t.committed_url.as_ref().map(|u| matches!(u.scheme(), "http" | "https")).unwrap_or(false))
            .collect();
        let active = self.active.and_then(|a| persistable.iter().position(|t| t.id == a));
        Session {
            tabs: persistable
                .iter()
                .map(|t| SessionTab {
                    url_bar_attached: t.url_bar_attached,
                    exempt: t.view_exempt.clone(),
                    zoom: (t.zoom != crate::tabs::DEFAULT_ZOOM).then_some(t.zoom),
                    launcher: t.launcher.clone(),
                    url: t.committed_url.as_ref().map(|u| u.to_string()).unwrap_or_default(),
                    title: t.title.clone(),
                    name: t.name.clone(),
                    emoji: t.emoji.clone(),
                })
                .collect(),
            active,
        }
    }

    pub fn blocklist(&self) -> &BlockList {
        &self.blocklist
    }

    /// Settings → Blocked changed (or the stored list was loaded): tabs
    /// showing a page that is now blocked are hidden behind the notice at
    /// once. Pages no longer blocked stay behind theirs until reloaded.
    pub fn set_blocklist(&mut self, list: BlockList) -> Vec<Effect> {
        self.blocklist = list;
        let ids: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        for id in ids {
            self.block_if_listed(id);
        }
        let mut out = Vec::new();
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        out
    }

    pub fn adblock_allowlist(&self) -> &BTreeSet<String> {
        &self.adblock_allowed
    }

    pub fn set_adblock_allowlist(&mut self, sites: BTreeSet<String>) -> Vec<Effect> {
        self.adblock_allowed = sites;
        vec![Effect::NotifyChrome]
    }

    pub fn set_adblock_status(&mut self, status: AdblockStatus) -> Vec<Effect> {
        self.adblock_status = status;
        vec![Effect::NotifyChrome]
    }

    pub fn view(&self) -> ChromeState {
        ChromeState {
            tabs: self.tabs.iter().map(|t| tab_view(t, &self.adblock_allowed)).collect(),
            active: self.active,
            os: self.os,
            content_visible: !self.shown.is_empty(),
            split: self.split_view(),
            splits: self.splits.clone(),
            sheet: self.front_sheet().map(|(s, _)| s),
            max_live_tabs: self.budget.max_live,
            adblock: self.adblock_status.clone(),
        }
    }
}
