//! SECURITY-REVIEW: the user's AI API keys in the Keychain (generic
//! passwords, service `app.snorkle.ai-keys`, one account per provider),
//! accessible only while the device is unlocked and never synced or backed
//! up to another device (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`).
//! Keys are read only to send a request; they never reach the chrome UI.
//!
//! macOS: that accessibility class exists only in the data-protection
//! keychain (`kSecUseDataProtectionKeychain`), which needs a signed app
//! with an application identifier. Builds without one (unsigned or
//! ad-hoc-signed development builds) get `errSecMissingEntitlement`, and
//! the key is stored in the user's login keychain instead: encrypted at
//! rest and locked with the user's session, but readable by any process
//! the user approves in the Keychain prompt. Reads and deletes look in both.

use objc2_core_foundation::{CFData, CFDictionary, CFRetained, CFString, CFType};
#[cfg(target_os = "macos")]
use objc2_security::kSecUseDataProtectionKeychain;
use objc2_security::{
    kSecAttrAccessible, kSecAttrAccessibleWhenUnlockedThisDeviceOnly, kSecAttrAccount, kSecAttrService, kSecClass,
    kSecClassGenericPassword, kSecMatchLimit, kSecMatchLimitOne, kSecReturnData, kSecValueData, SecItemAdd,
    SecItemCopyMatching, SecItemDelete,
};

const SERVICE: &str = "app.snorkle.ai-keys";
const ERR_SEC_SUCCESS: i32 = 0;
/// `errSecMissingEntitlement`: no data-protection keychain for this build.
#[cfg(target_os = "macos")]
const ERR_SEC_MISSING_ENTITLEMENT: i32 = -34018;

/// Which keychain a query goes to. On iOS there is only one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keychain {
    /// iOS's keychain; on macOS the data-protection keychain.
    Protected,
    /// macOS only: the login keychain (fallback, see the module docs).
    #[cfg(target_os = "macos")]
    Login,
}

#[cfg(target_os = "ios")]
const KEYCHAINS: &[Keychain] = &[Keychain::Protected];
#[cfg(target_os = "macos")]
const KEYCHAINS: &[Keychain] = &[Keychain::Protected, Keychain::Login];

fn query(which: Keychain, account: &str, extra: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let service = CFString::from_str(SERVICE);
    let account = CFString::from_str(account);
    let mut keys: Vec<&CFString> = unsafe { vec![kSecClass, kSecAttrService, kSecAttrAccount] };
    let mut values: Vec<&CFType> =
        unsafe { vec![kSecClassGenericPassword.as_ref(), service.as_ref(), account.as_ref()] };
    #[cfg(target_os = "macos")]
    let yes = unsafe { objc2_core_foundation::kCFBooleanTrue };
    #[cfg(target_os = "macos")]
    if let (Keychain::Protected, Some(yes)) = (which, yes) {
        keys.push(unsafe { kSecUseDataProtectionKeychain });
        values.push(yes.as_ref());
    }
    #[cfg(target_os = "ios")]
    let _ = which;
    for (k, v) in extra {
        keys.push(k);
        values.push(v);
    }
    CFDictionary::from_slices(&keys, &values)
}

pub fn get(account: &str) -> Option<String> {
    KEYCHAINS.iter().find_map(|k| get_from(*k, account))
}

fn get_from(which: Keychain, account: &str) -> Option<String> {
    let yes = unsafe { objc2_core_foundation::kCFBooleanTrue }?;
    let q = unsafe {
        query(which, account, &[(kSecReturnData, yes.as_ref()), (kSecMatchLimit, kSecMatchLimitOne.as_ref())])
    };
    let mut out: *const CFType = std::ptr::null();
    let status = unsafe { SecItemCopyMatching(q.as_opaque(), &mut out) };
    if status != ERR_SEC_SUCCESS || out.is_null() {
        return None;
    }
    // SAFETY: SecItemCopyMatching returns a +1 CFData for kSecReturnData.
    let data = unsafe { CFRetained::from_raw(std::ptr::NonNull::new(out as *mut CFData)?) };
    String::from_utf8(data.to_vec()).ok()
}

pub fn set(account: &str, secret: &str) -> Result<(), String> {
    delete(account);
    let data = CFData::from_bytes(secret.as_bytes());
    let q = unsafe {
        query(
            Keychain::Protected,
            account,
            &[
                (kSecValueData, data.as_ref()),
                (kSecAttrAccessible, kSecAttrAccessibleWhenUnlockedThisDeviceOnly.as_ref()),
            ],
        )
    };
    match unsafe { SecItemAdd(q.as_opaque(), std::ptr::null_mut()) } {
        ERR_SEC_SUCCESS => Ok(()),
        #[cfg(target_os = "macos")]
        ERR_SEC_MISSING_ENTITLEMENT => {
            let q = unsafe { query(Keychain::Login, account, &[(kSecValueData, data.as_ref())]) };
            match unsafe { SecItemAdd(q.as_opaque(), std::ptr::null_mut()) } {
                ERR_SEC_SUCCESS => Ok(()),
                status => Err(format!("Could not save the key in the Keychain ({status}).")),
            }
        }
        status => Err(format!("Could not save the key in the Keychain ({status}).")),
    }
}

pub fn delete(account: &str) {
    for k in KEYCHAINS {
        let q = query(*k, account, &[]);
        unsafe { SecItemDelete(q.as_opaque()) };
    }
}
