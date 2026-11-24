//! Tab groups (owner request 2026-11-24): tabs chosen together in the
//! sidebar (⇧- or ⌘-click) and grouped from their menu, under a name the
//! user can change; a group folds away. A group's tabs are of one profile
//! and one session (everyday or a research session) and stay next to each
//! other in the list. Groups of bookmarks are kept by the store
//! (`storage::BookmarkGroup`), not here.

use super::*;

/// A tab group: its id (its tabs name it), name and whether it is folded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TabGroup {
    pub id: u64,
    pub name: String,
    pub collapsed: bool,
}

/// Longest group name kept (characters).
pub const MAX_GROUP_NAME_CHARS: usize = 60;

/// A group name as typed or stored: one line, cleaned like a page title,
/// at most [`MAX_GROUP_NAME_CHARS`]; "Group" when nothing is left.
pub fn clean_group_name(name: &str) -> String {
    let n = sanitize_page_text(name, MAX_GROUP_NAME_CHARS);
    let n = n.trim();
    if n.is_empty() {
        "Group".to_string()
    } else {
        n.to_string()
    }
}

/// Tabs that can be in a group: listed ones (not a pin's or bookmark's,
/// not a popup sheet or a saved copy).
fn groupable(t: &Tab) -> bool {
    t.launcher.is_none() && !t.sheet && t.saved.is_none()
}

impl Browser {
    pub fn groups(&self) -> &[TabGroup] {
        &self.groups
    }

    /// Groups `ids` under `name`: those that can be grouped and share the
    /// first one's profile and session, in list order, gathered where the
    /// first one is. They leave any group they were in. Returns the new
    /// group, or `None` when none of them can be grouped.
    pub fn group_tabs(&mut self, ids: &[TabId], name: &str) -> Option<(u64, Vec<Effect>)> {
        let first = ids.iter().find_map(|id| self.tab(*id).filter(|t| groupable(t)))?;
        let (profile, space) = (first.profile, first.space.clone());
        let members: Vec<TabId> = self
            .tabs
            .iter()
            .filter(|t| ids.contains(&t.id) && groupable(t) && t.profile == profile && t.space == space)
            .map(|t| t.id)
            .collect();
        let id = self.next_group;
        self.next_group += 1;
        self.groups.push(TabGroup { id, name: clean_group_name(name), collapsed: false });
        for t in self.tabs.iter_mut().filter(|t| members.contains(&t.id)) {
            t.group = Some(id);
        }
        self.compact_groups();
        self.prune_groups();
        Some((id, self.group_changed(profile)))
    }

    /// The group's tabs stay where they are, ungrouped.
    pub fn ungroup(&mut self, group: u64) -> Vec<Effect> {
        let Some(profile) = self.group_profile(group) else { return self.drop_group(group) };
        for t in self.tabs.iter_mut().filter(|t| t.group == Some(group)) {
            t.group = None;
        }
        self.prune_groups();
        self.group_changed(profile)
    }

    /// Takes `ids` out of their groups; each leaves its group's run of
    /// tabs (it goes just after it).
    pub fn ungroup_tabs(&mut self, ids: &[TabId]) -> Vec<Effect> {
        let mut private = true;
        let mut changed = false;
        for id in ids {
            let Some(g) = self.tab(*id).and_then(|t| t.group) else { continue };
            let Some(from) = self.tabs.iter().position(|t| t.id == *id) else { continue };
            let mut t = self.tabs.remove(from);
            private &= t.is_private();
            t.group = None;
            let at = self.tabs.iter().rposition(|x| x.group == Some(g)).map_or(from, |j| j + 1);
            self.tabs.insert(at.min(self.tabs.len()), t);
            changed = true;
        }
        if !changed {
            return Vec::new();
        }
        self.prune_groups();
        self.group_changed(if private { Profile::Private } else { Profile::Default })
    }

    pub fn rename_group(&mut self, group: u64, name: &str) -> Vec<Effect> {
        let name = clean_group_name(name);
        let Some(g) = self.groups.iter_mut().find(|g| g.id == group) else { return Vec::new() };
        if g.name == name {
            return Vec::new();
        }
        g.name = name;
        let profile = self.group_profile(group).unwrap_or(Profile::Default);
        self.group_changed(profile)
    }

