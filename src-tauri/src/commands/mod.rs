//! SECURITY-REVIEW: Tauri commands. Reachable only from the bundled chrome
//! UI in the `main` webview (capabilities/chrome.json; enforced by Tauri's
//! ACL and tested in tests/ipc_isolation.rs). Content web views have no IPC
//! at all.
//!
//! Even though the caller is trusted, every argument is treated as data:
//! tab ids that do not exist are ignored, strings are length-limited and
//! parsed by the core, numbers are clamped by the content host.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use snorkle_core::browser::ChromeState;
use snorkle_core::input::InputError;
use snorkle_core::tabs::{Profile, TabId};
use snorkle_core::url_display::{self, DisplayUrl};
use snorkle_core::viewmodes::{self, ViewModes};
use snorkle_core::Url;

use crate::state::{now_ms, with_host, Shared};

type S<'a> = tauri::State<'a, Arc<Shared>>;

mod tabs;
mod history;
mod page;
mod later;
mod system;
mod items;
mod downloads;
mod settings;

pub use tabs::*;
pub use history::*;
pub use page::*;
pub use later::*;
pub use system::*;
pub use items::*;
pub use downloads::*;
pub use settings::*;

fn with_browser<F>(s: &S<'_>, f: F)
where
    F: FnOnce(&mut snorkle_core::browser::Browser) -> Vec<snorkle_core::browser::Effect>,
{
    let effects = f(&mut s.browser());
    s.run(effects);
}
