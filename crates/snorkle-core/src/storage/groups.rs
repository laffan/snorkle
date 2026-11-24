//! Groups of bookmarks (2026-11-24), research sessions' notebooks, and
//! favicons' dark-mode copies.

use super::*;

/// A group of bookmarks (`bookmark-groups.json`): its bookmarks name it by
/// `id` and stay next to each other in the bookmarks bar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookmarkGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub collapsed: bool,
}

pub(super) const BOOKMARK_GROUPS_FILE: &str = "bookmark-groups.json";
/// Research sessions' notebooks, one file each (`<session id>.json`).
const NOTEBOOKS_DIR: &str = "notebooks";
/// Favicons' copies for the dark appearance, in the icons folder.
const DARK_ICONS_DIR: &str = "dark";
/// A notebook being exported (2026-11-24): the one file the save panel or
/// Files picker copies from; replaced by the next export.
const EXPORTS_DIR: &str = "exports";

/// A bookmark group id: `g` and 1–20 digits.
pub fn valid_bookmark_group_id(id: &str) -> bool {
    id.len() >= 2 && id.len() <= 21 && id.starts_with('g') && id[1..].bytes().all(|b| b.is_ascii_digit())
}

/// Keeps each group's bookmarks together (where the first of them is),
/// drops groups without bookmarks and group marks naming no group.
pub(super) fn tidy_bookmark_groups(bookmarks: &mut Vec<Bookmark>, groups: &mut Vec<BookmarkGroup>) {
    for b in bookmarks.iter_mut() {
        if b.group.as_ref().is_some_and(|g| !groups.iter().any(|x| &x.id == g)) {
            b.group = None;
        }
    }
    let mut order: Vec<Bookmark> = Vec::with_capacity(bookmarks.len());
    let mut rest = std::mem::take(bookmarks);
    while !rest.is_empty() {
        let b = rest.remove(0);
        let g = b.group.clone();
        order.push(b);
        if let Some(g) = g {
            let mut i = 0;
            while i < rest.len() {
                if rest[i].group.as_ref() == Some(&g) {
                    order.push(rest.remove(i));
                } else {
                    i += 1;
                }
            }
        }
    }
    *bookmarks = order;
    groups.retain(|g| bookmarks.iter().any(|b| b.group.as_ref() == Some(&g.id)));
}

impl Store {
    pub fn bookmark_groups(&self) -> Vec<BookmarkGroup> {
        self.data().bookmark_groups.clone()
    }

    /// Writes the bookmarks and their groups after `f` changed them (kept
    /// together, empty groups dropped). Returns what `f` returned.
    pub(super) fn change_bookmarks<T>(&self, f: impl FnOnce(&mut Data) -> T) -> std::io::Result<T> {
        let (result, bookmarks, groups) = {
            let mut d = self.data();
            let result = f(&mut d);
            let Data { bookmarks, bookmark_groups, .. } = &mut *d;
            tidy_bookmark_groups(bookmarks, bookmark_groups);
            (result, serde_json::to_vec(&d.bookmarks)?, serde_json::to_vec(&d.bookmark_groups)?)
        };
        atomic_write(&self.dir.join(BOOKMARKS_FILE), &bookmarks)?;
        atomic_write(&self.dir.join(BOOKMARK_GROUPS_FILE), &groups)?;
        Ok(result)
    }

    /// Groups the bookmarks of `urls` under `name` (cleaned as a tab
    /// group's), gathered where the first of them is; they leave their
    /// groups. Returns the new group's id, or `None` when none is a
    /// bookmark.
    pub fn group_bookmarks(&self, urls: &[String], name: &str, now_ms: u64) -> std::io::Result<Option<String>> {
        self.change_bookmarks(|d| {
            if !d.bookmarks.iter().any(|b| urls.contains(&b.url)) || d.bookmark_groups.len() >= MAX_BOOKMARKS {
                return None;
            }
            let mut n = now_ms;
            let id = loop {
                let id = format!("g{n}");
                if !d.bookmark_groups.iter().any(|g| g.id == id) {
                    break id;
                }
                n += 1;
            };
            d.bookmark_groups.push(BookmarkGroup {
                id: id.clone(),
                name: crate::browser::clean_group_name(name),
                collapsed: false,
            });
            for b in d.bookmarks.iter_mut().filter(|b| urls.contains(&b.url)) {
                b.group = Some(id.clone());
            }
            Some(id)
        })
    }

    pub fn rename_bookmark_group(&self, id: &str, name: &str) -> std::io::Result<bool> {
        self.change_bookmarks(|d| match d.bookmark_groups.iter_mut().find(|g| g.id == id) {
            Some(g) => {
                g.name = crate::browser::clean_group_name(name);
                true
            }
            None => false,
        })
    }

    pub fn set_bookmark_group_collapsed(&self, id: &str, collapsed: bool) -> std::io::Result<bool> {
        self.change_bookmarks(|d| match d.bookmark_groups.iter_mut().find(|g| g.id == id) {
            Some(g) => {
                g.collapsed = collapsed;
                true
            }
            None => false,
        })
    }

    /// The group goes; its bookmarks stay where they are.
    pub fn ungroup_bookmarks(&self, id: &str) -> std::io::Result<bool> {
        self.change_bookmarks(|d| {
            let had = d.bookmark_groups.iter().any(|g| g.id == id);
            d.bookmark_groups.retain(|g| g.id != id);
            had
        })
    }