    pub fn set_group_collapsed(&mut self, group: u64, collapsed: bool) -> Vec<Effect> {
        let Some(g) = self.groups.iter_mut().find(|g| g.id == group) else { return Vec::new() };
        if g.collapsed == collapsed {
            return Vec::new();
        }
        g.collapsed = collapsed;
        let profile = self.group_profile(group).unwrap_or(Profile::Default);
        self.group_changed(profile)
    }

    /// The tabs of a group, in list order.
    pub fn group_members(&self, group: u64) -> Vec<TabId> {
        self.tabs.iter().filter(|t| t.group == Some(group)).map(|t| t.id).collect()
    }

    /// Puts a moved tab into `group` (dropped next to one of its tabs) or
    /// out of any (`None`), if the group is of the tab's profile and
    /// session; then keeps every group's tabs together.
    pub(super) fn regroup_moved(&mut self, id: TabId, group: Option<u64>) {
        let Some((profile, space)) = self.tab(id).filter(|t| groupable(t)).map(|t| (t.profile, t.space.clone())) else {
            return;
        };
        let fits = group.is_none_or(|g| {
            self.groups.iter().any(|x| x.id == g)
                && self
                    .tabs
                    .iter()
                    .any(|t| t.id != id && t.group == Some(g) && t.profile == profile && t.space == space)
        });
        if fits {
            if let Some(t) = self.tab_mut(id) {
                t.group = group;
            }
        }
        self.compact_groups();
        self.prune_groups();
    }

    /// Keeps each group's tabs next to each other: they gather where the
    /// first of them is, in their order; other tabs keep theirs.
    pub(super) fn compact_groups(&mut self) {
        let mut order: Vec<Tab> = Vec::with_capacity(self.tabs.len());
        let mut rest = std::mem::take(&mut self.tabs);
        while !rest.is_empty() {
            let t = rest.remove(0);
            match t.group {
                Some(g) => {
                    order.push(t);
                    let mut i = 0;
                    while i < rest.len() {
                        if rest[i].group == Some(g) {
                            order.push(rest.remove(i));
                        } else {
                            i += 1;
                        }
                    }
                }
                None => order.push(t),
            }
        }
        self.tabs = order;
    }

    /// Drops groups without tabs, and group marks naming no group.
    pub(super) fn prune_groups(&mut self) {
        let known: BTreeSet<u64> = self.groups.iter().map(|g| g.id).collect();
        for t in self.tabs.iter_mut() {
            if t.group.is_some_and(|g| !known.contains(&g)) || (t.group.is_some() && !groupable(t)) {
                t.group = None;
            }
        }
        let used: BTreeSet<u64> = self.tabs.iter().filter_map(|t| t.group).collect();
        self.groups.retain(|g| used.contains(&g.id));
    }

    fn drop_group(&mut self, group: u64) -> Vec<Effect> {
        let before = self.groups.len();
        self.groups.retain(|g| g.id != group);
        if self.groups.len() == before {
            Vec::new()
        } else {
            vec![Effect::NotifyChrome]
        }
    }

    fn group_profile(&self, group: u64) -> Option<Profile> {
        self.tabs.iter().find(|t| t.group == Some(group)).map(|t| t.profile)
    }

    /// A group changed: saved unless it is of private tabs.
    fn group_changed(&self, profile: Profile) -> Vec<Effect> {
        let mut out = Vec::new();
        if profile == Profile::Default {
            out.push(Effect::PersistSession(self.session()));
        }
        out.push(Effect::NotifyChrome);
        out
    }

    /// Restores the saved groups that have restored tabs (`ids` maps saved
    /// group ids to the ones given now).
    pub(super) fn restore_groups(&mut self, session: &Session) -> std::collections::BTreeMap<u64, u64> {
        let mut ids = std::collections::BTreeMap::new();
        for g in &session.groups {
            if ids.contains_key(&g.id) || self.groups.len() >= MAX_SESSION_GROUPS {
                continue;
            }
            let id = self.next_group;
            self.next_group += 1;
            ids.insert(g.id, id);
            self.groups.push(TabGroup { id, name: clean_group_name(&g.name), collapsed: g.collapsed });
        }
        ids
    }
}

/// Most groups restored from a session.
const MAX_SESSION_GROUPS: usize = 500;
