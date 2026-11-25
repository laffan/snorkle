//! Links Snorkle's share extension left for the app (owner request
//! 2026-11-25): the extension (`src-tauri/ios/ShareExtension/`) adds each
//! link it is given to a list in the App Group's user defaults
//! (`snorkle_core::input::SHARE_APP_GROUP`, key `SHARED_LINKS_KEY`); the app
//! takes the list whenever it becomes active. The strings come from another
//! process and from whatever app shared them: they are checked by
//! `snorkle_core::input::shared_links` and go through the navigation policy.

use objc2::AllocAnyThread;
use objc2_foundation::{NSString, NSUserDefaults};
use snorkle_core::input::{SHARED_LINKS_KEY, SHARE_APP_GROUP};

/// The most strings read from the queue (the extension keeps fewer).
const MAX_READ: usize = 200;

/// The queued links, removed from the queue; unchecked. Empty when there
/// are none or the App Group is not available (the app was signed without
/// it: the suite is then the app's own, where nothing is queued).
pub fn take_shared_links() -> Vec<String> {
    let suite = NSString::from_str(SHARE_APP_GROUP);
    let Some(defaults) = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&suite)) else {
        return Vec::new();
    };
    let key = NSString::from_str(SHARED_LINKS_KEY);
    let Some(list) = defaults.stringArrayForKey(&key) else { return Vec::new() };
    defaults.removeObjectForKey(&key);
    list.iter().take(MAX_READ).map(|s| s.to_string()).collect()
}
