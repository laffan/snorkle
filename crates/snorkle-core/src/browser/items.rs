//! Closing and reopening tabs; tabs of pins and bookmarks (launchers); view
//! exemptions, page scale, offline and saved copies; tab labels and order.

use super::*;

impl Browser {
    pub fn close(&mut self, id: TabId, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        // Popup sheets go with the tab they cover.
        let sheets: Vec<TabId> = self.tabs.iter().filter(|t| t.sheet && t.opener == Some(id)).map(|t| t.id).collect();
        for s in sheets {
            out.extend(self.close(s, now));
        }
        let Some(idx) = self.tabs.iter().position(|t| t.id == id) else { return out };
        let tab = self.tabs.remove(idx);
        // A saved copy's tab is not reopened by ⌘⇧T (it was never an
        // ordinary tab).
        if !tab.is_private() && !tab.sheet && tab.saved.is_none() {
            if let Some(url) = tab.committed_url.clone().filter(|u| matches!(u.scheme(), "http" | "https")) {
                self.closed.retain(|c| *c != url);
                self.closed.push(url);
                let excess = self.closed.len().saturating_sub(MAX_CLOSED_TABS);
                self.closed.drain(..excess);
            }
        }
        self.shown.retain(|(t, _)| *t != id);
        let partner = self.pair_of(id).map(|s| if s.left == id { s.right } else { s.left });
        self.splits.retain(|s| s.left != id && s.right != id);
        if tab.life == Life::Live {
            out.push(Effect::DestroyView { tab: id, keep_state: false });
        }
        if tab.is_private() && !self.tabs.iter().any(|t| t.is_private()) {
            self.http_ok_private.clear();
            out.push(Effect::ReleasePrivateData);
        }
        if self.active == Some(id) {
            self.active = None;
            // A closing split half leaves the other half on screen (full
            // width), not the tab after it in the list; a closing popup
            // returns to its opener.
            let find = |want: Option<TabId>| want.and_then(|w| self.tabs.iter().find(|t| t.id == w));
            let next = find(partner)
                .or_else(|| find(tab.opener))
                .or_else(|| self.tabs.get(idx))
                .or_else(|| idx.checked_sub(1).and_then(|i| self.tabs.get(i)));
            match next.map(|t| t.id) {
                Some(n) => self.activate_into(n, now, &mut out),
                None => {
                    self.new_tab_into(Profile::Default, now, &mut out);
                }
            }
        }
        // A closed split partner or sheet leaves the remaining view in the
        // wrong place unless visibility is recomputed.
        self.sync_visibility(&mut out);
        if !tab.is_private() {
            out.push(Effect::PersistSession(self.session()));
        }
        out.push(Effect::NotifyChrome);
        out
    }

    /// The page of the most recently closed tab not yet reopened (and
    /// forgets it). The caller skips pages removed from history and asks
    /// again.
    pub fn pop_closed(&mut self) -> Option<Url> {
        self.closed.pop()
    }

    /// Reopens a closed tab's page (⌘⇧T) in a new default-profile tab,
    /// through the navigation policy.
    pub fn reopen_closed(&mut self, url: Url, now: u64) -> (Option<TabId>, Vec<Effect>) {
        if !self.os.allows_browsing() {
            return (None, Vec::new());
        }
        let mut out = Vec::new();
        let id = self.new_tab_into(Profile::Default, now, &mut out);
        out.extend(self.navigate_trusted(id, url, now));
        out.push(Effect::NotifyChrome);
        (Some(id), out)
    }

    /// A link opened from another app (Snorkle as the default browser): a
    /// new tab in the default profile, through the normal navigation
    /// policy. Only plain http(s) URLs without credentials are accepted.
    pub fn open_from_other_app(&mut self, url: Url, now: u64) -> (Option<TabId>, Vec<Effect>) {
        let plain = matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.as_str().len() <= input::MAX_INPUT_LEN;
        if !plain || !self.os.allows_browsing() {
            return (None, Vec::new());
        }
        let mut out = Vec::new();
        let id = self.new_tab_into(Profile::Default, now, &mut out);
        out.extend(self.navigate_trusted(id, url, now));
        out.push(Effect::NotifyChrome);
        (Some(id), out)
    }