    /// Puts a bookmark into a group (`Some`, an existing one) or out of
    /// any, at `index` (clamped) among the bookmarks.
    pub fn place_bookmark(&self, url: &str, index: usize, group: Option<&str>) -> std::io::Result<bool> {
        self.change_bookmarks(|d| {
            if group.is_some_and(|g| !d.bookmark_groups.iter().any(|x| x.id == g)) {
                return false;
            }
            let Some(from) = d.bookmarks.iter().position(|b| b.url == url) else { return false };
            let mut b = d.bookmarks.remove(from);
            b.group = group.map(str::to_string);
            let at = index.min(d.bookmarks.len());
            d.bookmarks.insert(at, b);
            true
        })
    }

    /// Removes the bookmarks of `urls` (and so any group left empty), and
    /// their emoji; returns those removed.
    pub fn remove_bookmarks(&self, urls: &[String]) -> std::io::Result<Vec<String>> {
        let removed = self.change_bookmarks(|d| {
            let removed: Vec<String> =
                d.bookmarks.iter().filter(|b| urls.contains(&b.url)).map(|b| b.url.clone()).collect();
            d.bookmarks.retain(|b| !urls.contains(&b.url));
            removed
        })?;
        self.prune_item_emoji()?;
        Ok(removed)
    }

    /// The bookmarks of a group, in order.
    pub fn bookmark_group_members(&self, id: &str) -> Vec<String> {
        self.data().bookmarks.iter().filter(|b| b.group.as_deref() == Some(id)).map(|b| b.url.clone()).collect()
    }

    fn notebook_path(&self, space: Option<&str>) -> Option<PathBuf> {
        match space {
            None => Some(self.dir.join(NOTEBOOK_FILE)),
            Some(id) if crate::browser::valid_research_id(id) => {
                Some(self.dir.join(NOTEBOOKS_DIR).join(format!("{id}.json")))
            }
            Some(_) => None,
        }
    }

    /// The notebook of everyday browsing (`None`, `notebook.json`) or of a
    /// research session (`notebooks/<id>.json`), checked again as it is
    /// read (`notebook::clean`); `None` when there is none.
    pub fn notebook_in(&self, space: Option<&str>) -> Option<String> {
        let bytes = fs::read(self.notebook_path(space)?).ok()?;
        if bytes.len() > crate::notebook::MAX_NOTEBOOK_BYTES {
            return None;
        }
        crate::notebook::clean(std::str::from_utf8(&bytes).ok()?)
    }

    /// Writes a session's notebook as the chrome sends it, checked and
    /// rebuilt. `Ok(false)`: refused (not a notebook, too large, not a
    /// research session id).
    pub fn set_notebook_in(&self, space: Option<&str>, json: &str) -> std::io::Result<bool> {
        let Some(path) = self.notebook_path(space) else { return Ok(false) };
        let Some(clean) = crate::notebook::clean(json) else { return Ok(false) };
        let _one = self.notebook_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        atomic_write(&path, clean.as_bytes())?;
        Ok(true)
    }

    /// Deletes a research session's notebook (the session was deleted).
    pub fn delete_notebook_in(&self, space: &str) -> std::io::Result<()> {
        let Some(path) = self.notebook_path(Some(space)).filter(|_| crate::browser::valid_research_id(space)) else {
            return Ok(());
        };
        let _one = self.notebook_lock.lock().unwrap_or_else(|e| e.into_inner());
        match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Writes a file to be exported (a notebook's page) under a cleaned
    /// name (`clean_download_name`) in the app's own `exports/` folder,
    /// which keeps only this one, and returns its path.
    pub fn export_file(&self, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let dir = self.dir.join(EXPORTS_DIR);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        let path = dir.join(clean_download_name(name));
        atomic_write(&path, bytes)?;
        Ok(path)
    }

    /// The favicon's copy for the dark appearance (made when the icon is
    /// too dark to see there; `favicon::dark_variant`), if there is one.
    pub fn icon_dark(&self, host: &str) -> Option<Vec<u8>> {
        let host = crate::blocking::normalise_site(host)?;
        let bytes = fs::read(self.dir.join(ICONS_DIR).join(DARK_ICONS_DIR).join(format!("{host}.png"))).ok()?;
        is_png(&bytes).then_some(bytes)
    }

    /// Stores a favicon and its dark copy (or removes an old copy). Both
    /// are PNGs the favicon fetcher drew; anything else is refused.
    pub fn save_icons(&self, host: &str, png: &[u8], dark: Option<&[u8]>) -> std::io::Result<bool> {
        let Some(host) = crate::blocking::normalise_site(host) else { return Ok(false) };
        if !is_png(png) || dark.is_some_and(|d| !is_png(d)) {
            return Ok(false);
        }
        let dir = self.dir.join(ICONS_DIR);
        let dark_dir = dir.join(DARK_ICONS_DIR);
        fs::create_dir_all(&dark_dir)?;
        let dark_file = dark_dir.join(format!("{host}.png"));
        match dark {
            Some(d) => atomic_write(&dark_file, d)?,
            None => {
                let _ = fs::remove_file(&dark_file);
            }
        }
        atomic_write(&dir.join(format!("{host}.png")), png)?;
        Ok(true)
    }
}
