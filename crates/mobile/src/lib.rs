//! LUNA mobile core, linked by the iOS app through UniFFI.
//!
//! The split follows the Control this Mac plan: Swift owns `URLSession`, TLS
//! trust evaluation, Keychain and app lifecycle, and hands validated results
//! to this core; the core owns what those results mean. RM-00 proves that
//! split with the direct-host descriptor probe in [`probe`].

uniffi::setup_scaffolding!("luna_core");

pub mod probe;
