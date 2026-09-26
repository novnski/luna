# Sidebar notice ownership and presentation acceptance

Product source: `5d4e48c058cc224e27b58c4e5e80463e8c44c357` on
`codex/sidebar-notice-ownership`, based on main
`3a173e84a0d6f775499ef19e833e31709cf1ac74`.

All ten retained-Shell ownership cases pass after seven demonstrated baseline
ownership failures. Native QA confirms Status, Error and actual dismissal in
Light and Dark. The original unsent draft, all persisted QA settings and the
original parent view are restored. The primary LUNA Dev process and bundle
identity are unchanged. This final handoff changes only this report; the runtime
source and failure-first test module remain frozen.

## Runtime behavior

The existing sidebar notice now carries a fresh UUID, Status/Error tone and
message. Cloning retains ownership; two newly published notices with identical
wording have different IDs. Existing conversions default to Error. Side-chat
release progress, result-to-parent confirmation and busy guidance explicitly
use Status with existing normal text, border and accent-wash theme tokens.
Error retains the existing danger palette. Wording, geometry and typography
remain unchanged.

A release callback changes the notice only while its captured ID still owns it.
Dismissal checks the rendered ID before clearing. Newer confirmation, error,
progress or dismissal therefore survives an old callback. The runtime patch is
limited to `crates/ui/src/shell.rs` and `crates/ui/src/shell/side_chats.rs`.
There are no engine, lease, timeout, provider or send-behavior changes.

## Failure-first evidence

The failure matrix and all ten tests preceded runtime edits. Each case retains
the production Shell and uses `luna_rpc::memory_client` with held unload replies.
Handoff cases emit the real `SideChatEvent::ResultToParent` and verify composer
and durable draft. The full test module is byte-identical between corrected
baseline `a67cd531` and runtime fix `5d4e48c0`.

| Intervening action after release A starts | A outcome | Baseline | Fixed |
| --- | --- | --- | --- |
| New result-to-parent confirmation | Success | Fail | Pass |
| New result-to-parent confirmation | Failure | Fail | Pass |
| New error | Success | Fail | Pass |
| New error | Failure | Fail | Pass |
| Dismiss A | Success | Pass | Pass |
| Dismiss A | Failure | Fail | Pass |
| Release B with identical progress text | Success | Fail | Pass |
| Release B with identical progress text | Failure | Fail | Pass |
| No newer action | Success | Pass | Pass |
| No newer action | Failure | Pass | Pass |

The B cases also complete B and verify it clears its own progress. Callback
checks drain both executors after server return. Every fixed case preserves its
expected draft and parent transcript, with zero forbidden send/queue/command
mutations or real provider calls.

Evidence directories under `target/qa/sidebar-notices/`:

- `baseline-3ce0b9ce-20260926T065518Z/`: original run compiled, but all ten cases
  failed in fixture setup; it is not counted as ownership evidence. Exit 101,
  total 108.71s. Original logs retained.
- `control-b5b9121f-20260926T071627Z/`: bounded diagnostics isolated the
  `create-read-lease:held-side-1` stage, after draft restoration and create
  response but before read/lease. Exit 101, total 48.58s. Source contract showed
  the fixture lacked required `ChatConfig.sandbox`; no captured serde-error
  claim is made. The one-field repair supplied `read-only`.
- `corrected-baseline-a67cd531-20260926T073521Z/`: three controls passed and seven
  ownership cases failed, with no compile/setup failure. Tests 0.37s, total
  48.72s, exit 101. Ten raw case JSON files retain the failed behavior.
- `fixed-5d4e48c0-20260926T075122Z/`: all ten pass, tests 0.23s. The test command
  took 50.98s; format 6.62s; focused UI check 21.56s; Clippy 54.94s. All exit 0,
  combined 143.28s. Complete logs and before/after classification are retained.

Commands used `scripts/cargo.sh`, `CARGO_INCREMENTAL=0`, the default 25 GiB
reservation and `--locked` for test/check/Clippy. No guard exit 75 or silent retry
occurred. Existing compiler/lint warnings remain in the logs.

```sh
scripts/cargo.sh test --locked -p luna-ui --lib \
  shell::exit_regressions::sidebar_notice_regressions -- --nocapture --test-threads=1
scripts/cargo.sh fmt --all --check
scripts/cargo.sh check --locked -p luna-ui
scripts/cargo.sh clippy --locked -p luna-ui --lib
```

## Native acceptance

