//! Site scripts, chats, AI usage, launch modes, the block list, the
//! session and the ad-block allowlist.

use super::*;

impl Store {
    pub fn user_scripts(&self) -> Vec<UserScript> {
        self.data().user_scripts.clone()
    }

    /// Adds or replaces (by domain) a user script after validation.
    pub fn save_user_script(&self, script: UserScript) -> Result<UserScript, UserScriptError> {
        let script = userscripts::validate(script)?;
        let snapshot = {
            let mut d = self.data();
            match d.user_scripts.iter().position(|s| s.domain == script.domain) {
                Some(i) => d.user_scripts[i] = script.clone(),
                None if d.user_scripts.len() >= userscripts::MAX_SCRIPTS => return Err(UserScriptError::TooMany),
                None => d.user_scripts.push(script.clone()),
            }
            d.user_scripts.sort_by(|a, b| a.domain.cmp(&b.domain));
            serde_json::to_vec(&d.user_scripts).map_err(|_| UserScriptError::TooLarge)?
        };
        atomic_write(&self.dir.join(USER_SCRIPTS_FILE), &snapshot).map_err(|_| UserScriptError::TooLarge)?;
        Ok(script)
    }

    /// Archived chats, most recently used first.
    pub fn chats(&self) -> Vec<ChatRecord> {
        let mut c = self.data().chats.clone();
        c.sort_by_key(|c| std::cmp::Reverse(c.updated_ms));
        c
    }

    pub fn chat(&self, id: &str) -> Option<ChatRecord> {
        self.data().chats.iter().find(|c| c.id == id).cloned()
    }

