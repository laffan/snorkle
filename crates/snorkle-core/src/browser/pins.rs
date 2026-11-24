//! SECURITY-REVIEW: pins keep their page (owner request 2026-11-24). A
//! pin's tab stays on the page it shows; going to another page from it
//! opens that page in a new tab (shown at once, at the end of the list),
//! so the pin stays as it was and the new tab can be bookmarked or closed.
//!
//! Three ways out of a pin's page are caught:
//! - a link the user follows (WebKit's `LinkActivated` main-frame
//!   navigation): cancelled, its URL opened in the new tab;
//! - a link the user clicks in a single-page app, which would change the
//!   address without a navigation (`history.pushState`): the page's
//!   watcher (`pinguard::watch_js`, client world, real clicks only) stops
//!   the click and reports the link (channel C25, `on_pin_link`);
//! - any other address change right after a real click or key press in
//!   the page (a route changed from a button's handler): the new address
//!   opens in a new tab and the pin goes back (`on_url_changed`).
//!
//! Not caught, on purpose: redirects and the page's own address changes
//! without a user action (a site moving its pin to its real address, or
//! rewriting it after loading), form submissions (a sign-in form posts
//! there; its data cannot be sent again in another tab), back/forward and
//! reload, changes of the fragment only (`#…`, as Gmail's), and loads the
//! browser starts itself (opening the pin, Return to Original URL).
//!
//! Every page opened this way goes through the navigation policy as a link
//! with a user gesture (as a ⌘-clicked link does); a pin sends at most
//! [`MAX_DIVERTS`] pages a minute.

use super::*;

/// A route change this soon after the page reported a real click or key
/// press is the user's.
pub const GESTURE_WINDOW_MS: u64 = 1_500;
/// Pages one pin's tab may send to new tabs per minute.
pub const MAX_DIVERTS: usize = 10;
const DIVERT_WINDOW_MS: u64 = 60_000;

impl Browser {
    /// The pins' stored URLs changed (or were loaded): tabs that became or
    /// stopped being a pin's have their page's guard turned on or off.
    pub fn set_pins(&mut self, keys: impl IntoIterator<Item = String>) -> Vec<Effect> {
        let before: Vec<TabId> = self.tabs.iter().filter(|t| self.is_pin_tab(t.id)).map(|t| t.id).collect();
        self.pins = keys.into_iter().collect();
        let mut out = Vec::new();
        for t in &self.tabs {
            let now_pin = self.is_pin_tab(t.id);
            if now_pin != before.contains(&t.id) && t.life == Life::Live {
                out.push(Effect::PinGuard { tab: t.id, on: now_pin });
            }
        }
        out
    }

    /// Whether `key` (a stored URL) is a pin.
    pub fn is_pin(&self, key: &str) -> bool {
        self.pins.contains(key)
    }

    /// A pin's own tab: it keeps its page.
    pub fn is_pin_tab(&self, id: TabId) -> bool {
        self.tab(id).is_some_and(|t| {
            !t.is_private()
                && !t.sheet
                && t.saved.is_none()
                && t.launcher.as_ref().is_some_and(|k| self.pins.contains(k))
        })
    }

    /// The guard's state for a tab whose page just committed or finished.
    pub(super) fn pin_guard_effect(&self, id: TabId) -> Option<Effect> {
        self.is_pin_tab(id).then_some(Effect::PinGuard { tab: id, on: true })
    }

    /// Whether going to `url` leaves the page the tab shows: another
    /// http(s) address once the fragment is set aside.
    pub(super) fn leaves_page(&self, id: TabId, url: &Url) -> bool {
        let Some(shown) = self.tab(id).and_then(|t| t.committed_url.as_ref()) else { return false };
        matches!(url.scheme(), "http" | "https") && without_fragment(shown) != without_fragment(url)
    }

    /// The pin's page reported a real click or key press (C25).
    pub fn on_pin_gesture(&mut self, id: TabId, now: u64) {
        if self.is_pin_tab(id) {
            if let Some(t) = self.tab_mut(id) {
                t.gesture_ms = Some(now);
            }
        }
    }

    /// The pin's page stopped a real click on a link to `url` (C25): it
    /// opens in a new tab if it leaves the pin's page. Anything else is
    /// ignored (the click is lost; the page was told the guard is on).
    pub fn on_pin_link(&mut self, id: TabId, url: &Url, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        if !self.is_pin_tab(id) || !self.leaves_page(id, url) {
            return out;
        }
        if self.divert_from_pin(id, url, now, &mut out).is_some() {
            self.sync_visibility(&mut out);
            out.push(Effect::NotifyChrome);
        }
        out
    }

    /// Opens `url` from pin tab `id` in a new tab of the same profile and
    /// session at the end of the list and shows it; Back there returns to
    /// the pin's page. `None` when refused (the policy, the rate limit).
    pub(super) fn divert_from_pin(&mut self, id: TabId, url: &Url, now: u64, out: &mut Vec<Effect>) -> Option<TabId> {
        let t = self.tab_mut(id)?;
        t.diverted_ms.retain(|at| now.saturating_sub(*at) < DIVERT_WINDOW_MS);
        if t.diverted_ms.len() >= MAX_DIVERTS {
            return None;
        }
        t.diverted_ms.push(now);
        t.gesture_ms = None;
        let (fresh, effects) = self.on_background_tab_request(id, url, now);
        out.extend(effects);
        let fresh = fresh?;
        // At the end of the list (a pin's tab has no place in it).
        if let Some(at) = self.tabs.iter().position(|t| t.id == fresh) {
            let t = self.tabs.remove(at);
            self.tabs.push(t);
        }
        self.activate_into(fresh, now, out);
        Some(fresh)
    }

    /// A same-document address change in a pin's tab (already accepted):
    /// right after a real click or key press it is the user leaving the
    /// page (a single-page app's route), so the new address opens in a new
    /// tab and the pin goes back to `previous`.
    pub(super) fn pin_route_changed(&mut self, id: TabId, previous: Url, now: u64, out: &mut Vec<Effect>) {
        let Some(t) = self.tab(id) else { return };
        let recent = t.gesture_ms.is_some_and(|g| now.saturating_sub(g) <= GESTURE_WINDOW_MS);
        let Some(url) = t.committed_url.clone() else { return };
        if !recent || !self.is_pin_tab(id) || without_fragment(&previous) == without_fragment(&url) {
            return;
        }
        let can_go_back = t.can_go_back;
        let Some(fresh) = self.divert_from_pin(id, &url, now, out) else { return };
        // Back in the new tab returns to the pin's page, not to the route.
        if let Some(t) = self.tab_mut(fresh) {
            t.back_to = Some(previous.clone()).filter(|u| matches!(u.scheme(), "http" | "https"));
        }
        // The pin's page goes back to where it was: Back undoes the
        // route's history entry (the app shows its previous view); with no
        // entry to undo, the page is loaded again.
        if can_go_back {
            out.push(Effect::GoBack(id));
        } else {
            out.extend(self.navigate_trusted(id, previous, now));
        }
    }
}
