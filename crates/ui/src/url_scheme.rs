//! Deep-link scheme ownership.
//!
//! GPUI's `register_url_scheme` calls macOS
//! `setDefaultApplicationAtURL:toOpenURLsWithScheme:`, which rebinds the
//! user's handler for whichever bundle asks, whatever that bundle's Info.plist
//! declares. A LUNA bundle therefore claims a scheme only when its own
//! Info.plist lists it under `CFBundleURLTypes`: the release and `LUNA Dev`
//! bundles do, while QA and fixture bundles strip that key
//! (`scripts/run-macos-dev.sh`) and must leave the user's handler alone.

/// The scheme the release and dev bundles own for conversation links.
pub(crate) const SCHEME: &str = "luna";

/// Whether the running bundle's Info.plist declares `scheme` (compared
/// case-insensitively, as URL schemes are). Unbundled runs and malformed
/// entries answer false.
#[cfg(target_os = "macos")]
pub(crate) fn bundle_declares(scheme: &str) -> bool {
    use objc2_foundation::{NSArray, NSBundle, NSDictionary, NSString, ns_string};

    let Some(types) =
        NSBundle::mainBundle().objectForInfoDictionaryKey(ns_string!("CFBundleURLTypes"))
    else {
        return false;
    };
    let Some(types) = types.downcast_ref::<NSArray>() else {
        return false;
    };
    types.iter().any(|entry| {
        entry
            .downcast_ref::<NSDictionary>()
            .and_then(|entry| entry.objectForKey(ns_string!("CFBundleURLSchemes")))
            .is_some_and(|schemes| {
                schemes.downcast_ref::<NSArray>().is_some_and(|schemes| {
                    schemes.iter().any(|value| {
                        value
                            .downcast_ref::<NSString>()
                            .is_some_and(|value| value.to_string().eq_ignore_ascii_case(scheme))
                    })
                })
            })
    })
}

/// Other platforms register through their own mechanisms and have no QA
/// bundle identities, so they keep registering as before.
#[cfg(not(target_os = "macos"))]
pub(crate) fn bundle_declares(_scheme: &str) -> bool {
    true
}