    /// Stores (or replaces) a chat; the least recently used beyond
    /// [`MAX_CHATS`] are deleted. Returns whether it was stored.
    pub fn save_chat(&self, chat: ChatRecord) -> std::io::Result<bool> {
        let Some(chat) = validate_chat(chat) else { return Ok(false) };
        let dir = self.dir.join(CHATS_DIR);
        fs::create_dir_all(&dir)?;
        let bytes = serde_json::to_vec(&chat)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Ok(false);
        }
        let dropped: Vec<String> = {
            let mut d = self.data();
            d.chats.retain(|c| c.id != chat.id);
            d.chats.push(chat.clone());
            d.chats.sort_by_key(|c| std::cmp::Reverse(c.updated_ms));
            let keep = MAX_CHATS.min(d.chats.len());
            d.chats.drain(keep..).map(|c| c.id).collect()
        };
        atomic_write(&dir.join(format!("{}.json", chat.id)), &bytes)?;
        for id in dropped {
            let _ = fs::remove_file(dir.join(format!("{id}.json")));
        }
        Ok(true)
    }

    pub fn delete_chat(&self, id: &str) -> std::io::Result<()> {
        if !valid_chat_id(id) {
            return Ok(());
        }
        self.data().chats.retain(|c| c.id != id);
        match fs::remove_file(self.dir.join(CHATS_DIR).join(format!("{id}.json"))) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    pub fn ai_usage(&self) -> Vec<AiUsageEvent> {
        self.data().ai_usage.clone()
    }

    pub fn record_ai_usage(&self, event: AiUsageEvent) -> std::io::Result<()> {
        if crate::ai::validate_model(&event.model).is_err() {
            return Ok(());
        }
        let snapshot = {
            let mut d = self.data();
            d.ai_usage.push(event);
            let excess = d.ai_usage.len().saturating_sub(MAX_AI_USAGE_EVENTS);
            d.ai_usage.drain(..excess);
            serde_json::to_vec(&d.ai_usage)?
        };
        atomic_write(&self.dir.join(AI_USAGE_FILE), &snapshot)
    }

    pub fn clear_ai_usage(&self) -> std::io::Result<()> {
        self.data().ai_usage.clear();
        atomic_write(&self.dir.join(AI_USAGE_FILE), b"[]")
    }

    /// The view modes each launch starts with.
    pub fn launch_modes(&self) -> crate::viewmodes::LaunchModes {
        self.data().launch_modes
    }

    /// Changes one launch default and stores them. Returns the result.
    pub fn set_launch_mode(
        &self,
        mode: crate::viewmodes::Kind,
        on: bool,
    ) -> std::io::Result<crate::viewmodes::LaunchModes> {
        let (modes, snapshot) = {
            let mut d = self.data();
            let before = d.launch_modes;
            d.launch_modes.set(mode, on);
            if d.launch_modes == before {
                return Ok(before);
            }
            (d.launch_modes, serde_json::to_vec(&d.launch_modes)?)
        };
        atomic_write(&self.dir.join(LAUNCH_MODES_FILE), &snapshot)?;
        Ok(modes)
    }

    /// The block list as stored (already validated when the store opened).
    pub fn blocked(&self) -> crate::blocklist::BlockSettings {
        self.data().blocked.clone()
    }

    pub fn set_blocked(&self, blocked: &crate::blocklist::BlockSettings) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            d.blocked = blocked.clone();
            serde_json::to_vec(&d.blocked)?
        };
        atomic_write(&self.dir.join(BLOCKED_FILE), &snapshot)
    }

    pub fn delete_user_script(&self, domain: &str) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            d.user_scripts.retain(|s| s.domain != domain);
            serde_json::to_vec(&d.user_scripts)?
        };
        atomic_write(&self.dir.join(USER_SCRIPTS_FILE), &snapshot)
    }

    pub fn remove_bookmark(&self, url: &str) -> std::io::Result<()> {
        let snapshot = {
            let mut d = self.data();
            d.bookmarks.retain(|b| b.url != url);
            serde_json::to_vec(&d.bookmarks)?
        };
        atomic_write(&self.dir.join(BOOKMARKS_FILE), &snapshot)?;
        self.prune_item_emoji()
    }

    pub fn bookmarks(&self) -> Vec<Bookmark> {
        self.data().bookmarks.clone()
    }

    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.data().bookmarks.iter().any(|b| b.url == url)
    }

    pub fn save_session(&self, session: &Session) -> std::io::Result<()> {
        let mut s = session.clone();
        s.tabs.retain_mut(|t| match storable_url(&t.url) {
            Some(u) => {
                t.url = u;
                t.title = clean_title(&t.title);
                t.name = t.name.as_deref().map(clean_title).filter(|n| !n.is_empty());
                t.emoji = t.emoji.as_deref().and_then(clean_emoji);
                true
            }
            None => false,
        });
        s.tabs.truncate(MAX_SESSION_TABS);
        if s.active.map(|a| a >= s.tabs.len()).unwrap_or(false) {
            s.active = None;
        }
        atomic_write(&self.dir.join(SESSION_FILE), &serde_json::to_vec(&s)?)
    }

    /// Sites where ad blocking is turned off.
    pub fn adblock_allowed(&self) -> BTreeSet<String> {
        self.data().adblock_allowed.clone()
    }

    /// Turns ad blocking off (`allowed = true`) or back on for `site`.
    /// Returns the updated set. Invalid site keys are ignored.
    pub fn set_adblock_allowed(&self, site: &str, allowed: bool) -> std::io::Result<BTreeSet<String>> {
        let Some(site) = crate::blocking::normalise_site(site) else { return Ok(self.adblock_allowed()) };
        let (set, snapshot) = {
            let mut d = self.data();
            if allowed {
                if d.adblock_allowed.len() < MAX_BOOKMARKS {
                    d.adblock_allowed.insert(site);
                }
            } else {
                d.adblock_allowed.remove(&site);
            }
            (d.adblock_allowed.clone(), serde_json::to_vec(&d.adblock_allowed)?)
        };
        atomic_write(&self.dir.join(ADBLOCK_ALLOW_FILE), &snapshot)?;
        Ok(set)
    }

    pub fn load_session(&self) -> Session {
        let mut s: Session = load_or_reset(&self.dir.join(SESSION_FILE));
        s.tabs.retain(|t| storable_url(&t.url).is_some());
        s.tabs.truncate(MAX_SESSION_TABS);
        if s.active.map(|a| a >= s.tabs.len()).unwrap_or(false) {
            s.active = None;
        }
        s
    }
}
