//! WebKit code shared by the iPadOS and macOS content hosts: the content
//! web views' delegates and observers, script injection, content blocking,
//! and the app's own HTTPS requests and Keychain items. Platform-specific
//! pieces (the `WKWebView` class, the main queue, popup bookkeeping) come
//! from `crate::platform`.

pub mod delegates;
pub mod downloads;
pub mod http;
pub mod injection;
pub mod keychain;
pub mod rules;
pub mod stores;

use std::cell::Cell;

use block2::RcBlock;
use objc2_foundation::{NSOperationQueue, NSProcessInfo};
use snorkle_core::os::OsVersion;

/// Schedules `f` on the main queue. Always asynchronous, even when called
/// from the main thread, so effects are never applied re-entrantly from
/// inside a WebKit delegate callback.
pub fn dispatch_main(f: impl FnOnce() + Send + 'static) {
    let cell = Cell::new(Some(f));
    let block = RcBlock::new(move || {
        if let Some(f) = cell.take() {
            f()
        }
    });
    unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
}

/// Keeps `path` (a folder) out of device backups (iCloud / Finder backups
/// on iPadOS, Time Machine on macOS). Best effort.
pub fn exclude_from_backup(path: &std::path::Path) {
    use objc2_foundation::{NSNumber, NSString, NSURLIsExcludedFromBackupKey, NSURL};
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let yes = NSNumber::new_bool(true);
    let _ = unsafe { url.setResourceValue_forKey_error(Some(&yes), NSURLIsExcludedFromBackupKey) };
}

pub fn physical_memory() -> u64 {
    NSProcessInfo::processInfo().physicalMemory()
}

pub fn os_version() -> OsVersion {
    let v = NSProcessInfo::processInfo().operatingSystemVersion();
    let c = |x: isize| u32::try_from(x).unwrap_or(0);
    OsVersion::new(c(v.majorVersion), c(v.minorVersion), c(v.patchVersion))
}