    /// Opens a pin or bookmark: returns to the tab that belongs to it as
    /// the user left it (2026-11-03; before, it always went back to the
    /// item's own page), or creates one (default profile) and loads `url`.
    /// With `reset` ("Return to Original URL"), the item's tab loads `url`
    /// unless it already shows it. A tab with no page loads `url` either
    /// way. `key` is the pin's or bookmark's stored URL.
    pub fn open_launcher(&mut self, key: &str, url: Url, reset: bool, now: u64) -> (TabId, Vec<Effect>) {
        let existing = self.tabs.iter().find(|t| t.launcher.as_deref() == Some(key) && !t.is_private()).map(|t| t.id);
        if let Some(id) = existing {
            let mut out = self.activate(id, now);
            let shown = self.tab(id).and_then(|t| t.committed_url.clone());
            if shown.is_none() || (reset && shown.as_ref() != Some(&url)) {
                out.extend(self.navigate_trusted(id, url, now));
                if let Some(t) = self.tab_mut(id) {
                    t.landing = Some(key.to_string());
                }
                out.push(Effect::NotifyChrome);
            }
            return (id, out);
        }
        let mut out = Vec::new();
        let id = self.new_tab_into(Profile::Default, now, &mut out);
        if let Some(t) = self.tab_mut(id) {
            t.launcher = Some(key.to_string());
            t.landing = Some(key.to_string());
        }
        out.extend(self.navigate_trusted(id, url, now));
        out.push(Effect::NotifyChrome);
        (id, out)
    }

    /// A tab was bookmarked or pinned as `key`: it becomes that item's tab
    /// (moved out of the tab list, not duplicated) unless another tab
    /// already belongs to `key`, it already belongs to an item, or it is
    /// private (never persisted). Returns whether it was bound.
    pub fn bind_launcher(&mut self, id: TabId, key: &str) -> (bool, Vec<Effect>) {
        let taken = self.tabs.iter().any(|t| t.launcher.as_deref() == Some(key));
        let ok = !taken
            && self.tab(id).is_some_and(|t| t.launcher.is_none() && !t.is_private() && !t.sheet && t.opener.is_none());
        if !ok {
            return (false, Vec::new());
        }
        if let Some(t) = self.tab_mut(id) {
            t.launcher = Some(key.to_string());
        }
        (true, vec![Effect::PersistSession(self.session()), Effect::NotifyChrome])
    }

    /// The view modes turned off for a tab ("Disable for this tab"). Saved
    /// with the session (not for private tabs, which are never saved).
    pub fn set_view_exempt(&mut self, id: TabId, kinds: &[crate::viewmodes::Kind]) -> Vec<Effect> {
        let kinds = clean_exempt(kinds);
        let Some(t) = self.tab_mut(id) else { return Vec::new() };
        if t.view_exempt == kinds {
            return Vec::new();
        }
        t.view_exempt = kinds;
        let private = t.is_private();
        if private { Vec::new() } else { vec![Effect::PersistSession(self.session())] }
    }

    /// Changes a tab's page scale (⋯ menu). Saved with the session (not for
    /// private tabs); a tab without a web view gets it when one is created.
    pub fn zoom(&mut self, id: TabId, step: crate::tabs::ZoomStep) -> Vec<Effect> {
        let Some(t) = self.tab_mut(id) else { return Vec::new() };
        let percent = step.apply(t.zoom);
        if percent == t.zoom {
            return Vec::new();
        }
        t.zoom = percent;
        let mut out = Vec::new();
        if t.life == Life::Live {
            out.push(Effect::SetZoom { tab: id, percent });
        }
        if !t.is_private() {
            out.push(Effect::PersistSession(self.session()));
        }
        out.push(Effect::NotifyChrome);
        out
    }

    /// The network failed for `url` in `id` and an offline copy of it
    /// (saved at `saved_ms`) exists: the host is about to load the copy
    /// (with JavaScript off) in place of the error. Only for a live,
    /// non-private tab whose failed load was its main page, and never for a
    /// copy that itself failed. Clears the notice and expects the copy's
    /// load (its base URL is `url`) as the browser's own. Returns false when
    /// the copy must not be used.
    pub fn use_offline_copy(&mut self, id: TabId, url: &Url, saved_ms: u64) -> (bool, Vec<Effect>) {
        let mut out = Vec::new();
        let Some(t) = self.tab_mut(id) else { return (false, out) };
        let failed_here =
            matches!(&t.notice, Some(Notice::LoadError { certificate: false, url: u, .. }) if u == url.as_str());
        if t.life != Life::Live || t.is_private() || t.offline.is_some() || t.offline_tried || !failed_here {
            return (false, out);
        }
        t.notice = None;
        t.loading = true;
        t.expected_load = Some((url.clone(), Source::Typed));
        t.offline = Some(crate::tabs::OfflineCopy { saved_ms, committed: false });
        t.offline_tried = true;
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        (true, out)
    }

    /// The pin or bookmark URL whose opening started the tab's current load
    /// (see `Tab::landing`).
    pub fn landing(&self, id: TabId) -> Option<String> {
        self.tab(id).and_then(|t| t.landing.clone())
    }

