# Test audit, 2026-10-09

A campaign-mode run of the [test-audit skill](../../../.codex/skills/test-audit/SKILL.md)
over every LUNA-owned test: Rust crates and apps, the edge worker, iOS, QA
scripts and UI fixtures. `vendor/` holds upstream forks and was out of scope.
Ten lanes, split along production owners, each wrote a ledger with one mark per
test declaration and then applied it; the coordinator ran the baseline, the
integration build, every suite and the preservation review.

## Results

| Lane | Scope | R | F | C | D |
| --- | --- | ---: | ---: | ---: | ---: |
| [L01](L01-engine-src.md) | `crates/engine/src` inline tests | 413 | 0 | 17 | 19 |
| [L02](L02-engine-it-a-l.md) | `crates/engine/tests/[a-l]*` | 270 | 6 | 4 | 4 |
| [L03](L03-engine-it-m-z.md) | `crates/engine/tests/[m-z]*`, `tests/support` | 308 | 2 | 3 | 5 |
| [L04](L04-harness.md) | `crates/harness` and its provider peers | 360 | 2 | 7 | 20 |
| [L05](L05-ui-chat.md) | UI chat views | 407 | 1 | 3 | 15 |
| [L06](L06-ui-shell.md) | UI shell, state, pickers | 332 | 8 | 17 | 15 |
| [L07](L07-ui-panels.md) | UI settings, files, terminal, changes, theme | 353 | 4 | 16 | 29 |
| [L08](L08-sync-doc-proto-rpc.md) | `crates/sync`, `doc`, `proto`, `rpc` | 242 | 13 | 9 | 33 |
| [L09](L09-small-crates-apps-edge.md) | small crates, `apps/luna*`, `edge/` | 176 | 10 | 3 | 13 |
| [L10](L10-ios-qa-tooling.md) | iOS, QA scripts, UI fixtures, CI | 272 | 9 | 0 | 8 |
| **Total** | 3,428 units | **3,133** | **55** | **79** | **161** |

R retain, F repair the assertion, C consolidate into a named keeper, D delete
with the remaining proof named. Rust test declarations went from 3,108 to
2,888. About 1,000 lines of production code went with them: test-only seams
(`#[cfg(test)]` accessors, `*_for_tests` constructors, fixture-only feature
impls) and dead paths whose only callers were tests.

## Proof

All runs used `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0` in one linked
build worktree, split per package to stay under the build guard's growth
reservation:

```
scripts/cargo.sh test --locked -p luna-engine --no-fail-fast
scripts/cargo.sh test --locked --workspace --exclude luna-ui --exclude luna-engine --features luna-sync/mock-server --no-fail-fast
scripts/cargo.sh test --locked -p luna-ui --no-fail-fast -- --test-threads=1
npm run typecheck --prefix edge && npm test --prefix edge
```

| Run | Passed | Failed | Notes |
| --- | ---: | ---: | --- |
| Baseline `e4a0f534` | 2,512 | 18 tests | Edge 54/54 |
| Final, before the Codex fake repair | 2,373 | 6 tests | Nothing newly failing; no new compiler warnings; edge 48/48 |
| Codex login suites after repair | 4 | 0 | |

The three `workspace_identity_repair` tests that also failed on the baseline
were repaired afterwards (below); the suite passes 17/17.

Preservation review: a keeper-existence check over every D and C row found one
apparent gap: L06 and L07 each retired a `resize_drag_sample` replay and cited
the other's copy. The production sidebar-drag tests still own it, and the
ledger now names them. Each carried or restored contract then got one
production mutation, all caught and restored byte for byte:

| Mutation | Keeper that went red |
| --- | --- |
| Named Codex login drops the file-auth override | `codex_login_resolver` |
| RPC inventory forgets an automation method | `remote_auth::policy_inventory_is_complete_and_unknown_fails_closed` |
| Gradient-spin rest band ends at 0.85 | `luna_proto::motion::gradient_spin_holds_dim_then_snaps_back` (the carried 0.9 row) |
| Resize drag never latches its edge | `shell::sidebar_drag_nudges_each_edge_once_until_rearmed` |
| Store accepts one schema ahead | `side_chat_store::next_schema_is_rejected_before_wal_or_migration_changes` |
| Update manifest tolerates a missing `files` map | `update::manifest_parses_files_and_rejects_a_manifest_without_them` |

## Baseline failures

| Test | Cause | Resolution |
| --- | --- | --- |
| `luna-rpc` `policy_inventory_is_complete_and_unknown_fails_closed` | Five automation methods (824a0603) missing from `methods::ALL`; remote callers got `UnknownMethod` | Product fix d494e7e5 |
| `side_chat_store` ×3 | Pinned schema v16; main is at v19 | Reads the current version from a fresh store; fixture undoes v18/v19 |
| `luna-update` `manifest_parses_with_and_without_files` | Signed manifests require `files` since 4099e720 | Asserts refusal |
| `luna` `paths` and `daemon` identity tests | Rebrand rewrote one side of each assertion | Repaired in L09 |
| `codex_login_resolver` | Fake expected `login` first; named logins pass `-c cli_auth_credentials_store="file"` first | Fake pins the full argv |
| `codex_login_resolution` | Fake ran `sleep` through a PATH the probe empties; exit 127 read as Error | `exec /bin/sleep` |
| `luna-preview` peer ×2, `leak` | WebRTC/STUN reachability | Passed on later runs; environment |
| `usage_refresh` | Timing | Passed on later runs |
| `luna-sync` `registry_client` | Needed `mock-server` but did not declare it | `required-features` |
| `workspace_identity_repair` `repair_contract` ×2 | The legacy seed kept project `sourceSpaceId`s on rows it downgraded to `projectless:`, so the repair guard refused contradictory provenance | Seed nulls the legacy row's source space; mutation keeping the task on the shared row is caught |
| `workspace_identity_repair` `canonical_aliases_…` | `CreateSpace` restores the existing project for a folder (26f4b096), so the second project never existed | Second scope owns the fixture parent; mutation dropping project provenance is caught |

## Follow-ups

- An engine-side auxiliary Claude `--print` call runs in the process cwd; under
  test it writes `.cc09-fake.log` into `crates/engine`, and in production it
  inherits the daemon's cwd, where a stray `CLAUDE.md` or `.claude/` could apply.
- `provider_lifecycle_audit::lost_ordinary_steer_reply_never_replays` failed
  once under full-suite load and passed five isolated reruns.
- GitHub Actions starts no job on this repository: every workflow since at
  least 2026-10-09 fails in seconds with "recent account payments have failed
  or your spending limit needs to be increased". Until billing is fixed, CI
  verifies nothing, including the suites this audit routed into it and the
  Windows-only `harness/tests/windows_native.rs` repair.
- Production paths kept alive only by their tests, retained for a product
  decision: engine `integration_actions`, `persist_unwired_interrupt_with_id`,
  the prospective/private interrupt and synthetic-archive paths in
  `sessions.rs`, `backup::recover_interrupted_restore`, the Claude
  `SessionIntegrationReceipt`, and Codex skill observations.
- Example-embedded tests, the `appshots-fixture` examples and `LunaUITests`
  run in no workflow.
- Per-lane follow-ups are in each ledger.
