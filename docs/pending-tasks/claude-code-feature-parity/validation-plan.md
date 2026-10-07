# Validation and delivery plan

Status: proposed checks. None of these runtime scenarios was executed by the research audit. Existing fixtures and comments are useful inputs, not evidence that a new capability works with the current installed Claude/account.

## Failure cases to record before implementation

The implementation owner must establish expected outcomes for these cases before writing isolated system code. Prefer production-path E2E, not unit tests added after implementation.

1. Native CLI/version/account/cwd differs between discovery and execution; unknown capability or unavailable model is presented as supported.
2. Tool executes but result content, error, child attribution or output reference is lost; a second native execution is triggered to repair display.
3. Partial and complete frames duplicate tools/results; repeated native IDs overwrite another task generation; malformed/unknown semantic events disappear.
4. Image works initially but becomes text on warm/steer/resume; image-only input fails; unreadable/oversized images are silently removed.
5. Approval is answered twice, native cancellation races a late allow, recovery answers a different request, or unknown control leaves Claude waiting.
6. Ask/Auto labels do not match native permission enforcement; unavailable sandbox is presented as protection; organization deny is bypassed.
7. Queue stdin acknowledgment is labelled model incorporation; duplicate text is incorrectly matched; ambiguous retries repeat a mutation.
8. App MCP bridge advertises tools without working handlers, leaks cross-task authority or accepts stale session tokens after closure.
9. Root completion hides active child/background work; output is routed to the wrong agent; task notification resurrects a settled generation.
10. Native peer text is treated as a human instruction; app child creation duplicates on retry or shares a parent's native session.
11. Fork changes the source session; rewind restores untracked shell files without scope evidence; crash leaves provider and UI bound to different histories.
12. Plugin/skill inventory shows installed as attached, stale cwd/account discovery persists, command expansion discards prose/images, or reload duplicates MCP connections.
13. Goal/schedule/monitor state becomes Idle falsely; app and native timers duplicate; a stopped native task remains active in UI.
14. Hosted service is unavailable but called supported; browser/OS access, publishing or sharing occurs outside user scope.
15. Observer polling changes scheduling, delayed delivery is labelled failure, artifacts contain credentials, or cleanup deletes active/cache/user data.

## Test layers and evidence limits

- **Captured-native replay through the real harness process boundary:** deterministic JSONL fixtures feed the production parser, normalizer, engine, journal, RPC and UI. Cover complete native shapes and future-event fallbacks. A replay validates transformation and recovery, not real Claude execution or account eligibility.
- **Real bounded native sessions:** run the selected installed CLI with disposable workspace/input fixtures and controlled tools, retaining native frames and outcomes. Use actual endpoint/provider data before blaming a renderer.
- **Native UI E2E:** exercise the shared LUNA composer, tool expansion, approval, task/history/settings/review surfaces, with actual RPC/readback and screenshots where useful.
- **Hosted/platform verification:** a separate live scenario per claimed remote/cloud/browser/computer-use service and account/OS configuration. An inaccessible gate remains blocked/unverified.

Use persistent transcript subscriptions throughout a scenario. Do not repeatedly open/cancel `WatchDocMessages` as a polling loop. Snapshot subscription opens can schedule reconciliation and change the behavior being observed. Retain subscription timing/observer side effects and use bounded readback only at milestones. A timeout is not by itself proof of semantic nondelivery.

## Scenario matrix

| ID | Production-path scenario and expected outcome | Repeatable artifact |
| --- | --- | --- |
| E01 | Exact executable/version/hash plus native initialization; older/unsupported version produces explicit capability state | Sanitized help/init, launch metadata, capability ledger |
| E02 | Account/cwd-sensitive model/command discovery, stale cache refresh and unavailable option rejection | Discovery before/after, selected/effective snapshots |
| E03 | Start → warm turn → resume → interrupt → graceful close; inject unknown control, EOF and child exit | Raw control/stream frames, process tree, correlated lifecycle timeline |
| E04 | Built-in root/child tool success/error/empty/large output, including output above synchronized preview limit | Native result hash versus host retrieval hash; bounded doc preview |
| E05 | Mixed text/image/resource/structured MCP result, partial tool JSON and unknown content/event | Ordered block manifest, coverage ledger, lazy RPC/UI captures |
| E06 | Rich permission/question/form/URL requests, concurrent requests and one-shot responses | Request/context/answer/cancel timeline, UI and native reply evidence |
| E07 | Native managed deny, actual workspace/network enforcement, Auto unavailable and sandbox failure | Effective policy snapshot, harmless denied fixture outcomes |
| E08 | Same image fixture initially, image-only, warm, active steer and resumed; invalid/corrupt/oversized/missing images | Attachment hashes, serialized blocks, native response evidence and explicit errors |
| E09 | Notebook/LSP/web/task/findings tools plus schema-valid and schema-failing structured output | Native result/provenance/diff/schema outcome and reopened UI |
| E10 | Live model/effort/settings changes with usage/cache/context/limit/retry events | Acknowledged/effective readback and accounting totals |
| E11 | Identical input text sent twice; interrupt before/after write; reconnect with ambiguous delivery | Command/native UUID/receipt ledger, explicit unknown state, no duplicate mutation |
| E12 | Real app bridge tool discovery/call/error/cancel; stale token/cross-project request rejected | Catalog hash, native tool trace, engine authority/revocation evidence |
| E13 | Nested agents, background shell, root finishes early, late notification, failed/stopped/reused task IDs | Task graph with generations, child transcript, output/usage and terminal readback |
| E14 | Authorized persistent child creation/retry/restart and native peer message arrival | Independent native IDs, lineage record, branch/cwd, destination/origin authority evidence |
| E15 | Native history list/read/fork at real UUID; source unchanged; file rewind preview and interrupted binding update | Source/destination message hashes, preview, intent log and recovery state |
| E16 | Existing Executor connection, auth/disconnect state, MCP search/resource/reload; gated Apps widget | Source/effective inventory, connection call, resource hashes, widget origin checks |
| E17 | Native scopes/collisions, skill mention with image/prose, plugin/style reload, hook deny/abort and LSP failure | Effective roots, command dispatch, hook/plugin lifecycle and settings readback |
| E18 | Workflow interleaved members/progress/retry/stop; experimental team disabled/enabled; channel message | Native task/workflow graph, opt-in evidence, origin/cancel/result timelines |
| E19 | Bounded native goal start/status/verdict/clear/error; session cron resume/delete; monitor deadline/stop | Native state before/after, schedule IDs, task output and no duplicate timers |
| E20 | Review pinned revision → findings/empty/failure → reopen after restart; stale source rejected | Reviewed SHA, native findings, file/line links and result ownership |
| E21 | One scoped scenario per claimed hosted/device integration, including stop/revoke/readback | Account/OS/service gate, native IDs, assets/jobs and external state readback |
| E22 | Restart/crash/auth expiration/network/proxy/limits/idle exit and supported provider/architecture matrix | Compatibility rows, recovery logs, pending-state reconciliation and process cleanup |