    /// Whether the tab shows an offline copy or a Save for Later archive
    /// (its page is not saved again, nor its text read for history).
    pub fn shows_offline_copy(&self, id: TabId) -> bool {
        self.tab(id).is_some_and(|t| t.offline.is_some() || t.saved.is_some())
    }

    /// Shows a Save for Later archive in a non-private tab (owner request
    /// 2026-11-09): the platform loads `copy.page` from the archive right
    /// after these effects, as the browser's own load (its base URL is the
    /// page's). Creates the tab's web view if it has none, without loading
    /// anything into it. The copy is not written to history.
    pub fn show_saved_copy(&mut self, id: TabId, copy: crate::tabs::SavedCopy) -> Vec<Effect> {
        let mut out = Vec::new();
        if !self.os.allows_browsing() {
            return out;
        }
        let Some(t) = self.tab_mut(id) else { return out };
        if t.is_private() || t.sheet {
            return out;
        }
        let page = copy.page.clone();
        t.notice = None;
        t.offline = None;
        t.https = HttpsUpgrade::default();
        t.loading = true;
        t.progress = 0;
        t.saved = Some(copy);
        if t.life == Life::Live {
            t.expected_load = Some((page, Source::Typed));
        } else {
            self.ensure_live(id, AfterCreate::Restore { fallback: None }, Some((page, Source::Typed)), &mut out);
        }
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        out
    }

    /// The tab showing saved item `item`, if one is open.
    pub fn saved_tab(&self, item: &str) -> Option<TabId> {
        self.tabs.iter().find(|t| t.saved.as_ref().is_some_and(|c| c.item == item)).map(|t| t.id)
    }

    /// Closes the tabs showing saved item `item` (it was removed from the
    /// list: recovered, bookmarked or deleted).
    pub fn close_saved(&mut self, item: &str, now: u64) -> Vec<Effect> {
        let ids: Vec<TabId> =
            self.tabs.iter().filter(|t| t.saved.as_ref().is_some_and(|c| c.item == item)).map(|t| t.id).collect();
        let mut out = Vec::new();
        for id in ids {
            out.extend(self.close(id, now));
        }
        out
    }

    /// Adding pages: the chosen pages load live in the saved copy's tab
    /// (`on`) and are captured; the tab shows the archive again after.
    pub fn set_adding(&mut self, id: TabId, on: bool) {
        if let Some(c) = self.tab_mut(id).and_then(|t| t.saved.as_mut()) {
            c.adding = on;
        }
    }

    /// The Save for Later archive a tab shows, if any.
    pub fn saved_copy(&self, id: TabId) -> Option<&crate::tabs::SavedCopy> {
        self.tab(id).and_then(|t| t.saved.as_ref())
    }

    /// Adding pages to a saved item: starts (with no link chosen) or ends
    /// choosing links in the tab showing its archive.
    pub fn set_picking(&mut self, id: TabId, on: bool) -> Vec<Effect> {
        let Some(c) = self.tab_mut(id).and_then(|t| t.saved.as_mut()) else { return Vec::new() };
        c.picking = on.then(Vec::new);
        vec![Effect::NotifyChrome]
    }

    /// Ends choosing links and returns the item and the chosen links.
    pub fn take_picks(&mut self, id: TabId) -> Option<(String, Vec<Url>)> {
        let c = self.tab_mut(id)?.saved.as_mut()?;
        let picks = c.picking.take()?;
        Some((c.item.clone(), picks))
    }

    /// A main-frame navigation in a tab showing a Save for Later archive:
    /// `Some` when the archive handles it instead of the network. While
    /// adding pages, a link to another page of the same host toggles its
    /// choice and nothing navigates; otherwise a link to another page the
    /// archive holds opens that page from the archive. Back/forward is left
    /// to WebKit.
    pub(super) fn saved_copy_navigation(&mut self, id: TabId, url: &Url, kind: NavKind, now: u64) -> Option<(Verdict, Vec<Effect>)> {
        let blocked = self.blocklist.matches(url);
        let t = self.tab_mut(id)?;
        let c = t.saved.as_mut().filter(|c| c.committed && !c.adding)?;
        let bare = without_fragment(url);
        if let Some(picks) = c.picking.as_mut() {
            let host = c.pages.first().and_then(|p| p.host_str().map(str::to_string));
            let eligible = kind == NavKind::LinkActivated
                && matches!(bare.scheme(), "http" | "https")
                && bare.host_str().map(str::to_string) == host
                && !c.pages.contains(&bare);
            if eligible {
                if let Some(at) = picks.iter().position(|p| *p == bare) {
                    picks.remove(at);
                } else if c.pages.len() + picks.len() < crate::later::MAX_PAGES {
                    picks.push(bare);
                }
            }
            return Some((Verdict::Cancel, vec![Effect::NotifyChrome]));
        }
        if kind == NavKind::BackForward || bare == c.page {
            return None;
        }
        if c.pages.contains(&bare) && !blocked {
            return Some((Verdict::Cancel, vec![Effect::OpenSavedPage { tab: id, item: c.item.clone(), url: bare }]));
        }
        // Leaving the archive: a link or form opens in a new ordinary tab
        // (through the policy, as a ⌘-click would) and becomes the active
        // tab; anything else (a script or redirect) goes nowhere.
        if !matches!(kind, NavKind::LinkActivated | NavKind::FormSubmitted) {
            return Some((Verdict::Cancel, Vec::new()));
        }
        let (fresh, mut out) = self.on_background_tab_request(id, url, now);
        if let Some(fresh) = fresh {
            out.extend(self.activate(fresh, now));
        }
        Some((Verdict::Cancel, out))
    }

