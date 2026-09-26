# Project execution defaults — 26 September 2026

Branch: `codex/project-execution-defaults`.
Worktree: `/Users/iulian/.codex/worktrees/9f21/luna`.
Base: `d1c71c3a31d6ef7c895adc9084c27293940fce39`.
Preimplementation regression cases: `8ab4632b`.
Source checkpoint: `3650bd0e`; catalog race guard `25d06a81`.
Reviewed backend base `dbb38ee1` adopted without conflicts in `aabb52a1`.
Shared fixture repair `65c3800d` was adopted as `7d4b5f0b` before that merge.

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
a timestamped directory beneath `target/qa/project-defaults`, preserves exact
commands/source/exit codes and stops on the shared guard. An explicit
`LUNA_DEFAULTS_QA_OUTPUT` selects a named batch directory.
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

The first granted batch ran with `CARGO_INCREMENTAL=0` and the standard wrapper
on `aabb52a1` (only the logging script differed). Initial failures are retained.

| Check | Result | Evidence |
| --- | --- | --- |
| Engine RPC/registry/restart integration | 2 passed, 0 failed; compile 51.28s, tests 0.05s | `target/qa/project-defaults/batch-1/engine.log`, `persistence.json` |
| Picker GPUI integration compilation | Exit 101: new Projects category missing from existing sidebar icon match | `target/qa/project-defaults/batch-1/picker.log` |
| Picker retry after one-line existing-icon fix | Exit 75 before compilation: aggregate reservation blocked | `target/qa/project-defaults/batch-2/picker.log` |
| Source formatting, shell/Node syntax, diff whitespace | Passed static checks | Source checkpoint / git diff checks |
| Corrected picker GPUI integration | 3 passed, 0 failed; compile 29.91s, tests 0.03s (34s wall) | `target/qa/project-defaults/batch-3/picker.log` |
| Full workspace format check | Exit 0, 5s wall | `target/qa/project-defaults/batch-3/format.log` |
| Focused engine/UI compile | Exit 0, 48s wall | `target/qa/project-defaults/batch-3/compile.log` |
| Focused engine/UI Clippy | Exit 0, 35s wall; existing warnings, none in the new defaults modules | `target/qa/project-defaults/batch-3/lint.log` |
| Native app/UI/IPC/signature | Pending separately granted slot | No native pass claimed |

Engine coverage confirms missing-field migration through serialized registry
state, owner/missing-project/malformed-value rejection, stale expected-value
rejection, restart persistence, explicit inherit clearing and unchanged existing
task rows. The corrected GPUI cases verify A → B → A transitions, reconstructed
picker state, inherit clearing, retained explicit draft choices across watch
updates, global fallback preservation, existing task config authority,
provider-default model semantics, unavailable model retention/blocking and
first-Full-access confirmation/recovery. Native visual behavior remains pending.

The second admission observed 156,642,626,920 bytes (145.89 GiB) in aggregate;
adding the default 25 GiB reservation would exceed the 170 GiB operating limit.
No reservation override, retry loop, cache cleanup or native launch followed.
The build slot was released and the coordinator notified. This checkout's
`target/debug` measured 7.1 GiB; it is preserved for the pending verification.

After coordinator-managed normal 20 GiB cache collection, batch 3 was explicitly
regranted and passed on `6d637af9` without lowering the 25 GiB reservation or
changing cache policy. `commands.txt`, `source.txt`, `exits.tsv`, `result.txt`
and before/after accounting are retained with that batch. Final aggregate was
154,439,057,096 bytes (143.83 GiB); this target/debug was 9.0 GiB and available
disk space 393 GiB. The compiler slot was released after the checks.
