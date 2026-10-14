//! luna-proto — wire types shared by engine, UI, and RPC.
//!
//! Ported from luna's `packages/control/src/wire.ts` + `packages/harness/src/types.ts`.
//! Context occupancy is replicated per chat; billing `Usage` remains a harness passthrough.

pub mod activity;
pub mod agent;
pub mod automation;
pub mod backup;
pub mod board;
pub mod chat_workbench;
pub mod composer_catalog;
pub mod context_control;
pub mod context_transfer;
pub mod contract;
pub mod cross_task_tools;
pub mod diagnostics;
pub mod draft;
pub mod entities;
pub mod execution;
pub mod extension_lifecycle;
pub mod extension_packages;
pub mod extension_runtime;
pub mod git_actions;
pub mod git_checkpoints;
pub mod github_pr;
pub mod goal;
pub mod history;
pub mod host_capacity;
pub mod mcp_input;
pub mod motion;
pub mod native_extensions;
pub mod native_tasks;
pub mod preview;
pub mod project_commands;
pub mod project_defaults;
pub mod prompt_stash;
pub mod provider_accounting;
pub mod provider_catalog;
pub mod provider_defaults;
pub mod provider_id;
pub use provider_catalog::*;
pub mod provider_mcp;
pub mod provider_updates;
pub mod recovery;
/// RM-04 paired-phone task commands; used by path, not re-exported.
pub mod remote_command;
pub mod remote_control;
/// RM-03 host task projections; used by path, not re-exported.
pub mod remote_projection;
pub mod runtime_contract;
pub mod task_command;
pub mod task_identity;
pub mod task_work;
/// CORE-12 native UI contributions and their declarative view models.
pub mod ui_contributions;
pub mod usage;
pub mod view;
pub mod workspace;

pub use activity::*;
pub use agent::*;
pub use automation::*;
pub use backup::*;
pub use board::*;
pub use chat_workbench::*;
pub use composer_catalog::*;
pub use context_control::*;
pub use context_transfer::*;
pub use cross_task_tools::*;
pub use diagnostics::*;
pub use draft::*;
pub use entities::*;
pub use execution::*;
pub use extension_lifecycle::*;
pub use extension_packages::*;
pub use extension_runtime::*;
pub use git_actions::*;
pub use git_checkpoints::*;
pub use github_pr::*;
pub use goal::*;
pub use history::*;
pub use host_capacity::*;
pub use mcp_input::*;
pub use native_extensions::*;
pub use native_tasks::*;
pub use preview::*;
pub use project_commands::*;
pub use project_defaults::*;
pub use prompt_stash::*;
pub use provider_accounting::*;
pub use provider_defaults::*;
pub use provider_mcp::*;
pub use provider_updates::*;
pub use recovery::*;
pub use runtime_contract::*;
pub use task_command::*;
pub use task_identity::*;
pub use task_work::*;
pub use ui_contributions::*;
pub use usage::*;
pub use workspace::*;

/// Parse "0.2.12" (tolerating a `-suffix`/`+build` tail on the last part)
/// into a comparable triple — the fleet feature-gate primitive (device rows
/// stamp `Device::version` at boot). `None` for anything that doesn't lead
/// with three dotted integers, and gates treat `None` as "too old".
pub fn version_triple(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().splitn(3, '.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    let patch = parts.next()?;
    let patch: u64 = patch
        .split(['-', '+'])
        .next()
        .unwrap_or(patch)
        .parse()
        .ok()?;
    Some((major, minor, patch))
}
