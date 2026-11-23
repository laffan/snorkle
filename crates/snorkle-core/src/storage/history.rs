//! Opening the store; history, saved page text and downloads.

use super::*;

impl Store {
    /// Opens (or creates) the store. Corrupt files are moved aside to
    /// `<name>.corrupt` and replaced with empty data.
    pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        for old in OLD_ICONS_DIRS {
            let _ = fs::remove_dir_all(dir.join(old));
        }
        let _ = fs::remove_file(dir.join(OLD_HEADING_RULES_FILE));
        let mut history: Vec<HistoryEntry> = load_or_reset(&dir.join(HISTORY_FILE));
        history.retain_mut(|h| match storable_url(&h.url) {
            Some(u) => {
                h.url = u;
                h.title = clean_title(&h.title);
                true
            }
            None => false,
        });
        history.truncate(MAX_HISTORY);
        let mut bookmarks: Vec<Bookmark> = load_or_reset(&dir.join(BOOKMARKS_FILE));
        bookmarks.retain_mut(|b| match storable_url(&b.url) {
            Some(u) => {
                b.url = u;
                b.title = clean_title(&b.title);
                true
            }
            None => false,
        });
        bookmarks.truncate(MAX_BOOKMARKS);
        let adblock_allowed: BTreeSet<String> = load_or_reset::<BTreeSet<String>>(&dir.join(ADBLOCK_ALLOW_FILE))
            .iter()
            .filter_map(|s| crate::blocking::normalise_site(s))
            .take(MAX_BOOKMARKS)
            .collect();
        let mut user_scripts: Vec<UserScript> = load_or_reset::<Vec<UserScript>>(&dir.join(USER_SCRIPTS_FILE))
            .into_iter()
            .filter_map(|s| userscripts::validate(s).ok())
            .collect();
        user_scripts.truncate(userscripts::MAX_SCRIPTS);
        let mut pins: Vec<Bookmark> = load_or_reset(&dir.join(PINS_FILE));
        pins.retain_mut(|b| match storable_url(&b.url) {
            Some(u) => {
                b.url = u;
                b.title = clean_title(&b.title);
                true
            }
            None => false,
        });
        pins.truncate(MAX_PINS);
        let mut ai_usage: Vec<AiUsageEvent> = load_or_reset::<Vec<AiUsageEvent>>(&dir.join(AI_USAGE_FILE))
            .into_iter()
            .filter(|e| crate::ai::validate_model(&e.model).is_ok())
            .collect();
        let excess = ai_usage.len().saturating_sub(MAX_AI_USAGE_EVENTS);
        ai_usage.drain(..excess);
        let chats = load_chats(&dir.join(CHATS_DIR));
        let launch_modes = load_or_reset(&dir.join(LAUNCH_MODES_FILE));
        let blocked =
            crate::blocklist::BlockList::from_settings(load_or_reset(&dir.join(BLOCKED_FILE))).settings().clone();
        let mut pin_emoji: BTreeMap<String, String> = load_or_reset(&dir.join(PIN_EMOJI_FILE));
        pin_emoji.retain(|url, e| {
            (pins.iter().any(|p| &p.url == url) || bookmarks.iter().any(|b| &b.url == url))
                && clean_emoji(e).as_deref() == Some(e.as_str())
        });
        let offline = load_offline(&dir.join(OFFLINE_DIR), |u| {
            pins.iter().any(|p| p.url == u) || bookmarks.iter().any(|b| b.url == u)
        });
        let page_texts = load_page_texts(&dir.join(PAGE_TEXT_DIR), |u| history.iter().any(|h| h.url == u));
        let downloads = load_downloads(&dir);
        let next_download = downloads.iter().map(|d| d.id).max().unwrap_or(0) + 1;
        Ok(Store {
            later: crate::later::LaterStore::open(dir.join("later"))?,
            notebook_lock: Mutex::new(()),
            dir,
            inner: Mutex::new(Data {
                history,
                bookmarks,
                adblock_allowed,
                user_scripts,
                ai_usage,
                pins,
                chats,
                launch_modes,
                blocked,
                pin_emoji,
                offline,
                page_texts,
                downloads,
                next_download,
            }),
        })
    }

    pub(super) fn data(&self) -> std::sync::MutexGuard<'_, Data> {
        // A poisoned lock means a panic mid-update; with panic=abort this is
        // unreachable, but recover the data rather than propagating.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn add_history(&self, url: &str, title: &str, now_ms: u64) -> std::io::Result<()> {
        let Some(url) = storable_url(url) else { return Ok(()) };
        let snapshot = {
            let mut d = self.data();
            if d.history.first().map(|h| h.url == url).unwrap_or(false) {
                let first = &mut d.history[0];
                first.title = clean_title(title);
                first.visited_ms = now_ms;
            } else {
                d.history.insert(0, HistoryEntry { url, title: clean_title(title), visited_ms: now_ms });
                d.history.truncate(MAX_HISTORY);
            }
            serde_json::to_vec(&d.history)?
        };
        atomic_write(&self.dir.join(HISTORY_FILE), &snapshot)
    }

    pub fn history(&self, limit: usize) -> Vec<HistoryEntry> {
        self.data().history.iter().take(limit).cloned().collect()
    }

    pub fn clear_history(&self) -> std::io::Result<()> {
        {
            let mut d = self.data();
            d.history.clear();
            d.page_texts.clear();
        }
        let _ = fs::remove_dir_all(self.dir.join(PAGE_TEXT_DIR));
        atomic_write(&self.dir.join(HISTORY_FILE), b"[]")
    }

    /// Whether `url` (normalised) is in history.
    pub fn in_history(&self, url: &str) -> bool {
        storable_url(url).is_some_and(|u| self.data().history.iter().any(|h| h.url == u))
    }

    /// Removes the visits `(url, visited_ms)` from history; a page with no
    /// visits left also loses its saved text. Returns how many were removed.
    pub fn remove_history(&self, visits: &[(String, u64)]) -> std::io::Result<usize> {
        let (snapshot, index, dropped, removed) = {
            let mut d = self.data();
            let before = d.history.len();
            d.history.retain(|h| !visits.iter().any(|(u, ms)| *u == h.url && *ms == h.visited_ms));
            let removed = before - d.history.len();
            if removed == 0 {
                return Ok(0);
            }
            let Data { history, page_texts, .. } = &mut *d;
            let dropped: Vec<String> = page_texts
                .iter()
                .filter(|e| !history.iter().any(|h| h.url == e.url))
                .map(|e| e.file.clone())
                .collect();
            page_texts.retain(|e| !dropped.contains(&e.file));
            let index = (!dropped.is_empty()).then(|| serde_json::to_vec(&d.page_texts)).transpose()?;
            (serde_json::to_vec(&d.history)?, index, dropped, removed)
        };
        atomic_write(&self.dir.join(HISTORY_FILE), &snapshot)?;
        let dir = self.dir.join(PAGE_TEXT_DIR);
        for f in dropped {
            let _ = fs::remove_file(dir.join(f));
        }
        if let Some(index) = index {
            atomic_write(&dir.join(OFFLINE_INDEX), &index)?;
        }
        Ok(removed)
    }

    /// Saves the text of a page in history (replacing its previous text)
    /// and deletes the oldest beyond [`MAX_PAGE_TEXTS`]. False when `url`
    /// is not in history or there is no text.
    pub fn save_page_text(&self, url: &str, title: &str, text: &str, now_ms: u64) -> std::io::Result<bool> {
        let Some(url) = storable_url(url) else { return Ok(false) };
        let text = crate::ai::clean_multiline(text, MAX_PAGE_TEXT_STORED_CHARS);
        if text.is_empty() {
            return Ok(false);
        }
        let page = SavedPageText { url: url.clone(), title: clean_title(title), text, saved_ms: now_ms };
        let bytes = serde_json::to_vec(&page)?;
        if bytes.len() as u64 > MAX_PAGE_TEXT_FILE_BYTES {
            return Ok(false);
        }
        let dir = self.dir.join(PAGE_TEXT_DIR);
        let file = page_text_file(&url);
        let (index, evicted) = {
            let mut d = self.data();
            if !d.history.iter().any(|h| h.url == url) {
                return Ok(false);
            }
            fs::create_dir_all(&dir)?;
            atomic_write(&dir.join(&file), &bytes)?;
            d.page_texts.retain(|e| e.url != url && e.file != file);
            d.page_texts.insert(0, PageTextEntry { url, file, saved_ms: now_ms, bytes: bytes.len() as u64 });
            let evicted: Vec<String> = d.page_texts.iter().skip(MAX_PAGE_TEXTS).map(|e| e.file.clone()).collect();
            d.page_texts.truncate(MAX_PAGE_TEXTS);
            (serde_json::to_vec(&d.page_texts)?, evicted)
        };
        for f in evicted {
            let _ = fs::remove_file(dir.join(f));
        }
        atomic_write(&dir.join(OFFLINE_INDEX), &index)?;
        Ok(true)
    }

    /// A new download's id and the path its file goes to (its folder is
    /// created). `suggested` is cleaned ([`clean_download_name`]).
    pub fn new_download(&self, suggested: &str) -> std::io::Result<(u64, String, PathBuf)> {
        let id = {
            let mut d = self.data();
            let id = d.next_download;
            d.next_download += 1;
            id
        };
        let name = clean_download_name(suggested);
        let folder = self.dir.join(DOWNLOADS_DIR).join(id.to_string());
        fs::create_dir_all(&folder)?;
        Ok((id, name.clone(), folder.join(name)))
    }

    /// The folder downloads are kept in (created if missing).
    pub fn downloads_root(&self) -> std::io::Result<PathBuf> {
        let root = self.dir.join(DOWNLOADS_DIR);
        fs::create_dir_all(&root)?;
        Ok(root)
    }

    /// Where download `id`'s file named `name` is (whether or not it exists).
    pub fn download_path(&self, id: u64, name: &str) -> Option<PathBuf> {
        (clean_download_name(name) == name).then(|| self.dir.join(DOWNLOADS_DIR).join(id.to_string()).join(name))
    }

    /// Finished downloads of non-private tabs, newest first.
    pub fn downloads(&self) -> Vec<DownloadRecord> {
        self.data().downloads.clone()
    }

    /// Records a finished download (newest first); beyond
    /// [`MAX_DOWNLOAD_RECORDS`] the oldest are deleted with their files.
    pub fn record_download(&self, mut rec: DownloadRecord) -> std::io::Result<()> {
        rec.name = clean_download_name(&rec.name);
        rec.source = storable_url(&rec.source).unwrap_or_default();
        rec.mime = sanitize_page_text(&rec.mime, 100);
        rec.error = sanitize_page_text(&rec.error, 200);
        let (snapshot, evicted) = {
            let mut d = self.data();
            d.downloads.retain(|r| r.id != rec.id);
            d.downloads.insert(0, rec);
            let evicted: Vec<u64> = d.downloads.iter().skip(MAX_DOWNLOAD_RECORDS).map(|r| r.id).collect();
            d.downloads.truncate(MAX_DOWNLOAD_RECORDS);
            (serde_json::to_vec(&d.downloads)?, evicted)
        };
        for id in evicted {
            let _ = fs::remove_dir_all(self.dir.join(DOWNLOADS_DIR).join(id.to_string()));
        }
        atomic_write(&self.dir.join(DOWNLOADS_FILE), &snapshot)
    }

    /// Deletes a download's file (and its record, if it has one).
    pub fn delete_download(&self, id: u64) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            let before = d.downloads.len();
            d.downloads.retain(|r| r.id != id);
            (d.downloads.len() != before).then(|| serde_json::to_vec(&d.downloads)).transpose()?
        };
        let _ = fs::remove_dir_all(self.dir.join(DOWNLOADS_DIR).join(id.to_string()));
        match snapshot {
            Some(s) => atomic_write(&self.dir.join(DOWNLOADS_FILE), &s),
            None => Ok(()),
        }
    }

    /// Size in bytes of each saved page text, by URL.
    pub fn page_text_sizes(&self) -> BTreeMap<String, u64> {
        self.data().page_texts.iter().map(|e| (e.url.clone(), e.bytes)).collect()
    }

    /// The saved text of `url`, if any.
    pub fn page_text(&self, url: &str) -> Option<SavedPageText> {
        let url = storable_url(url)?;
        let file = self.data().page_texts.iter().find(|e| e.url == url)?.file.clone();
        read_page_text(&self.dir.join(PAGE_TEXT_DIR).join(file)).filter(|p| p.url == url)
    }

    /// Saved pages whose text contains `query` (case-insensitive), newest
    /// first, with a snippet around the first match.
    pub fn search_page_texts(&self, query: &str, limit: usize) -> Vec<TextHit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let files: Vec<PageTextEntry> = self.data().page_texts.clone();
        let dir = self.dir.join(PAGE_TEXT_DIR);
        files
            .iter()
            .filter_map(|e| {
                let p = read_page_text(&dir.join(&e.file)).filter(|p| p.url == e.url)?;
                Some(TextHit { url: e.url.clone(), snippet: snippet(&p.text, &q)? })
            })
            .take(limit)
            .collect()
    }
}
