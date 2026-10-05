# Slice 1 verification — 2026-10-05

The first delivery adds versioned, owning-host execution observations to
`WatchChats` and `GetRuntimeContract`. It distinguishes the saved selection
from the admitted runtime, identifies pending changes, explains the selection
transition, and rejects native steering while those changes would be ignored.
Queued input stays durable and requires explicit delivery. Composer chips,
queue actions and the focused-chat Execution card use the same observation.

## Evidence and checks

Evidence root: `target/qa/harness-evolution/`. The final `SHA256SUMS` records
source patch, logs, JSON readbacks and rendered captures. Mutable QA profile
data is preserved separately and excluded from that manifest.

| Check | Result |
| --- | --- |
| Engine E2E | 31 passed, 2 unrelated live tests ignored |
| Message queue E2E | 34 passed |
| Task commands E2E | 36 passed |
| Claude child permission ceilings | 4 passed |
| Codex permission subprocess receipts | 1 passed; six start/resume policy combinations |
| Claude live-control protocol | 5 passed |
| Claude permission protocol | 10 passed |
| Existing UI library suite, serial | 1,185 passed |
| Production Shell composer fixture | c01–c11 and e01–e08 passed |
| Workspace formatting and affected-crate Clippy | Passed; existing warnings remain |
| Signed native macOS app | Built in 94 seconds; exact process, port and signature verified |
| Real Codex and Claude providers | Both permission-transition scenarios passed |
| Native app restart | Both saved chats have no stale live observation; no duplicated message |

The existing permission E2E was extended before implementation. Its initial
failure recorded the missing snapshot. A later assertion exposed requested
sandbox values incorrectly inherited from legacy storage; the preview now
uses the same policy resolver as dispatch. Both Full → Ask and Ask → Full
retain the old running policy until replacement starts. Child creation
ceilings, current parent ceilings and explicit user grants remain distinct.

Queue scenarios cover model, reasoning, options, permissions and a different
account with the same model name. A refused steer stays held, reaches no old
runtime, and appears once when explicitly sent. Private options and native
session identifiers are absent from public execution snapshots. Inherited
defaults do not become false pending changes.

The UI fixture verifies running/requested chips and tooltips, cleared pending
state, warm-runtime labels, absent/unknown-schema/wrong-task snapshots,
focused fork diagnostics, late response rejection, watch invalidation and
engine-generation fencing. It measures the Execution card against the real
settings scroll viewport before capture. The first capture missed the card;
the corrected `composer-final/e07-execution-card.png` was visually reviewed.

## Native repeatable acceptance

Use the four QA identity overrides from the QA infrastructure guide:

```sh
LUNA_DEV_APP_NAME='LUNA QA Harness' \
LUNA_DEV_BUNDLE_ID='com.novnski.luna.qa.harness' \
LUNA_DEV_IPC_PORT=59809 \
LUNA_DEV_DATA_DIR="$PWD/target/qa/harness-evolution/native/data" \
scripts/run-macos-dev.sh --refresh
node scripts/qa-execution-summary-live.mjs codex
node scripts/qa-execution-summary-live.mjs claude-code
```

Quit and reopen that exact app gracefully, then run:

```sh
node scripts/qa-execution-summary-live.mjs verify-restart
```

Only one LUNA GUI may run. Each provider invocation creates a fresh named
evidence directory and records IDs before sending; a timeout never replays
the uncertain send. The runner rejects the wrong signed bundle, listener or
data path before mutations. It discovers the live model catalog and uses
normal provider authentication without copying credentials.

Real acceptance used Codex CLI 0.160.0 with `gpt-6-luna` and Claude CLI
2.1.289 with the catalog default `claude-fable-5-1`. Each performed one
15-second sleep/print command. While it ran, Full → Ask left Running at Full,
Requested at Ask and Steer unavailable. The held message was absent from the
transcript until explicitly sent after completion, then ran under Ask and
appeared once. The native Execution card was also inspected and correctly
labelled the parked Codex runtime "Loaded, between turns".

## Boundaries

Capabilities are adapter declarations, not blanket claims of installed-CLI,
account or session support. Running settings describe the configuration
admitted after session start; native readback confirms only the fields the
provider exposes. No live OpenCode model session was exercised in this slice.

The transition is a selection preview. Actual dispatch still checks current
runtime instructions, tool contracts, account/workspace ownership and unknown
acceptance. This slice does not replace those admission boundaries or add
automatic context transfer. Remote tasks do not receive local capabilities.

Portable context/merge-back, background-work lifecycle, additional proven
recovery gaps, scheduling/PR wakeups and measured history/pane improvements
remain the later slices listed in the roadmap.

The reviewed QA bundle was stopped gracefully, unregistered and removed after
its evidence was hashed. Its data profile is retained; disposable project
fixtures were archived before removal. Primary LUNA Dev was refreshed in
4 seconds and its exact process, IPC listener and strict code signature were
verified. Source, primary user data and the shared build cache were preserved.
