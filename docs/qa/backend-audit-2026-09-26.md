# Backend audit — 26 September 2026

## Scope and provenance

This review mapped the 302 commits attributed to `noviulian` at the starting
revision `a6ca1085`; the checkout's SSH remote is `novnski/luna`. Investigation
focused on the heavily changed engine/session, provider, transcript, historical
usage and model-catalog paths, with regression coverage for queues, identity,
history, profiles, backup/restore and owner authorization.

The theme work and Cargo cache integration were being developed concurrently.
Their changes were preserved. This audit changes no UI layout, theme, color,
border, radius or component styling. The verified fixes are checkpointed locally
before the user-requested parallel worktree audit; nothing is pushed by this run.

## Reproduced defects and corrections

| Defect | Evidence before correction | Correction and regression |
| --- | --- | --- |
| Tool details disappear when completion arrives after a steer | The original tool chip was resolved but its output preview and expansion references were absent. | Apply the normal result fold to the original chip, including privacy filtering, output bytes and diff references. E2E verifies placement, full journal output and snapshot round-trip. |
| Equal-sized Codex calls are undercounted | Two distinct calls, each using 20 tokens, produced a total of 20 instead of 40. | Correlate repeated snapshots using cumulative usage as well as per-call counts. Verify cold and warm scans. |
| Archived Codex sessions disappear from cold usage history | An archive-only fixture returned zero instead of 20 tokens. | Scan `archived_sessions` too; deduplicate records across a cached old path and its new archive path. |
| Invalid usage boundaries panic the scanner | A boundary beginning at `i64::MIN` caused subtraction overflow. | Checked arithmetic rejects the invalid window and uses the default period. A subsequent scan remains usable. |
| OpenCode silently starts a fresh session when resume lookup fails | Both a missing session and a simulated HTTP 503 caused a new session creation attempt. | Surface the failed lookup, preserving the requested conversation binding. Verify no new session or prompt is sent. |
| Closed engines keep refreshing provider model catalogs | The retired-engine fixture counted a provider probe after shutdown. | Cancel periodic discovery during both graceful shutdown and engine drop, including the initial delay and in-flight async probe. |
| Provider discovery fails with a sparse shell PATH | The existing interactive-shell fallback regression failed independently on macOS when PATH excluded `/usr/bin`. | Run `/usr/bin/env` directly when capturing the shell's PATH. The existing failing regression passes. |

The usage, late-tool and model-refresh defects trace to `noviulian` changes
(`1d2c5ebe`, `8074b4c0`, `ed57c96a`, `8db8675b`, `58dd47b3`, `1a7d8e9c`).
The OpenCode fallback and shell probe also include inherited upstream code;
they are included because they affect the same backend flows.

## Verification repair

Several existing tests no longer exercised the current contracts:

- The old E2E helper inserted raw remote document commands. Owner admission
  correctly refuses these today. It now uses the same admission path as the
  composer; separate remote-authority tests still verify foreign-row rejection.
- Codex steering fixtures returned `{}` instead of the installed CLI schema's
  required `turnId`. Tests now check one boundary for a correlated ACK and no
  boundary/replay for missing or mismatched IDs, lost replies and rejections.
- Interrupted transcripts can settle before their command receipts. Metadata
  mutation and model-change tests now wait for the relevant settled state or
  applied receipt before checking the next operation.
- The generated-image test compares canonical macOS paths, and the RPC test
  includes the current context-control and replay-baseline fields.

New regressions are filesystem, subprocess, or engine/RPC integration tests.
No new unit tests were added. Existing crate tests are also run.

## Repeatable commands and artifacts

```sh
scripts/qa-backend-audit.sh
scripts/qa-backend-audit.sh --live-codex
scripts/run-macos-dev.sh --refresh
node scripts/qa-usage.mjs 1 7 30 90
```

The runner uses `scripts/cargo.sh` and the installed Mr Boxington integration,
retains Cargo's shared target, and saves commands, full logs, exit codes and
cache statistics under a unique `target/qa/backend-audit.*` directory. The
`--live-codex` option consumes account quota and creates two native QA sessions
using `gpt-6-luna` at High reasoning. It checks an exact `pong` reply, a harmless
shell command, one acknowledged steer, command-output preservation and the
literal final response `LUNA_AUDIT_STEER_OK`.

Final verification: **928 passed, zero failed**. Eighteen other opt-in tests
were ignored; the two selected real Codex checks were explicitly executed.

| Check | Result |
| --- | --- |
| Formatting, diff whitespace, script syntax and ShellCheck | Passed |
| Locked check: engine, harness and doc crates | Passed |
| Existing document tests | 119 passed |
| Provider crate and subprocess integration tests | 220 passed |
| Engine crate plus 15 integration suites | 587 passed |
| Real GPT-6 Luna reply and tool/steer checks | 2 passed |

The complete final run is saved in
`target/qa/backend-audit-2026-09-26-final/`: `results.tsv`, `test-counts.json`,
the command/log pairs, build-cache statistics and `reproductions/` containing
the failing-before-fix evidence. The initial broader run is retained separately
at `target/qa/backend-audit-2026-09-26/` so repaired failures remain reviewable.

Live results: `pong`, `LUNA_AUDIT_TOOL_OK`, accepted steering with exactly one
boundary, and `LUNA_AUDIT_STEER_OK`. The four native QA sessions created across
the two live runs were archived after completion. Provider output and prompts
in these fixtures contain only the fixed QA sentinels.

LUNA Dev rebuilt in **58 seconds**. Its exact bundle process, localhost IPC on
`127.0.0.1:59777`, and strict deep code signature were verified. The app build
restored 63 files and avoided 21 compiler invocations through Mr Boxington.

Live `GetUsageSnapshot` readback passed for 1, 7, 30 and 90 days: every response
was complete (`partial=false`) and every bucket matched the requested period
starts. Cold/resumed reads took approximately 20, 12, 32 and 67 seconds. See
`live-usage.jsonl`. Unpriced records remain explicitly unpriced; the audit does
not invent pricing for unsupported models.

The debug target reached approximately 52 GiB. A dev-profile cleanup was
previewed but **not executed**: the user requested a parallel workspace plan
and an increased aggregate build-storage budget, never above 200 GiB. The
existing target and shared cache remain available for the parallel audit.

## Practical limits

- Real provider testing covers Codex/GPT-6 Luna. Claude and OpenCode protocol
  coverage uses fixtures, not live accounts.
- The native handoff is macOS. Linux/Windows GUI behavior and real multi-device
  network sync are not verified by this run.
- The usage parser cache version changes so existing transcripts are reparsed
  once. A large first scan may be partial at the existing 90-second budget;
  refresh resumes it. Provider transcripts are read-only.
- This is an author-focused backend audit with explicit coverage and
  reproductions, not a proof that the whole application is free of defects.
