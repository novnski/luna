# Chat workbench verification

Acceptance record for the 2026-10-04 implementation. Evidence is retained under
`target/qa/super-chat-workbench/` (ignored runtime output). The source fixtures
and native driver make the checks repeatable. Failure cases were written before
implementation; no new unit tests were added.

## Backend

| Area | Confirmed result | Evidence |
| --- | --- | --- |
| Workbench RPC and persistence | Six integration scenarios pass: nested layouts, protected Main, close/reopen/restart, empty-pane focus, mixed-axis balancing, optimistic revisions, idempotency, profile isolation, stale writer rejection, invalid inputs, missing-chat cleanup, storage failure and corruption preservation | `engine/layout-*.json`, `engine/empty-pane-focus.json`, `engine/balanced-panes.json` |
| Child and side lifecycle | Eleven pane/child/side cases, four Claude child cases and thirteen side-chat cases pass on baseline `455caac0` plus this feature | `engine/resume-tests.log` |
| Blank child lifecycle | Zero provider starts before first Send, independent retained drafts, restart/retry, a running turn surviving view close, explicit Stop, and retained side-fork identity | `engine/blank-child-workbench-lifecycle.json` |
| Encrypted backup and restore | 26 cases pass, including exact layout/ratio/closed-history restoration and pending-file exclusion | `engine/layout-backup-roundtrip.json`, `/tmp/luna-workbench-engine-regressions.log` |
| Cross-task and bridge boundaries | Five cross-task and two pane bridge/approval cases pass | `/tmp/luna-workbench-engine-regressions.log` |
| Compilation and lint | Focused backend checks and Clippy pass, with existing repository warnings | `/tmp/luna-workbench-*.log` |

The empty-pane focus scenario first failed against the real RPC's unknown
operation response. Its passing rerun also proves atomic rejection of invalid
targets, no-op repeated focus, unchanged chat records and no provider starts.

## Production UI fixture

Build and run from the repository root, with every other LUNA GUI stopped:

```sh
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example chat-workbench-fixture
target/debug/examples/chat-workbench-fixture "$PWD/target/qa/super-chat-workbench/replay"
```

The fixture drives the production Shell with pointer and keyboard events
against an in-process engine and Mock harness. `chat-workbench.pass` together
with `readback.json` and exit zero is required. A failed run writes
`failure.txt` and `failure.png`; earlier milestones remain in
`readback-progress.json`.

Final run 17 passed nested drag/split/reorder, minimum sizes and divider equalize,
protected Main, blank-child creation, separate drafts/send, real Stop routing
(Child interrupted, Main completed), child model selection, Tab focus and
checkout context, active-tab reveal, close/reopen/reload, profile switching,
empty-pane focus and creation, two independent ended histories, and all
sidebar states. Captures w21-w28 cover the unified frame, sidebar resize,
a transition pinned at 100 ms and its reduced-motion equivalent, right-panel
headers, and eight representative themes.

The final run also passes single-move tab/Related-row/divider drags, Escape
before any preview, the next independent gesture after cancellation, a real
Review button click at both 1220 and 980 px widths, and End immediately followed
by Cmd-/ without another click. Main's picker stays closed and the ended tab
keeps keyboard ownership. With both sidebars open, pane minima are preserved;
real wheel/Tab events scroll and reveal groups, manual scrolling stays put,
sidebar drops are refused, and widening restores the same saved structure and
revision. Composer width and screenshots verify that the controls no longer
overlap; a pane wider than the viewport is reachable through horizontal scroll.

Runs 12-15 retain the earlier cancellation/focus failures and fixture targeting
failures caused by confusing canvas scrolling with tab-strip scrolling. Their
temporary runtimes were archived and hashed before scoped removal. The final
acceptance files are `ui-attempt-17/readback.json` and
`ui-attempt-17/chat-workbench.pass`; build 22 took 48.85 seconds. The final
combined UI/engine/proto/RPC Clippy run passed in 18.97 seconds with repository
warnings.

The existing UI library suite passes **1,195 tests**, zero failures, serialized:
28.36 seconds compilation and 8.27 seconds execution. Workspace formatting and
`git diff --check` pass. Final UI logs are copied into `checks/`; the final
engine/proto/RPC Clippy run passed in 22.90 seconds (`engine/final-clippy.log`).

