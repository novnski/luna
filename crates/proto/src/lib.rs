//! luna-proto — wire types shared by engine, UI, and RPC.
//!
//! Ported from luna's `packages/control/src/wire.ts` + `packages/harness/src/types.ts`.
//! Context occupancy is replicated per chat; billing `Usage` remains a harness passthrough.

pub mod activity;
pub mod agent;
pub mod backup;
pub mod chat_workbench;
pub mod composer_catalog;
pub mod context_control;
pub mod context_transfer;
pub mod cross_task_tools;
pub mod draft;
pub mod entities;
pub mod execution;
pub mod git_actions;
pub mod git_checkpoints;
pub mod github_pr;
pub mod goal;
pub mod history;
pub mod mcp_input;
pub mod motion;
pub mod native_extensions;
pub mod native_tasks;
pub mod preview;
pub mod project_commands;
pub mod project_defaults;
pub mod prompt_stash;
pub mod provider_accounting;
pub mod provider_mcp;
pub mod provider_updates;
pub mod recovery;
pub mod runtime_contract;
pub mod task_command;
pub mod task_identity;
pub mod task_work;
pub mod usage;
pub mod view;
pub mod workspace;

pub use activity::*;
pub use agent::*;
pub use backup::*;
pub use chat_workbench::*;
pub use composer_catalog::*;
pub use context_control::*;
pub use context_transfer::*;
pub use cross_task_tools::*;
pub use draft::*;
pub use entities::*;
pub use execution::*;
pub use git_actions::*;
pub use git_checkpoints::*;
pub use github_pr::*;
pub use goal::*;
pub use history::*;
pub use mcp_input::*;
pub use native_extensions::*;
pub use native_tasks::*;
pub use preview::*;
pub use project_commands::*;
pub use project_defaults::*;
pub use prompt_stash::*;
pub use provider_accounting::*;
pub use provider_mcp::*;
pub use provider_updates::*;
pub use recovery::*;
pub use runtime_contract::*;
pub use task_command::*;
pub use task_identity::*;
pub use task_work::*;
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