Packet: `target/qa/sidebar-notices/native-5d4e48c0-20260926T075656Z/`.
Only `LUNA QA Sidebar` (`com.novnski.luna.qa.sidebar`) was refreshed and
controlled. Its preserved profile is `target/luna-qa-data/sidebar` and its IPC
port is 59787. The single refresh built in 53s and completed in 54.31s, exit 0.
The exact running bundle process is PID 79512; strict deep codesign passed both
after refresh and at final readback. Binary SHA-256:
`037bdb49dbfc193fd9e977c7269abddcc30e19457b5086e2918968f93e1d5563`.
The QA root claim and device matched the saved profile. Cargo ownership was
released immediately after build and identity verification.

For each theme, the actual saved side reply's “Insert this result into the
parent draft” control was clicked once. The production confirmation read
“Added to the parent draft. Review it and press Send.” It used normal text and
accent wash. The persisted draft contained exactly one newly appended saved
result; Send was never pressed. The actual Dismiss control removed the banner.
The exact original draft was restored through the composer before proceeding.

For each theme, the existing synthetic task's actual “Restore earlier history…”
menu produced the real production refusal “No earlier-message recovery is
available.” Error text/border stayed red and its actual Dismiss removed it.

| Theme | Status | Status dismissed | Error | Error dismissed |
| --- | --- | --- | --- | --- |
| Light | `light-status.png` | `light-status-dismissed.png` | `light-error.png` | `light-error-dismissed.png` |
| Dark | `dark-status.png` | `dark-status-dismissed.png` | `dark-error.png` | `dark-error-dismissed.png` |

All eight images plus `initial-window.png` and `final-restored.png` were received
and visually inspected. Controls and text were readable with consistent
geometry. The coordinator independently inspected the four notice images.

Eight saved phase readbacks verify unchanged parent transcript and saved side
history, zero submissions/queue items, no active provider and no side runtime.
The two Status snapshots contain exactly the expected temporary insertion;
restoration snapshots match all original draft fields except revision/time.
Attachments, appshots, context and execution match exactly. One persistent
parent transcript subscription was used; the observer closed cleanly.

`ui-settings-before.json` and `ui-settings-after.json` are fully equal. Final
appearance is Light, the original parent is selected, the right pane is closed,
no notice is visible and the original unsent draft is present. Passive final
process/file reads confirm the main LUNA Dev PID 47463, port 59777, command,
binary and plist hashes are unchanged. No primary-app RPC or UI action occurred.

## Repeatable evidence and limits

The packet's `acceptance.json` hashes the native artifacts. `preservation.json`
rechecks all 31 earlier native artifacts, the three retained diagnostic/baseline
logs, corrected-baseline ten-case evidence, fixed ten-case evidence and current
runtime hashes. Re-run the saved-evidence verifier without touching either app:

```sh
agent-python target/qa/sidebar-notices/native-5d4e48c0-20260926T075656Z/verify-evidence.py
```

Its output is saved in `verification-result.json` and `verification.log`. It
checks hashes, exact draft transformations/restoration, queue/history/session
snapshots, settings and captured process identities. Visual conclusions remain
explicit screenshot observations; this offline verifier does not re-observe a
live process or infer colors from pixels.

Native coverage establishes real Status/Error rendering and dismissal. Native
unload-error injection and same-text races were not performed; held-RPC Shell
cases establish that ownership behavior. No new provider completion or native
subagent was requested during this acceptance packet.

The returned AX tree exposed the handoff and appearance controls but not the
notice/Dismiss; observed screenshot coordinates were used for dismissal. Some
Settings targets returned stale IDs or no-frame errors and were reacquired from
fresh screenshots. Both draft-restoration paste calls reported a clipboard
read timeout despite applying correctly; exact durable readback verified each
restoration and neither paste was replayed. `native-observations.md` preserves
these limitations and the observed AX excerpts.

QA app/data, build cache, earlier evidence and the original draft are retained.
No task-created disposable target directory requires removal. Final disk/source
receipts and the report-only commit identity are saved in the native packet.

## Coordinator integration and primary app handoff

The coordinator verified all 59 native and 66 preserved artifact hashes, all
eight state snapshots, the four notice screenshots and final restored view.
Handoff `6df0087a` merged without conflicts as local `8399ca21`; the integration
tree exactly matched the reviewed handoff. Main's four pre-existing local files
remained byte-identical and were not included in the merge.

Primary `LUNA Dev` refreshed from `8399ca21` in 54s build time, 57.19s total.
Exact bundle PID 36151 owns IPC 59777; strict deep codesign passed. Live engine
device and owner-root claim match the preserved primary data directory. Binary
SHA-256: `fd0488aa75c3771a69fcc6827ada3c3899ddb74a54b3068b411923a77b062206`.
Evidence is in main's
`target/qa/parallel-audit-2026-09-26/sidebar-notice-primary-handoff/`.
Aggregate target/cache accounting was 142.476 GiB afterward, below 170 GiB;
main `target/debug` was 28 GiB. No push, deployment or cache deletion occurred.
