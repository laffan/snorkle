//! The History preview (owner request 2026-11-24): a page shown inside the
//! chrome's History dialog instead of its saved text.
//!
//! At most one preview tab exists. It is a tab of the default profile
//! (never private), loaded through the navigation policy like an address
//! typed by the user, and it is never listed with the other tabs, saved in
//! the session, reopened by ⌘⇧T, written to history or made active. While
//! it can show its page it is the only view on screen
//! (`Pane::Preview`, in the rectangle the chrome reports for the dialog's
//! body); the chrome hides everything else with its overlay. Choosing
//! another tab closes it.

use super::*;

impl Browser {
    /// The preview tab, if one is open.
    pub fn preview_tab(&self) -> Option<TabId> {
        self.tabs.iter().find(|t| t.preview).map(|t| t.id)
    }

    /// Opens `url` (http/https) in the preview, replacing any preview open.
    /// `None` when browsing is not allowed or the address is not a web
    /// page.
    pub fn open_preview(&mut self, url: Url, now: u64) -> (Option<TabId>, Vec<Effect>) {
        let mut out = Vec::new();
        self.drop_preview(&mut out);
        if !self.os.allows_browsing() || !matches!(url.scheme(), "http" | "https") {
            return (None, out);
        }
        let id = self.alloc_id();
        let mut tab = Tab::new(id, Profile::Default, now);
        tab.preview = true;
        self.tabs.push(tab);
        out.extend(self.navigate_trusted(id, url, now));
        self.enforce_budget(now, &mut out);
        self.sync_visibility(&mut out);
        out.push(Effect::NotifyChrome);
        (Some(id), out)
    }

    /// Closes the preview, if one is open.
    pub fn close_preview(&mut self) -> Vec<Effect> {
        let mut out = Vec::new();
        if self.drop_preview(&mut out) {
            self.sync_visibility(&mut out);
            out.push(Effect::NotifyChrome);
        }
        out
    }

    /// Removes the preview tab without the bookkeeping of an ordinary
    /// close: nothing to reopen, no session change, no other tab chosen.
    /// Windows its page opened stay, as ordinary tabs. Whether there was
    /// one.
    pub(super) fn drop_preview(&mut self, out: &mut Vec<Effect>) -> bool {
        let Some(id) = self.preview_tab() else { return false };
        for t in self.tabs.iter_mut().filter(|t| t.opener == Some(id)) {
            t.opener = None;
            t.sheet = false;
        }
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else { return false };
        let tab = self.tabs.remove(i);
        self.shown.retain(|(v, _)| *v != id);
        if tab.life == Life::Live {
            out.push(Effect::DestroyView { tab: id, keep_state: false });
        }
        true
    }
}
