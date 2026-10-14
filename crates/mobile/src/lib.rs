//! LUNA mobile core, linked by the iOS app through UniFFI.
//!
//! The split follows the Control this Mac plan: Swift owns `URLSession`, TLS
//! trust evaluation, Keychain and app lifecycle, and hands validated results
//! to this core; the core owns what those results mean. RM-00 proves that
//! split with the direct-host descriptor probe in [`probe`], RM-02 enrolls a
//! phone in [`pairing`], and IOS-00's [`session`] runs the public SDK
//! (`luna-sdk`) over the platform's pinned socket. RM-03 makes that session
//! the reconnecting runtime that follows the Mac's task projections
//! ([`follow`]); RM-04 gives it the Mac's task commands and their outbox
//! ([`session::commands`]).
//!
//! The Swift-visible contract is versioned by [`FACADE_VERSION`]; UniFFI's
//! own contract version and per-function checksums refuse a binding compiled
//! against another library at first use. [`core_identity`] reports what the
//! running library is.

uniffi::setup_scaffolding!("luna_core");

pub mod follow;
pub mod pairing;
pub mod probe;
pub mod runtime;
pub mod session;

/// The facade's own contract version. Bump it whenever a change to an
/// exported type or call is incompatible with Swift written for the old one
/// (UniFFI's checksums already refuse mismatched generated bindings).
///
/// 2 (RM-03): `SocketTransport::close_socket`, the runtime session
/// (`RemoteSession::following`), its lifecycle calls, states and projections.
/// 3 (RM-04): the runtime's task commands and outbox
/// (`session::commands`: create, send, stop, queue actions, archive,
/// `outbox`, `outbox_bytes`/`restore_outbox`); the `tasks-operate` scope.
pub const FACADE_VERSION: u16 = 3;

/// What the running core library is, for build and support evidence.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CoreIdentity {
    pub facade_version: u16,
    /// This crate's version (the workspace version).
    pub core_version: String,
    /// The `luna-sdk` version linked into the core.
    pub sdk_version: String,
    /// The operation contract that SDK speaks.
    pub api_version: u16,
    /// The UniFFI scaffolding contract the generated Swift must match.
    pub uniffi_contract_version: u32,
    /// `<arch>-<os>` plus `-sim` for a simulator build.
    pub target: String,
    /// True for a debug (Cargo dev profile) build.
    pub debug_build: bool,
}

#[uniffi::export]
pub fn core_identity() -> CoreIdentity {
    CoreIdentity {
        facade_version: FACADE_VERSION,
        core_version: env!("CARGO_PKG_VERSION").into(),
        sdk_version: luna_sdk::SDK_VERSION.into(),
        api_version: luna_sdk::API_VERSION,
        uniffi_contract_version: ffi_luna_mobile_uniffi_contract_version(),
        target: format!(
            "{}-{}{}",
            std::env::consts::ARCH,
            std::env::consts::OS,
            if cfg!(target_abi = "sim") { "-sim" } else { "" }
        ),
        debug_build: cfg!(debug_assertions),
    }
}
