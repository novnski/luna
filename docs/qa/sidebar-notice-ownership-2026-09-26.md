# Sidebar notice ownership: failure-first checkpoint

Base: committed main `3a173e84a0d6f775499ef19e833e31709cf1ac74`.
Branch: `codex/sidebar-notice-ownership`. The previous accepted sidebar branch,
QA app, data, evidence, and unsent draft are retained. This batch initially
changes test/design source only. No Cargo, native UI, provider, or engine work
is authorized at this checkpoint, and no runtime fix has been made.

## Failure matrix recorded before test or runtime changes

The production Shell owns a release callback after dropping its side panel.
The RPC response is held while that same Shell receives a newer UI action.
Both successful and failed old responses must be considered independently.

| Case | Intervening action after release A starts | Required result when A completes |
| --- | --- | --- |
| Handoff / success | A fresh panel emits the real result-to-parent event | New confirmation remains; existing draft plus one result stays unsent. |
| Handoff / failure | Same | Old failure cannot replace the newer confirmation. |
| Error / success | A newer error occupies the notice | New error remains. |
| Error / failure | Same | Old failure cannot replace the newer error. |
| Dismiss / success | User clears A's progress notice | Notice stays absent. |
| Dismiss / failure | Same | Old failure cannot resurrect a dismissed notice. |
| Same text / success | Release B starts with identical progress wording | B still owns progress; only B completion may clear it. |
| Same text / failure | Same | A failure cannot replace B's progress. |
| Current / success | No newer action | A clears its own progress. |
| Current / failure | No newer action | A's failure remains visible and uses error presentation. |

Every case retains the production Shell, creates the panel through the real
Shell path, supplies create/read replies through `luna_rpc::memory_client`, and
calls production `release_side_chat`. No duplicated release callback or model
of the notice state machine is accepted as evidence. The handoff cases emit the
real `SideChatEvent::ResultToParent` into the Shell subscription and verify the
composer and persisted draft. The fixture records any attempted provider or
queue mutation; it never starts a provider. Dismissal initially exercises the
same state mutation as the existing dismiss callback; native clicking remains
a later readback requirement.

Expected baseline behavior, not yet measured: current-success, current-failure,
and dismissed-success pass; the seven newer-notice/failure-resurrection cases
fail because the old callback assigns `sidebar_notice` unconditionally.

## Narrow presentation proposal

Runtime work is limited to `shell.rs` and `shell/side_chats.rs`: associate
Status/Error and a unique ownership token with the existing notice payload.
Preserve existing producers' behavior through default Error conversions and
existing message comparisons. Do not rewrite unrelated callers or route these
messages through activity notifications, sounds, or engine state.

Handoff confirmation, draft-busy guidance, and release progress are Status;
actual current unload/readback failures are Error. Adopt the existing activity
notice's theme tokens (`accent_wash`, normal text and ghost dismissal) for
Status, retaining the existing compact sidebar placement. Keep the Error
palette. No typography, spacing system, theme palette, or engine lifetime change
is proposed. Two identical progress strings must have different ownership.

The release callback may clear or replace only its still-current owned notice.
Dismissal retires ownership. A newer notice also retires it, regardless of tone.

## Verification gates

This first checkpoint contains the matrix and executable retained-Shell test
source, not a pass claim. Run the baseline only after the coordinator grants a
Cargo slot, keeping the failed JSON/log artifacts before changing runtime code.
The test-only environment variable `LUNA_SIDEBAR_NOTICE_EVIDENCE_DIR` writes a
separate JSON result per case before asserting, with event order, expected and
actual notice, draft text and RPC mutation counts. Do not point it at earlier
accepted evidence.

After the narrow fix, rerun the same cases, the focused UI compile/format/lint
checks, then use a separately granted QA app refresh and native readback. Native
acceptance must show Status in Light and Dark without error-red styling, a real
current failure still red, successful handoff retaining the unsent draft, and
the actual dismiss control retiring notice ownership. These test-context cases
assert notice ownership and draft behavior; they do not claim pixel or theme
verification. Existing native proof remains unchanged.

## Prepared source and readback

The ten cases live in
`shell::exit_regressions::sidebar_notice_regressions`, reusing
`command_palette_test_window`, `init_handoff_shell_test`, `ShellDraftRpc`, and
the existing GPUI/Tokio pump helpers. Each unload has a distinct oneshot gate;
the same Shell survives panel release and callback delivery. The second-release
cases also complete B, checking that its own successful response clears its
progress. Callback checks drain both executors after server return; a server
return counter alone is not treated as completed UI delivery.

Source verification passed `rustfmt --edition 2024 --config skip_children=true
--check crates/ui/src/shell.rs` and `git diff --check`. Removing only the new
test module reproduces the base `shell.rs` byte for byte; `side_chats.rs` is
unchanged. All 31 hashes from the prior native acceptance manifest still match.
`target/qa/sidebar-notices/preparation/source-checkpoint.json` retains these
checks. No test binary was compiled or executed, and no baseline failure has
yet been observed. There is no new provider call or native screenshot claim.

Once separately granted, the bounded baseline command is:

```sh
CARGO_INCREMENTAL=0 \
LUNA_SIDEBAR_NOTICE_EVIDENCE_DIR="$PWD/target/qa/sidebar-notices/baseline" \
scripts/cargo.sh test --locked -p luna-ui --lib \
  shell::exit_regressions::sidebar_notice_regressions -- --test-threads=1
```

Capture its original exit status and complete output in a baseline log. Fixture
or compile failures must be resolved and identified separately from the expected
notice-ownership failures before any runtime implementation proceeds.
