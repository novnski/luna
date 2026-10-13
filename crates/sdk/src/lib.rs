//! luna-sdk: the typed public client of a LUNA engine (CORE-05).
//!
//! A client depends on this crate and the public contract, never on the
//! engine. It speaks the versioned operation catalog over any connected
//! [`luna_rpc::RpcClient`] (in-process, local owner IPC or an authenticated
//! remote socket); the desktop app, the `luna` CLI and external tools share
//! it, and the TypeScript SDK is generated from the same types.
//!
//! - [`ops`]: each operation's wire method, catalog id and typed values.
//! - [`Client`]: version negotiation, runtime-validated calls and the
//!   command contract: a stable command id, an exact target, fences, and a
//!   lost reply read back by receipt instead of resent under a new id.
//! - [`TaskCommandWatch`]: a disposable watch with cursor repair.
//! - [`ProjectionStore`]: RM-03's host-keyed, ordered and bounded view of a
//!   paired host's task projections ([`Client::watch_remote_tasks`]).
//! - [`SdkError`]: typed errors whose [`SdkError::kind`] the TypeScript SDK
//!   shares. A domain refusal is a receipt, not an error.

mod client;
mod error;
pub mod ops;
mod projection;
#[cfg(feature = "schema")]
pub mod schema;
mod watch;

pub use client::{Actor, Client, Fencing, TaskCommand, Transport};
pub use error::SdkError;
pub use ops::{CommandIntent, Fences, TaskIntent};
pub use projection::{
    CacheError, Freshness, HostKey, MAX_CACHE_BYTES, MAX_CACHED_TASKS, ProjectionError,
    ProjectionStore, ProjectionStream, ProjectionUpdate, RegistryView, TaskView,
};
pub use watch::{TaskCommandUpdate, TaskCommandWatch};

/// This SDK's version, independent of the app's.
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The operation contract version this SDK speaks.
pub const API_VERSION: u16 = luna_rpc::operations::CATALOG_VERSION;
