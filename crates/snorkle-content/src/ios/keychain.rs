//! SECURITY-REVIEW: the user's AI API keys in the iOS Keychain (generic
//! passwords, service `app.snorkle.ai-keys`, one account per provider),
//! accessible only while the device is unlocked and never synced or backed
//! up to another device (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`).
//! Keys are read only to send a request; they never reach the chrome UI.

use objc2_core_foundation::{CFData, CFDictionary, CFRetained, CFString, CFType};
use objc2_security::{
    kSecAttrAccessible, kSecAttrAccessibleWhenUnlockedThisDeviceOnly, kSecAttrAccount, kSecAttrService, kSecClass,
    kSecClassGenericPassword, kSecMatchLimit, kSecMatchLimitOne, kSecReturnData, kSecValueData, SecItemAdd,
    SecItemCopyMatching, SecItemDelete,
};

const SERVICE: &str = "app.snorkle.ai-keys";
const ERR_SEC_SUCCESS: i32 = 0;

fn query(account: &str, extra: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let service = CFString::from_str(SERVICE);
    let account = CFString::from_str(account);
    let mut keys: Vec<&CFString> = unsafe { vec![kSecClass, kSecAttrService, kSecAttrAccount] };
    let mut values: Vec<&CFType> =
        unsafe { vec![kSecClassGenericPassword.as_ref(), service.as_ref(), account.as_ref()] };
    for (k, v) in extra {
        keys.push(k);
        values.push(v);
    }
    CFDictionary::from_slices(&keys, &values)
}

pub fn get(account: &str) -> Option<String> {
    let yes = unsafe { objc2_core_foundation::kCFBooleanTrue }?;
    let q = unsafe { query(account, &[(kSecReturnData, yes.as_ref()), (kSecMatchLimit, kSecMatchLimitOne.as_ref())]) };
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
            account,
            &[
                (kSecValueData, data.as_ref()),
                (kSecAttrAccessible, kSecAttrAccessibleWhenUnlockedThisDeviceOnly.as_ref()),
            ],
        )
    };
    match unsafe { SecItemAdd(q.as_opaque(), std::ptr::null_mut()) } {
        ERR_SEC_SUCCESS => Ok(()),
        status => Err(format!("Could not save the key in the Keychain ({status}).")),
    }
}

pub fn delete(account: &str) {
    let q = query(account, &[]);
    unsafe { SecItemDelete(q.as_opaque()) };
}