Fixtures should include a known marker in shell output, a controlled file/notebook edit, a locally generated image with a fixed hash, and a controlled MCP result containing mixed blocks and structured content. Native tool-choice nondeterminism must be recorded; use deterministic captured frames to verify a specific normalization branch and a separate real execution to prove native delivery. Do not silently substitute a mocked server for a required real integration pass.

## Artifact contract

Create a retained directory per scenario/run, under a task-scoped evidence location. At minimum include:

- `manifest.json`: scenario ID, UTC time, source SHA, selected CLI/transport/dependency versions, binary hash, sanitized account identifier, cwd, capabilities and configured/effective policy.
- `commands.jsonl`: exact repeatable commands, environment variable **names** and non-secret values, exit status, duration and stdout/stderr log paths.
- `native-frames.jsonl`: permitted raw provider frames with secret-bearing headers/tokens removed; preserve ordering, IDs and a redaction manifest. Keep sensitive user/tool content host-local with appropriate access.
- `normalized-events.jsonl`, `state-before.json`, `state-after.json`, subscription/RPC captures and blob/attachment manifests.
- `observer.json`: subscription lifecycle, milestone snapshot calls, timeout policy, scheduling side effects and known measurement limitations.
- UI screenshots or video when they substantiate interaction, plus exact fixture assets and cleanup ownership.
- `SHA256SUMS` and `result.md`: hashes, actual outcomes, failures, unverified claims and replay instructions.

Record both original content hash when safely available and redacted artifact hash; never describe a redacted capture as byte-identical raw evidence. Do not capture credential files, Keychain data or authorization headers. Full provider output may contain user material; it is not automatically eligible for source-control/public export.

An E2E run passes only when its expected native, persisted and UI outcomes match. A screenshot proves presentation, not provider execution. A successful RPC write proves admission, not model consumption. A model's statement that it completed work is not an independent verification artifact.

## Required build and native handoff during implementation

This documentation-only research requires no Rust build or app refresh. For later meaningful code changes:

1. Read current `AGENTS.md`, relevant manifests/CI and `docs/reference/qa-infrastructure.md`. Check `python3 scripts/build-budget.py --status` before builds. Use the primary checkout's existing target; do not launch parallel compilers or allocate disposable target trees casually.
2. Run applicable focused compile through `scripts/cargo.sh check --locked -p <actual-package>`, required formatting/lint and production-path E2E. Keep flags/profile stable, save verbose logs and preserve original exit statuses. Exit 75 is a busy/budget blocker to coordinate, not bypass.
3. Perform native QA with only one Luna GUI app. Let the existing app save and quit gracefully. Follow all identity/port/data overrides if using a separately authorized QA bundle; never replace primary identity from another checkout.
4. Run `scripts/run-macos-dev.sh --refresh` for the primary handoff. Verify the exact bundle process, expected IPC listener and `codesign --verify --deep --strict 'target/luna-macos-dev/LUNA Dev.app'`. Record build duration and limitations. Do not force-quit an app that declines to close.
5. Follow the repository delivery skill when feature delivery is authorized: scoped staging, immutable review, conflict resolution, verified merge/main push and remote SHA readback. Preserve evidence before cleanup.

The aggregate build operating ceiling is 170 GiB with reserved growth; follow the live guard rather than treating cache size as an available allocation. Preserve the primary app/data and `target/debug`. Never run broad `mbx clean` or unqualified Cargo cleanup.

## Release and cleanup gates

For each work package: source SHA reviewed; required checks passed; native/engine/UI evidence retained; claimed capability state updated. A feature is delivered only after applicable primary app verification and main/remote delivery, not when an implementation worker posts a report.

Remove only the package's proven disposable artifacts after all processes/workers using them are stopped. Preserve source/Git history, evidence, data and shared caches. Verify status and disk accounting after cleanup. Archive any associated chat only under the workflow the user authorized; this plan itself creates no chat.

The final release report groups verified core, advanced native and hosted coverage, then lists explicit product exclusions and remaining external blockers. It must not claim full parity while required functionality or verification is still pending.
