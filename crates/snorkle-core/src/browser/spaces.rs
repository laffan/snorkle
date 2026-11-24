//! Research sessions (owner request 2026-11-24): a session is a fresh set
//! of tabs with its own notebook (the chrome hides the pins and bookmarks
//! while one is shown). Every tab belongs to everyday browsing (`space:
//! None`) or to one research session; the session shown is the active
//! tab's, so choosing a tab of another session switches to that session.
//! Tabs are sent between sessions from their menu. Pins' and bookmarks'
//! own tabs stay in everyday browsing: sending one sends a copy (a new,
//! unloaded tab with its page).

use super::*;

/// A research session: its id (`r` and digits, also the name of its
/// notebook file), the topic the user gave it, and when it was made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Research {
    pub id: String,
    pub name: String,
    pub created_ms: u64,
}

/// A research session as the chrome lists it.
#[derive(Clone, Debug, Serialize)]
pub struct ResearchView {
    pub id: String,
    pub name: String,
    /// Its tabs (as listed: not popup sheets or saved copies).
    pub tabs: usize,
}

/// Most research sessions kept.
pub const MAX_RESEARCH: usize = 50;
/// Longest topic kept (characters).
pub const MAX_RESEARCH_NAME_CHARS: usize = 80;

/// A research session id: `r` and 1–20 digits (it is also a file name).
pub fn valid_research_id(id: &str) -> bool {
    id.len() >= 2 && id.len() <= 21 && id.starts_with('r') && id[1..].bytes().all(|b| b.is_ascii_digit())
}

/// A topic as typed or stored: cleaned like a page title, one line, at
/// most [`MAX_RESEARCH_NAME_CHARS`]; "Research" when nothing is left.
pub fn clean_research_name(name: &str) -> String {
    let n = sanitize_page_text(name, MAX_RESEARCH_NAME_CHARS);
    let n = n.trim();
    if n.is_empty() {
        "Research".to_string()
    } else {
        n.to_string()
    }
}

impl Browser {
    /// The research session shown: the active tab's (`None`: everyday).
    pub fn space(&self) -> Option<&str> {
        self.active.and_then(|a| self.tab(a)).and_then(|t| t.space.as_deref())
    }

    pub fn research(&self) -> &[Research] {
        &self.research
    }

    pub(super) fn research_exists(&self, space: Option<&str>) -> bool {
        space.is_none_or(|s| self.research.iter().any(|r| r.id == s))
    }

