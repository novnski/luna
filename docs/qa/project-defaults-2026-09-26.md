# Project execution defaults — 26 September 2026

Branch: `codex/project-execution-defaults`.
Worktree: `/Users/iulian/.codex/worktrees/9f21/luna`.
Base: `d1c71c3a31d6ef7c895adc9084c27293940fce39`.
Preimplementation regression cases: `8ab4632b`.
Source checkpoint: `3650bd0e` (verification pending).

## Contract and scope

Settings → Projects adds persisted provider/model, permissions and checkout
choices per project. Each group can inherit; “Inherit all global defaults”
clears all three groups. Old rows without `executionDefaults` inherit without
rewriting existing tasks. Data uses the existing registry, backup, sync and
DeviceWriteHandle path. The mutation checks owner and the expected previous
value under the registry lock; stale writes fail instead of overwriting newer
settings. Remote writes use the existing targeted relay for this operation.

All desktop new-chat affordances feed the same Pickers/composer resolver.
ComposerDefaults remains the fallback. Provider+model is one group: inherit
uses the global provider and its remembered model; an explicit provider with
no model uses that provider's catalog default. Explicit project choices never
silently fall back when unavailable. The user can pick another model/provider
for that draft or update the project settings. Provider/machine permission
limits and the existing first-Full-access confirmation still apply.

Current checkout retains existing worktree reuse; New worktree emits the
existing queued WorktreeSpec. A non-Git folder cannot use the latter. A project
or device transition clears stale draft selections. Watch updates preserve
explicit draft choices. Project-specific provider/model and permission picks
do not overwrite their global fallback groups.

The coordinator explicitly scoped this first increment to desktop new chats.
Raw CreateChat callers, native forks and provider-created child tasks keep
explicit config/Run contracts. No Plan → Build changes, new provider runtime,
queue authority, task identity or alternate worktree implementation is added.
The Projects container exposes selected_project_id() for the separately owned
ProjectCommandSettings child card; that composition remains coordinator-owned.

## Source comparison

- T3 Code checkout `e5a46d6c5d00b89afba5274a94d42428c8d79763`; safely fetched
  `origin/main` is `2a9832b8019039f812706639c11ccc24ce4c4e05`. Inspected the
  `packages/shared/src/projectSettings.ts` resolution (including the fetched
  ref), `packages/contracts/src/settings.ts` persisted fields and
  `apps/web/src/components/settings/ProjectDefaultsSettings.tsx`.
- BB checkout `9c8112e546f042b923f1a0c080f6b427c2fa60f6`; safely fetched
  `origin/main` is `9c9bae7f36a237c7e1b96de3d4c2186d13967686`. Inspected the
  project route's existing execution-defaults references on the fetched ref.
- Luna reuses its settings widgets, theme, ComposerDefaults, provider catalogs,
  permission checks and checkout plan. Neither reference checkout was changed.

## Repeatable verification

Requires a coordinator-granted build slot:

```sh
scripts/qa-project-defaults.sh
```

The runner saves logs and `persistence.json` under
`target/qa/project-defaults`, preserves exit codes and stops on the shared guard.
The first-written cases are in `project-defaults-cases.md`, the engine RPC /
registry restart integration target and the GPUI picker integration module.
No provider invocation is required for these cases.

Native QA requires a separately granted app slot:

```sh
LUNA_DEV_APP_NAME='LUNA QA Defaults' \
LUNA_DEV_BUNDLE_ID=com.novnski.luna.qa.defaults \
LUNA_DEV_IPC_PORT=59788 \
LUNA_DEV_DATA_DIR="$PWD/target/luna-qa-data/defaults" \
CARGO_INCREMENTAL=0 scripts/run-macos-dev.sh --refresh
node scripts/qa-project-defaults-live.mjs seed
node scripts/qa-project-defaults-live.mjs read
```

The live helper only accepts QA port 59788 and never sends prompts. It creates
or reuses two Git fixtures within this checkout's QA artifact directory and
saves authoritative project/task readback. `clear` explicitly restores both QA
projects to inherit. Keep fixtures while the packaged QA app refers to them.

## Verification status

Source formatting with rustfmt, shell syntax, Node syntax and git diff checks
passed. Cargo, focused regressions, Clippy and native QA are pending the shared
build slots. Do not report this source checkpoint as runtime-verified.
