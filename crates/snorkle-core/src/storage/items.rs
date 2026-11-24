//! The notebook, Save for Later, bookmarks, pins, offline copies, emoji and
//! icons.

use super::*;

impl Store {
    /// Save for Later.
    /// The Notebook (`notebook.json`, owner request 2026-11-15), checked
    /// again as it is read (`notebook::clean`); `None` when there is none
    /// or it cannot be read.
    pub fn notebook(&self) -> Option<String> {
        self.notebook_in(None)
    }

    /// Writes the Notebook as the chrome sends it, checked and rebuilt
    /// (`notebook::clean`). `Ok(false)`: refused (not a notebook, too
    /// large). Writes are serialised (one temporary file).
    pub fn set_notebook(&self, json: &str) -> std::io::Result<bool> {
        self.set_notebook_in(None, json)
    }

    pub fn later(&self) -> &crate::later::LaterStore {
        &self.later
    }

    /// Adds a bookmark; returns false if the URL is not storable or already
    /// bookmarked.
    pub fn add_bookmark(&self, url: &str, title: &str, now_ms: u64) -> std::io::Result<bool> {
        self.insert_bookmark_in(url, title, usize::MAX, None, now_ms)
    }

    /// Bookmarks `url` at position `index` (clamped). Returns false if the
    /// URL is not storable or already bookmarked.
    pub fn insert_bookmark(&self, url: &str, title: &str, index: usize, now_ms: u64) -> std::io::Result<bool> {
        self.insert_bookmark_in(url, title, index, None, now_ms)
    }

    /// Bookmarks `url` at `index` (clamped), in `group` if that group
    /// exists (2026-11-24). False if the URL is not storable or already
    /// bookmarked.
    pub fn insert_bookmark_in(&self, url: &str, title: &str, index: usize, group: Option<&str>, now_ms: u64) -> std::io::Result<bool> {
        let Some(url) = storable_url(url) else { return Ok(false) };
        self.change_bookmarks(|d| {
            if d.bookmarks.iter().any(|b| b.url == url) || d.bookmarks.len() >= MAX_BOOKMARKS {
                return false;
            }
            let group = group.filter(|g| d.bookmark_groups.iter().any(|x| x.id == *g)).map(str::to_string);
            let at = index.min(d.bookmarks.len());
            d.bookmarks.insert(at, Bookmark { url, title: clean_title(title), created_ms: now_ms, group });
            true
        })
    }

    /// Changes the URL of a pin or bookmark ("Edit URL"), keeping its title,
    /// position and emoji; its offline copy is deleted (it was of the old
    /// page). Returns the stored (normalised) new URL, or `None` when `old`
    /// is neither pinned nor bookmarked, `new` is not an http(s) URL, or
    /// it is already pinned or bookmarked.
    pub fn change_item_url(&self, old: &str, new: &str) -> std::io::Result<Option<String>> {
        self.edit_item(old, new, None)
    }

    /// Edits a saved item using the same URL rules, with an optional title
    /// sanitised like other bookmark titles. Title-only edits keep offline data.
    pub fn edit_item(&self, old: &str, new: &str, title: Option<&str>) -> std::io::Result<Option<String>> {
        let Some(new) = storable_url(new) else { return Ok(None) };
        let url_changed = new != old;
        let (pins, bookmarks, emoji, offline, dropped) = {
            let mut d = self.data();
            let taken = url_changed && d.pins.iter().chain(d.bookmarks.iter()).any(|b| b.url == new);
            let in_pins = d.pins.iter().position(|b| b.url == old);
            let in_bookmarks = d.bookmarks.iter().position(|b| b.url == old);
            if taken || (in_pins.is_none() && in_bookmarks.is_none()) {
                return Ok(None);
            }
            if let Some(i) = in_pins {
                d.pins[i].url = new.clone();
                if let Some(title) = title {
                    d.pins[i].title = clean_title(title);
                }
            }
            if let Some(i) = in_bookmarks {
                d.bookmarks[i].url = new.clone();
                if let Some(title) = title {
                    d.bookmarks[i].title = clean_title(title);
                }
            }
            let emoji = match url_changed.then(|| d.pin_emoji.remove(old)).flatten() {
                Some(e) => {
                    d.pin_emoji.insert(new.clone(), e);
                    Some(serde_json::to_vec(&d.pin_emoji)?)
                }
                None => None,
            };
            let dropped: Vec<String> =
                d.offline.iter().filter(|e| url_changed && e.url == old).map(|e| e.file.clone()).collect();
            d.offline.retain(|e| !url_changed || e.url != old);
            let offline = (!dropped.is_empty()).then(|| serde_json::to_vec(&d.offline)).transpose()?;
            (
                in_pins.map(|_| serde_json::to_vec(&d.pins)).transpose()?,
                in_bookmarks.map(|_| serde_json::to_vec(&d.bookmarks)).transpose()?,
                emoji,
                offline,
                dropped,
            )
        };
        if let Some(p) = pins {
            atomic_write(&self.dir.join(PINS_FILE), &p)?;
        }
        if let Some(b) = bookmarks {
            atomic_write(&self.dir.join(BOOKMARKS_FILE), &b)?;
        }
        if let Some(e) = emoji {
            atomic_write(&self.dir.join(PIN_EMOJI_FILE), &e)?;
        }
        if let Some(o) = offline {
            let dir = self.dir.join(OFFLINE_DIR);
            for f in dropped {
                let _ = fs::remove_file(dir.join(f));
            }
            atomic_write(&dir.join(OFFLINE_INDEX), &o)?;
        }
        Ok(Some(new))
    }

