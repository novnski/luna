//! Direct host for Control this Mac.
//!
//! M0 (RM-00) provides the two pieces every later milestone builds on:
//! - [`identity::HostIdentity`]: the profile-owned TLS key and target that a
//!   phone pins out of band;
//! - [`host`]: an explicit-interface TLS listener serving only the harmless
//!   descriptor and health endpoints.
//!
//! Nothing here authenticates a device or reaches engine methods; RM-01 adds
//! the authenticated gateway over the same identity and listener rules.
//! The host is Unix-only (macOS is the supported host); elsewhere the crate is
//! empty.

#![cfg(unix)]

pub mod host;
pub mod identity;

use luna_proto::remote_control::{REMOTE_DIRECT_V1, RemoteDescriptor};

/// The public descriptor for `identity`.
pub fn descriptor(identity: &identity::HostIdentity) -> RemoteDescriptor {
    RemoteDescriptor {
        protocol: REMOTE_DIRECT_V1.to_owned(),
        target: identity.target().clone(),
        catalog_version: luna_rpc::operations::CATALOG_VERSION,
        spki_sha256: identity.spki_sha256().to_owned(),
    }
}
