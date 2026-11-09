//! Non-iOS stand-in for [`ContentHost`]. iPadOS is the only target
//! platform; this exists so the app, its IPC-isolation tests and the chrome
//! UI can be built and exercised on a development machine. It renders no
//! web content and records the effects it was asked to apply.

use std::collections::BTreeSet;
use std::rc::Rc;

use snorkle_core::browser::Effect;
use snorkle_core::os::OsVersion;
use snorkle_core::tabs::TabId;

use crate::ContentEvents;

pub struct ContentHost {
    live: BTreeSet<TabId>,
    pub applied: Vec<Effect>,
    pub top: f64,
    pub left: f64,
    _events: Rc<dyn ContentEvents>,
}

impl ContentHost {
    /// # Safety
    /// Trivially safe on non-iOS targets; the signature matches the iOS
    /// version, where `root_view` must be a valid `UIView*`.
    pub unsafe fn new(_root_view: *mut std::ffi::c_void, events: Rc<dyn ContentEvents>) -> Self {
        ContentHost { live: BTreeSet::new(), applied: Vec::new(), top: 0.0, left: 0.0, _events: events }
    }

    pub fn apply(&mut self, effect: &Effect) {
        match effect {
            Effect::CreateView { tab, .. } | Effect::AdoptPopup { tab } => {
                self.live.insert(*tab);
            }
            Effect::DestroyView { tab, .. } => {
                self.live.remove(tab);
            }
            _ => {}
        }
        self.applied.push(effect.clone());
    }

    pub fn set_insets(&mut self, top: f64, left: f64, _right: f64, _bottom: f64, _holes: Vec<crate::Hole>, _animate: Option<f64>) {
        self.top = crate::clamp_inset(top, 1024.0);
        self.left = crate::clamp_inset(left, 1366.0);
    }

    /// Runs the conversion (so it is exercised in desktop builds) but has
    /// no WebKit to install into.
    pub fn install_rules(&mut self, _key: String, make: crate::MakeRuleLists, done: crate::RulesDone) {
        std::thread::spawn(move || done(make().map(|lists| lists.len())));
    }

    pub fn set_injections(&mut self, _injections: crate::Injections, _run_now: bool) {}

    pub fn scroll_to_heading(&mut self, _tab: snorkle_core::tabs::TabId, _index: u32) {}

    pub fn sample_headings(&mut self, _tab: snorkle_core::tabs::TabId) {}

    /// No pages render off iOS.
    pub fn find(
        &mut self,
        _tab: snorkle_core::tabs::TabId,
        _query: String,
        _step: snorkle_core::find::FindStep,
        done: crate::TextDone,
    ) {
        done(None);
    }

    pub fn end_find(&mut self, _tab: snorkle_core::tabs::TabId) {}

    /// # Safety
    /// Trivially safe off iOS (nothing is retained).
    pub unsafe fn set_chrome_view(&mut self, _view: *mut std::ffi::c_void) {}

    pub fn focus_chrome(&mut self) {}

    pub fn set_appearance(&mut self, _appearance: crate::Appearance) {}

    pub fn set_swipe_navigation(&mut self, _on: bool) {}

    pub fn set_pull_to_refresh(&mut self, _on: bool) {}

    /// No pages render off iOS: nothing to save.
    pub fn save_offline(&mut self, _tab: TabId, done: crate::DataDone) {
        done(None);
    }

    pub fn load_offline(&mut self, _tab: TabId, _url: &url::Url, _data: Vec<u8>) {}

    pub fn image_result(
        &mut self,
        _action: snorkle_core::imagemenu::ImageAction,
        _url: &url::Url,
        _data: Vec<u8>,
        _mime: &str,
        _from: (f64, f64, f64, f64),
    ) -> Result<(), String> {
        Ok(())
    }

    pub fn set_shortcuts(&mut self, _bindings: Vec<crate::KeyBinding>, _suspend: bool) {}

    /// No downloads off iOS and macOS.
    pub fn cancel_download(&mut self, _id: u64) {}

    pub fn answer_frame_download(&mut self, _ask: u64, _allow: bool) {}

    pub fn export_file(&mut self, _path: &std::path::Path) -> Result<(), String> {
        Err("Files can be saved on iPadOS and macOS only (this is the desktop preview).".into())
    }

    pub fn share_file(&mut self, _path: &std::path::Path, _from: (f64, f64, f64, f64)) -> Result<(), String> {
        Err("Files can be shared on iPadOS and macOS only (this is the desktop preview).".into())
    }

    pub fn share(&mut self, _url: &str, _from: (f64, f64, f64, f64)) {}

    pub fn define(&mut self, _term: &str, _from: (f64, f64, f64, f64)) {}

    /// No apps to open off iOS.
    pub fn open_in_app(&mut self, _url: &url::Url, done: crate::OpenedDone) {
        done(false);
    }

    pub fn read_selection(&mut self, _tab: snorkle_core::tabs::TabId, done: crate::TextDone) {
        done(None);
    }

    /// No pages render off iOS.
    pub fn read_page_text(&mut self, _tab: snorkle_core::tabs::TabId, done: crate::TextDone) {
        done(None);
    }

    pub fn clear_data(&mut self, _kind: crate::DataKind) {}

    pub fn set_console_capture(&mut self, _on: bool) {}

    /// No WebKit off iOS: no favicons.
    pub fn fetch_favicon(&mut self, _host: String, done: crate::FaviconDone) {
        done(None);
    }

    pub fn live_views(&self) -> usize {
        self.live.len()
    }
}

/// Runs `f` on the current thread (no UIKit run loop off iOS).
pub fn dispatch_main(f: impl FnOnce() + Send + 'static) {
    f()
}

pub fn physical_memory() -> u64 {
    8 * 1024 * 1024 * 1024
}

/// Pretends to be the newest known OS so desktop dev builds are not gated.
pub fn os_version() -> OsVersion {
    OsVersion::new(27, 0, 0)
}

/// No backups to keep out of off iOS and macOS.
pub fn exclude_from_backup(_path: &std::path::Path) {}

/// See `os_version`.
pub fn webkit_version() -> OsVersion {
    os_version()
}

/// No network stack off iOS (desktop preview): AI requests fail politely.
pub mod http {
    pub fn send(_req: snorkle_core::ai::HttpRequest, done: crate::HttpDone) {
        done(Err("AI requests run on iPadOS only (this is the desktop preview).".into()));
    }

    pub fn fetch_image(_url: &url::Url, done: crate::ImageDone) {
        done(Err("Images can be saved on iPadOS only (this is the desktop preview).".into()));
    }
}

/// In-memory stand-in for the Keychain (desktop preview and tests).
pub mod keychain {
    use std::collections::HashMap;
    use std::sync::Mutex;

    static KEYS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

    pub fn get(account: &str) -> Option<String> {
        KEYS.lock().ok()?.as_ref()?.get(account).cloned()
    }

    pub fn set(account: &str, secret: &str) -> Result<(), String> {
        let mut k = KEYS.lock().map_err(|_| "Keychain unavailable".to_string())?;
        k.get_or_insert_with(HashMap::new).insert(account.to_string(), secret.to_string());
        Ok(())
    }

    pub fn delete(account: &str) {
        if let Ok(mut k) = KEYS.lock() {
            if let Some(m) = k.as_mut() {
                m.remove(account);
            }
        }
    }
}
