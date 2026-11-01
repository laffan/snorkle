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

    pub fn set_insets(&mut self, top: f64, left: f64, _right: f64, _bottom: f64) {
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

/// No network stack off iOS (desktop preview): AI requests fail politely.
pub mod http {
    pub fn send(_req: snorkle_core::ai::HttpRequest, done: crate::HttpDone) {
        done(Err("AI requests run on iPadOS only (this is the desktop preview).".into()));
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