    /// A new research session about `name`, entered at once with an empty
    /// tab (the start page). `created_ms` is wall-clock time (the id is
    /// made from it, so ids stay unique across launches); `now` the core's
    /// clock. Err when there are already [`MAX_RESEARCH`].
    pub fn new_research(
        &mut self,
        name: &str,
        created_ms: u64,
        now: u64,
    ) -> Result<(String, Vec<Effect>), &'static str> {
        if self.research.len() >= MAX_RESEARCH {
            return Err("There are too many research sessions. Delete one first.");
        }
        let mut n = created_ms;
        let id = loop {
            let id = format!("r{n}");
            if !self.research.iter().any(|r| r.id == id) {
                break id;
            }
            n += 1;
        };
        self.research.push(Research { id: id.clone(), name: clean_research_name(name), created_ms });
        let mut out = Vec::new();
        let tab = self.alloc_id();
        let mut t = Tab::new(tab, Profile::Default, now);
        t.space = Some(id.clone());
        self.tabs.push(t);
        self.activate_into(tab, now, &mut out);
        out.push(Effect::PersistSession(self.session()));
        out.push(Effect::NotifyChrome);
        Ok((id, out))
    }

    pub fn rename_research(&mut self, id: &str, name: &str) -> Vec<Effect> {
        let Some(r) = self.research.iter_mut().find(|r| r.id == id) else { return Vec::new() };
        let name = clean_research_name(name);
        if r.name == name {
            return Vec::new();
        }
        r.name = name;
        vec![Effect::PersistSession(self.session()), Effect::NotifyChrome]
    }

    /// Deletes a research session and closes its tabs (everyday browsing
    /// is shown if it was). Its notebook is the caller's to delete.
    pub fn delete_research(&mut self, id: &str, now: u64) -> Vec<Effect> {
        if !self.research.iter().any(|r| r.id == id) {
            return Vec::new();
        }
        let mut out = Vec::new();
        if self.space() == Some(id) {
            out.extend(self.enter_space(None, now));
        }
        self.research.retain(|r| r.id != id);
        let ids: Vec<TabId> = self.tabs.iter().filter(|t| t.space.as_deref() == Some(id)).map(|t| t.id).collect();
        for t in ids {
            out.extend(self.close(t, now));
        }
        out.push(Effect::PersistSession(self.session()));
        out.push(Effect::NotifyChrome);
        out
    }

    /// Shows everyday browsing (`None`) or a research session: the tab of
    /// it used last (a pin's or bookmark's included), or a new empty one.
    pub fn enter_space(&mut self, space: Option<&str>, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        if !self.research_exists(space) {
            return out;
        }
        if self.space() == space && self.active.is_some() {
            out.push(Effect::NotifyChrome);
            return out;
        }
        let last = self
            .tabs
            .iter()
            .filter(|t| t.space.as_deref() == space && !t.sheet && t.saved.is_none())
            .max_by_key(|t| t.last_active_ms)
            .map(|t| t.id);
        match last {
            Some(id) => self.activate_into(id, now, &mut out),
            None => {
                self.new_tab_in(Profile::Default, space.map(str::to_string), now, &mut out);
            }
        }
        out.push(Effect::NotifyChrome);
        out
    }

    /// Sends tabs to everyday browsing (`None`) or a research session:
    /// they leave their pair and group and go to the end of the list there.
    /// Pins' and bookmarks' tabs, popup sheets and saved copies are not
    /// sent (a sheet goes with its opener). When the tab shown is sent
    /// away, the session shown stays: its last-used tab is shown, or a
    /// new one.
    pub fn send_tabs(&mut self, ids: &[TabId], to: Option<&str>, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        if !self.research_exists(to) {
            return out;
        }
        let shown = self.space().map(str::to_string);
        let movable = |t: &Tab| t.launcher.is_none() && !t.sheet && t.saved.is_none() && t.space.as_deref() != to;
        let sent: Vec<TabId> = ids.iter().copied().filter(|id| self.tab(*id).is_some_and(movable)).collect();
        if sent.is_empty() {
            return out;
        }
        for id in &sent {
            if self.pair_of(*id).is_some() {
                out.extend(self.unsplit(*id));
            }
            let followers: Vec<TabId> =
                self.tabs.iter().filter(|t| t.id == *id || (t.sheet && t.opener == Some(*id))).map(|t| t.id).collect();
            for f in followers {
                let Some(at) = self.tabs.iter().position(|t| t.id == f) else { continue };
                let mut t = self.tabs.remove(at);
                t.space = to.map(str::to_string);
                t.group = None;
                self.tabs.push(t);
            }
        }
        self.prune_groups();
        let active_left = self.active.and_then(|a| self.tab(a)).is_some_and(|t| t.space != shown);
        if active_left {
            let next = self
                .tabs
                .iter()
                .filter(|t| t.space == shown && !t.sheet && t.saved.is_none())
                .max_by_key(|t| t.last_active_ms)
                .map(|t| t.id);
            match next {
                Some(n) => self.activate_into(n, now, &mut out),
                None => {
                    self.new_tab_in(Profile::Default, shown, now, &mut out);
                }
            }
        }
        self.sync_visibility(&mut out);
        out.push(Effect::PersistSession(self.session()));
        out.push(Effect::NotifyChrome);
        out
    }

    /// Sends a copy of a page (a pin's or bookmark's) to everyday browsing
    /// or a research session: a new default-profile tab at the end of its
    /// list, not loaded until it is chosen (as restored tabs), titled
    /// `title`. Only http(s) URLs. The session shown does not change.
    pub fn send_page(&mut self, url: Url, title: &str, to: Option<&str>, now: u64) -> (Option<TabId>, Vec<Effect>) {
        if !self.research_exists(to) || !matches!(url.scheme(), "http" | "https") || !self.os.allows_browsing() {
            return (None, Vec::new());
        }
        let id = self.alloc_id();
        let mut t = Tab::new(id, Profile::Default, now);
        t.committed_url = Some(url);
        t.title = sanitize_page_text(title, MAX_PAGE_TEXT_CHARS);
        t.space = to.map(str::to_string);
        // Older than the tab shown, so it never counts as the session's
        // last-used tab before it is chosen.
        t.last_active_ms = 0;
        self.tabs.push(t);
        (Some(id), vec![Effect::PersistSession(self.session()), Effect::NotifyChrome])
    }

    /// A new tab of `profile` in `space`, after the active tab when that
    /// tab is in the same session (after its group's last tab), else at
    /// the end of the list; activated.
    pub(super) fn new_tab_in(
        &mut self,
        profile: Profile,
        space: Option<String>,
        now: u64,
        out: &mut Vec<Effect>,
    ) -> TabId {
        let id = self.alloc_id();
        let after = self.active.filter(|a| self.tab(*a).is_some_and(|t| t.space == space));
        let insert_at = after.map(|a| self.index_after(a)).unwrap_or(self.tabs.len());
        let mut t = Tab::new(id, profile, now);
        t.space = space;
        self.tabs.insert(insert_at, t);
        self.activate_into(id, now, out);
        id
    }

    /// The index just after `id` in the list, or after the last tab of its
    /// group (so a tab put there does not split the group).
    pub(super) fn index_after(&self, id: TabId) -> usize {
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else { return self.tabs.len() };
        match self.tabs[i].group {
            Some(g) => self.tabs.iter().rposition(|t| t.group == Some(g)).map_or(i + 1, |j| j + 1),
            None => i + 1,
        }
    }

    /// The research sessions as the chrome lists them.
    pub(super) fn research_views(&self) -> Vec<ResearchView> {
        self.research
            .iter()
            .map(|r| ResearchView {
                id: r.id.clone(),
                name: r.name.clone(),
                tabs: self
                    .tabs
                    .iter()
                    .filter(|t| t.space.as_deref() == Some(r.id.as_str()) && !t.sheet && t.saved.is_none())
                    .count(),
            })
            .collect()
    }

    /// Restores the saved research sessions (valid ids, unique, at most
    /// [`MAX_RESEARCH`]).
    pub(super) fn restore_research(&mut self, session: &Session) {
        for r in &session.research {
            if self.research.len() >= MAX_RESEARCH
                || !valid_research_id(&r.id)
                || self.research.iter().any(|x| x.id == r.id)
            {
                continue;
            }
            self.research.push(Research {
                id: r.id.clone(),
                name: clean_research_name(&r.name),
                created_ms: r.created_ms,
            });
        }
    }
}
