//! SECURITY-REVIEW: passkeys (owner request 2026-11-23). WebKit handles
//! WebAuthn (`navigator.credentials.create` / `.get`, passkeys and security
//! keys) in the content web views by itself; nothing here sees a page, a
//! relying party or a credential. For a browser that is not Safari, WebKit
//! accepts requests for every site only when the app is signed with an
//! entitlement Apple grants on request: on macOS
//! `com.apple.developer.web-browser.public-key-credential` (built in only
//! by `npm run macos:passkeys`, README → Passkeys), on iPadOS the
//! default-browser entitlement `com.apple.developer.web-browser`.
//!
//! This module only reads whether the running app holds the macOS
//! entitlement (Security's `SecTaskCopyValueForEntitlement` on the app's
//! own task) and the person's permission for the app to use their
//! passkeys, and asks for that permission
//! (`ASAuthorizationWebBrowserPublicKeyCredentialManager`,
//! AuthenticationServices, macOS 13.3 / iOS 17.4). AuthenticationServices
//! has no binding in the dependency graph: the class is found by name and
//! called through `msg_send!` with Objective-C exceptions caught, as in
//! `ocr.rs`. Selectors as documented for the class:
//! `authorizationStateForPlatformCredentials` (an `NSInteger` enum:
//! authorized 0, denied 1, not determined 2) and
//! `requestAuthorizationForPublicKeyCredentials:` (a block taking it).

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};

use crate::PasskeyPermission;

#[link(name = "AuthenticationServices", kind = "framework")]
extern "C" {}

/// The macOS entitlement that lets a browser's web views use passkeys for
/// any site.
pub const MACOS_ENTITLEMENT: &str = "com.apple.developer.web-browser.public-key-credential";

fn manager() -> Option<Retained<AnyObject>> {
    let class = AnyClass::get(c"ASAuthorizationWebBrowserPublicKeyCredentialManager")?;
    objc2::exception::catch(AssertUnwindSafe(|| unsafe {
        let m: Retained<AnyObject> = msg_send![class, new];
        m
    }))
    .ok()
}

/// Whether the running app is signed with the macOS passkey entitlement.
/// `None` where it cannot be read (iPadOS, whose public SDK has no
/// `SecTask`; there the default-browser entitlement is what counts).
#[cfg(target_os = "macos")]
pub fn entitled() -> Option<bool> {
    use objc2_foundation::NSString;
    use std::ffi::c_void;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecTaskCreateFromSelf(allocator: *const c_void) -> *mut c_void;
        fn SecTaskCopyValueForEntitlement(
            task: *mut c_void,
            entitlement: *const c_void,
            error: *mut *mut c_void,
        ) -> *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
        fn CFGetTypeID(cf: *const c_void) -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(boolean: *const c_void) -> u8;
    }

    // NSString is toll-free bridged to CFString.
    let key = NSString::from_str(MACOS_ENTITLEMENT);
    unsafe {
        let task = SecTaskCreateFromSelf(std::ptr::null());
        if task.is_null() {
            return Some(false);
        }
        let value = SecTaskCopyValueForEntitlement(task, Retained::as_ptr(&key).cast(), std::ptr::null_mut());
        let on = !value.is_null() && CFGetTypeID(value) == CFBooleanGetTypeID() && CFBooleanGetValue(value) != 0;
        if !value.is_null() {
            CFRelease(value);
        }
        CFRelease(task);
        Some(on)
    }
}

#[cfg(not(target_os = "macos"))]
pub fn entitled() -> Option<bool> {
    None
}

/// The person's permission for this app to use their passkeys.
pub fn permission() -> PasskeyPermission {
    let Some(m) = manager() else { return PasskeyPermission::Unavailable };
    objc2::exception::catch(AssertUnwindSafe(|| unsafe {
        let v: isize = msg_send![&*m, authorizationStateForPlatformCredentials];
        v
    }))
    .map_or(PasskeyPermission::Unavailable, PasskeyPermission::from_raw)
}

/// Asks the person (a system prompt, once) whether this app may use their
/// passkeys; `done` gets the answer, on whatever queue the system calls
/// back on. Call on the main thread.
pub fn request_permission(done: Box<dyn FnOnce(PasskeyPermission) + Send>) {
    let done = Arc::new(Mutex::new(Some(done)));
    let finish = {
        let done = done.clone();
        move |p: PasskeyPermission| {
            if let Some(f) = done.lock().ok().and_then(|mut d| d.take()) {
                f(p);
            }
        }
    };
    let Some(m) = manager() else {
        finish(PasskeyPermission::Unavailable);
        return;
    };
    let answer = finish.clone();
    // The block keeps the manager alive until the system answers.
    let keep = Mutex::new(Some(m.clone()));
    let block = RcBlock::new(move |state: isize| {
        answer(PasskeyPermission::from_raw(state));
        if let Ok(mut k) = keep.lock() {
            k.take();
        }
    });
    let asked = objc2::exception::catch(AssertUnwindSafe(|| unsafe {
        let _: () = msg_send![&*m, requestAuthorizationForPublicKeyCredentials: &*block];
    }));
    if asked.is_err() {
        finish(PasskeyPermission::Unavailable);
    }
}