    /// Moves the bookmark for `url` to position `index` (clamped).
    /// (Its group, if any, stays together: see [`Store::place_bookmark`].)
    pub fn move_bookmark(&self, url: &str, index: usize) -> std::io::Result<()> {
        let group = self.data().bookmarks.iter().find(|b| b.url == url).and_then(|b| b.group.clone());
        self.place_bookmark(url, index, group.as_deref()).map(|_| ())
    }

    pub fn pins(&self) -> Vec<Bookmark> {
        self.data().pins.clone()
    }

    /// Pins `url` at `index` (clamped). False if not storable, already
    /// pinned, or the grid is full.
    pub fn insert_pin(&self, url: &str, title: &str, index: usize, now_ms: u64) -> std::io::Result<bool> {
        let Some(url) = storable_url(url) else { return Ok(false) };
        let snapshot = {
            let mut d = self.data();
            if d.pins.iter().any(|b| b.url == url) || d.pins.len() >= MAX_PINS {
                return Ok(false);
            }
            let at = index.min(d.pins.len());
            d.pins.insert(at, Bookmark { url, title: clean_title(title), created_ms: now_ms, group: None });
            serde_json::to_vec(&d.pins)?
        };
        atomic_write(&self.dir.join(PINS_FILE), &snapshot)?;
        Ok(true)
    }