    /// Tabs with view-mode exemptions (restored from the session).
    pub fn view_exemptions(&self) -> Vec<(TabId, Vec<crate::viewmodes::Kind>)> {
        self.tabs.iter().filter(|t| !t.view_exempt.is_empty()).map(|t| (t.id, t.view_exempt.clone())).collect()
    }

    /// The pin or bookmark `key` was removed: its tab becomes an ordinary
    /// tab (listed again), so nothing the user had open is lost.
    pub fn release_launcher(&mut self, key: &str) -> Vec<Effect> {
        let mut changed = false;
        for t in self.tabs.iter_mut().filter(|t| t.launcher.as_deref() == Some(key)) {
            t.launcher = None;
            changed = true;
        }
        if !changed {
            return Vec::new();
        }
        vec![Effect::PersistSession(self.session()), Effect::NotifyChrome]
    }

    /// A pin's or bookmark's URL changed from `old` to `new` ("Edit URL"):
    /// its tab now belongs to the new key.
    pub fn rekey_launcher(&mut self, old: &str, new: &str) -> Vec<Effect> {
        let mut changed = false;
        for t in self.tabs.iter_mut().filter(|t| t.launcher.as_deref() == Some(old)) {
            t.launcher = Some(new.to_string());
            changed = true;
        }
        if !changed {
            return Vec::new();
        }
        vec![Effect::PersistSession(self.session()), Effect::NotifyChrome]
    }

    /// The Edit dialog (owner request 2026-11-09): the tab's own name,
    /// shown instead of its page's title, and an emoji shown instead of its
    /// favicon. `None` or an empty name clears it. Err when the emoji is
    /// not acceptable (`storage::clean_emoji`) or the tab is gone.
    pub fn set_tab_label(&mut self, id: TabId, name: Option<&str>, emoji: Option<&str>) -> Result<Vec<Effect>, &'static str> {
        let emoji = match emoji.map(str::trim).filter(|e| !e.is_empty()) {
            Some(e) => Some(crate::storage::clean_emoji(e).ok_or("Use one emoji (no spaces).")?),
            None => None,
        };
        let t = self.tab_mut(id).ok_or("That tab is gone.")?;
        t.name = name.and_then(clean_tab_name);
        t.emoji = emoji;
        let private = t.is_private();
        let mut out = Vec::new();
        if !private {
            out.push(Effect::PersistSession(self.session()));
        }
        out.push(Effect::NotifyChrome);
        Ok(out)
    }

    /// Dragging a tab in the sidebar (owner request 2026-11-09): moves `id`
    /// just before `before`, or to the end of the list. Only the order
    /// changes (pairs, pins' and bookmarks' tabs stay as they are); private
    /// and other tabs are listed separately, so their relative order does
    /// not matter.
    pub fn move_tab(&mut self, id: TabId, before: Option<TabId>) -> Vec<Effect> {
        if before == Some(id) {
            return Vec::new();
        }
        let Some(from) = self.tabs.iter().position(|t| t.id == id) else { return Vec::new() };
        if before.is_some_and(|b| !self.tabs.iter().any(|t| t.id == b)) {
            return Vec::new();
        }
        let tab = self.tabs.remove(from);
        let at = before.and_then(|b| self.tabs.iter().position(|t| t.id == b)).unwrap_or(self.tabs.len());
        let private = tab.is_private();
        self.tabs.insert(at, tab);
        let mut out = Vec::new();
        if !private {
            out.push(Effect::PersistSession(self.session()));
        }
        out.push(Effect::NotifyChrome);
        out
    }
}
