//! Noninteractive macOS Keychain reads for background discovery and startup.
//!
//! `security_framework::passwords::get_generic_password` uses the platform
//! default (`kSecUseAuthenticationUIAllow`) and can therefore open an OS
//! password dialog. Background work must instead fail closed when UI would be
//! required. Apple documents `kSecUseAuthenticationUIFail` as returning
//! `errSecInteractionNotAllowed` in that case.

use core_foundation::base::{CFGetTypeID, CFRelease, CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::{CFData, CFDataRef};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use security_framework::base::Error;
use security_framework_sys::base::{errSecParam, errSecSuccess};
use security_framework_sys::item::{
    kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword, kSecReturnData,
    kSecUseAuthenticationUI,
};
use security_framework_sys::keychain_item::SecItemCopyMatching;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecUseAuthenticationUIFail: CFStringRef;
}

pub(crate) fn get_generic_password_noninteractive(
    service: &str,
    account: &str,
) -> Result<Vec<u8>, Error> {
    let pairs: Vec<(CFString, CFType)> = unsafe {
        vec![
            (
                CFString::wrap_under_get_rule(kSecClass),
                CFString::wrap_under_get_rule(kSecClassGenericPassword).into_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kSecAttrService),
                CFString::from(service).into_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kSecAttrAccount),
                CFString::from(account).into_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kSecUseAuthenticationUI),
                CFString::wrap_under_get_rule(kSecUseAuthenticationUIFail).into_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kSecReturnData),
                CFBoolean::true_value().into_CFType(),
            ),
        ]
    };
    let query = CFDictionary::from_CFType_pairs(&pairs);
    let mut result: CFTypeRef = std::ptr::null();
    let status = unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &mut result) };
    if status != errSecSuccess {
        return Err(Error::from_code(status));
    }
    if result.is_null() {
        return Err(Error::from_code(errSecParam));
    }
    if unsafe { CFGetTypeID(result) } != CFData::type_id() {
        unsafe { CFRelease(result) };
        return Err(Error::from_code(errSecParam));
    }
    let data = unsafe { CFData::wrap_under_create_rule(result as CFDataRef) };
    Ok(data.bytes().to_vec())
}