The follow-up balancing scenario first failed at the real RPC's unknown
`balancePanes` operation, then all six backend cases passed (19.23 seconds
compilation, 0.77 seconds execution). It proves three/six normalized equal
areas, multiple tabs counting as one pane, empty-pane weighting, the sixteen-pane
ratio bound, one revision, no-op/replay/conflict behavior and restart retention.
Run 17 adds real button clicks for the three/six-pane arrangements and a
right-sidebar opening with the right pane focused: the left viewport anchor
stays at zero, and explicit later focus still reveals its target.

## Appearance

The final color matrix in `appearance-attempt-3/` passes **810 combinations**:
30 built-in theme variants, three surface preferences, and nine accent/diff
settings. It records 21,375 pairs and zero failures. Recorded minima are
4.5827:1 for text and 3.1018:1 for non-text. Earlier failed runs are retained,
including the corrected compositing model and the hue-preservation finding.

The separate settings fixture verifies persistence/reload, light/dark and
Catppuccin captures, opaque/frosted surfaces, and interface/code font sizes.
See [the appearance audit](appearance-audit.md) for the measured UI families,
rendered surface samples, fixes, performance measurements and exact limits.
The measurements do not establish full accessibility conformance. Custom
palettes are not enumerated; application-controlled terminal true colors are
preserved; macOS Increase Contrast/Reduce Transparency are not integrated.

## Native provider acceptance

`scripts/qa-chat-workbench.mjs` accepts only the signed **LUNA QA Workbench**
bundle, its exact executable process, port 59805 and isolated QA data directory.
Do not run `seed` again against its existing manifest. The driver provides
`snapshot`, `verify-drafts`, `verify`, `checkpoint-restart` and `verify-restart`.

Confirmed on the first signed QA build:

- The real `+` button created two user children of Main with zero messages and
  no provider binding before Send. Closing and reopening retained Child B's
  exact draft.
- Main and Child A each received their own real Codex sentinel response;
  Child B remained unstarted.
- A side chat created from Child A's assistant reply used `nativeFork` and
  its own provider conversation. It received its own sentinel, retained its
  binding through close/reopen from Main's Related library, and retained the
  full history after explicit End and another close/reopen.
- Native menu actions created nested panes. After fixing release handling,
  real native divider, tab and Related-row drags pass, preserve the draft and
  do not duplicate conversations (`verified-divider.json`,
  `verified-native-drags.json`).

The authoritative records are `native/verified-drafts.json`,
`native/verified-side-reopen.json` and `native/verified-native.json`.
Ended-side binding evidence uses the isolated QA profile's identity ledger
read-only, because the public generic task-identity RPC deliberately refuses
ended side chats. The active binding was independently compared with the
public RPC; the driver checks that the one binding was not rewritten.

The final signed QA build took 42 seconds. Its exact bundle process, listener
on 59805 and strict code signature were verified. The real Balance button
changed the two-over-one layout from 1/2 to 2/3 with one revision and unchanged
structure/focus (`native/verified-balance.json`). The real Review button opened
Working tree for QA Other checkout and showed its clean checkout. Native
captures n10-n12 show the focused right pane covered by the right sidebar while
the left panes stay anchored; n14 records a real Related-row move.

A graceful quit/relaunch changed the process ID while preserving the complete
layout, focus, closed history, drafts, transcript entries and managed binding
IDs (`native/verified-restart.json`). Readback records the binary hash, bundle
ID, port and data directory. The QA profile and inactive bundle are retained
for the requested next milestone: replacing the side-specific lifecycle with
ordinary persistent fork threads. No second LUNA GUI runs concurrently.

The primary **LUNA Dev** app was refreshed in 4 seconds and verified running
from `target/luna-macos-dev/LUNA Dev.app`, with its expected 59777 listener and
successful `codesign --verify --deep --strict`. Logs are in `checks/`. The
completed native milestone is copied to `native-workbench-final/` before any
later side-thread migration changes the QA profile. `SHA256SUMS` indexes the
retained evidence; failed/successful fixture runtime archives are kept with
their respective attempts. The primary data and shared build cache remain.