    pub fn move_pin(&self, url: &str, index: usize) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            let Some(from) = d.pins.iter().position(|b| b.url == url) else { return Ok(()) };
            let b = d.pins.remove(from);
            let at = index.min(d.pins.len());
            d.pins.insert(at, b);
            serde_json::to_vec(&d.pins)?
        };
        atomic_write(&self.dir.join(PINS_FILE), &snapshot)
    }

    /// Removes a pin and returns it. Its emoji is kept, so a pin moved into
    /// the bookmarks keeps it; [`Store::prune_item_emoji`] drops it when the
    /// pin was removed for good.
    pub fn remove_pin(&self, url: &str) -> std::io::Result<Option<Bookmark>> {
        let (removed, snapshot) = {
            let mut d = self.data();
            let removed = d.pins.iter().position(|b| b.url == url).map(|i| d.pins.remove(i));
            (removed, serde_json::to_vec(&d.pins)?)
        };
        atomic_write(&self.dir.join(PINS_FILE), &snapshot)?;
        Ok(removed)
    }

    /// Drops the emoji of addresses that are neither pinned nor bookmarked.
    pub fn prune_item_emoji(&self) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            let before = d.pin_emoji.len();
            let Data { pin_emoji, pins, bookmarks, .. } = &mut *d;
            pin_emoji.retain(|u, _| pins.iter().any(|p| &p.url == u) || bookmarks.iter().any(|b| &b.url == u));
            if pin_emoji.len() == before {
                return Ok(());
            }
            serde_json::to_vec(&d.pin_emoji)?
        };
        atomic_write(&self.dir.join(PIN_EMOJI_FILE), &snapshot)
    }

    /// Whether `url` (a committed page URL) is pinned or bookmarked and its
    /// offline copy is missing or older than [`OFFLINE_REFRESH_MS`].
    pub fn offline_copy_wanted(&self, url: &str, now_ms: u64) -> bool {
        let d = self.data();
        let saved = d.pins.iter().any(|p| p.url == url) || d.bookmarks.iter().any(|b| b.url == url);
        saved
            && d.offline
                .iter()
                .find(|e| e.url == url)
                .is_none_or(|e| now_ms.saturating_sub(e.saved_ms) >= OFFLINE_REFRESH_MS)
    }

    /// Stores the offline copy of a pinned or bookmarked page, replacing
    /// its previous one; deletes the oldest copies beyond
    /// [`MAX_OFFLINE_TOTAL_BYTES`]. False when `url` is neither pinned nor
    /// bookmarked or the copy is empty or too large.
    pub fn save_offline_copy(&self, url: &str, data: &[u8], now_ms: u64) -> std::io::Result<bool> {
        if data.is_empty() || data.len() > MAX_OFFLINE_COPY_BYTES {
            return Ok(false);
        }
        let dir = self.dir.join(OFFLINE_DIR);
        let file = offline_file(url);
        let (index, evicted) = {
            let mut d = self.data();
            if !(d.pins.iter().any(|p| p.url == url) || d.bookmarks.iter().any(|b| b.url == url)) {
                return Ok(false);
            }
            fs::create_dir_all(&dir)?;
            atomic_write(&dir.join(&file), data)?;
            d.offline.retain(|e| e.url != url && e.file != file);
            d.offline.push(OfflineEntry { url: url.to_string(), file, saved_ms: now_ms, bytes: data.len() as u64 });
            d.offline.sort_by_key(|e| std::cmp::Reverse(e.saved_ms));
            let mut total = 0u64;
            let mut evicted = Vec::new();
            d.offline.retain(|e| {
                total += e.bytes;
                let keep = total <= MAX_OFFLINE_TOTAL_BYTES;
                if !keep {
                    evicted.push(e.file.clone());
                }
                keep
            });
            (serde_json::to_vec(&d.offline)?, evicted)
        };
        for f in evicted {
            let _ = fs::remove_file(dir.join(f));
        }
        atomic_write(&dir.join(OFFLINE_INDEX), &index)?;
        Ok(true)
    }

    /// The offline copy of `url` and when it was saved, if there is one and
    /// the page is still pinned or bookmarked.
    pub fn offline_copy(&self, url: &str) -> Option<(Vec<u8>, u64)> {
        let entry = {
            let d = self.data();
            let saved = d.pins.iter().any(|p| p.url == url) || d.bookmarks.iter().any(|b| b.url == url);
            d.offline.iter().find(|e| e.url == url && saved).cloned()?
        };
        let path = self.dir.join(OFFLINE_DIR).join(&entry.file);
        if fs::metadata(&path).ok()?.len() > MAX_OFFLINE_COPY_BYTES as u64 {
            return None;
        }
        Some((fs::read(path).ok()?, entry.saved_ms))
    }

    /// Deletes every offline copy (Settings → Cache).
    pub fn clear_offline_copies(&self) {
        self.data().offline.clear();
        let _ = fs::remove_dir_all(self.dir.join(OFFLINE_DIR));
    }

    /// The emoji shown instead of a pin's or bookmark's favicon, if one was
    /// chosen.
    pub fn pin_emoji(&self, url: &str) -> Option<String> {
        self.data().pin_emoji.get(url).cloned()
    }

    /// Sets (or with `None` removes) a pin's or bookmark's emoji icon (the
    /// Edit dialog, 2026-11-09; pins only before). False when `url` is
    /// neither pinned nor bookmarked or the emoji is not acceptable
    /// ([`clean_emoji`]).
    pub fn set_pin_emoji(&self, url: &str, emoji: Option<&str>) -> std::io::Result<bool> {
        let snapshot = {
            let mut d = self.data();
            if !d.pins.iter().any(|p| p.url == url) && !d.bookmarks.iter().any(|b| b.url == url) {
                return Ok(false);
            }
            match emoji {
                Some(e) => {
                    let Some(e) = clean_emoji(e) else { return Ok(false) };
                    d.pin_emoji.insert(url.to_string(), e);
                }
                None => {
                    d.pin_emoji.remove(url);
                }
            }
            serde_json::to_vec(&d.pin_emoji)?
        };
        atomic_write(&self.dir.join(PIN_EMOJI_FILE), &snapshot)?;
        Ok(true)
    }

    /// Removes a bookmark and returns it (to move it into the pinned grid).
    pub fn take_bookmark(&self, url: &str) -> std::io::Result<Option<Bookmark>> {
        let removed = self.change_bookmarks(|d| d.bookmarks.iter().position(|b| b.url == url).map(|i| d.bookmarks.remove(i)))?;
        Ok(removed.map(|b| Bookmark { group: None, ..b }))
    }

    /// Cached favicon PNG for `host` (a validated ASCII host), if any.
    pub fn icon(&self, host: &str) -> Option<Vec<u8>> {
        let host = crate::blocking::normalise_site(host)?;
        let bytes = fs::read(self.dir.join(ICONS_DIR).join(format!("{host}.png"))).ok()?;
        is_png(&bytes).then_some(bytes)
    }

    /// Stores a favicon PNG produced by the favicon fetcher. Rejects
    /// anything that is not a PNG or is too large.
    pub fn save_icon(&self, host: &str, png: &[u8]) -> std::io::Result<bool> {
        self.save_icons(host, png, None)
    }
}
