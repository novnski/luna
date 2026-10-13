# Failure contracts and verification

**Status:** planned acceptance only. No result below has been executed for this feature.

Use the [test-audit authoring gate](../../../.codex/skills/test-audit/SKILL.md). Before writing a test, name its observable behavior, credible regression, strongest owning boundary and why existing coverage cannot catch it. No unit tests after implementation. If a system needs isolation, record its full failure matrix first. Prefer real engine/harness/document/RPC E2Es and native acceptance; no test-only production accessors or seams.

## Failure matrix recorded before implementation

| Contract | Credible failure | Primary evidence owner |
| --- | --- | --- |
| Three callable HTML integrations | A provider gets prose instructions but no callable tool; resume loses registration; image/visual metadata disappears in native encoding. | Actual provider-specific adapter/engine E2E, then one live acceptance per provider. |
| Caller authority | A model-supplied task ID, focused pane, late callback or stale run publishes into another chat. | Engine publication through real tool/session authority and durable document readback. |
| Publication/idempotency | Retry duplicates a card; different bytes reuse a receipt; interruption creates an orphan or late card. | Artifact service through engine restart/cancellation/retry E2E. |
| Size/preparation | Unicode bytes exceed the source limit; decoded image expands past its cap; missing/invalid image is silently ignored. | Real preparation/publication boundary with file/byte fixtures. |
| File privacy | Image manifest follows a symlink into denied data; a text secret renamed `.png` is embedded; `file://` remains reachable. | Real authorization and actual renderer/preview request observations. |
| Native isolation | Frame reaches session cookies, host IPC, loopback/LAN, opens a popup or escapes its scoped document; JS shims give false confidence. | Real native renderer and headless preview with local request counters and filesystem sentinels. |
| Preview fidelity | Returned image is unrelated to prepared document; digest mismatch is accepted; clipping/error is called success. | Real browser screenshot, diagnostics and digest binding E2E. |
| Stored persistence | Restart loses bytes; export contains credentials; deleting one fork removes another chat's document. | Engine/restart/reference/export E2E, with native reopening. |
| Stable transcript placement | Late capture creates a duplicate tail row; resize/prepend moves the viewport; off-screen WebViews cover menus or keep animating. | Native virtualized transcript with stable-ID readback and captures. |
| MCP provenance | Tool output points at another server's app; an app calls a hidden/different server tool. | Actual origin-bound app bridge and fixture-server operation log. |
| Live-session fence | Stop/restart reuses old handles; a stale view starts another provider/admin process or mutates the rebound session. | Real provider-session generation/cancellation E2E plus live Stop acceptance. |
| Mode settlement | An Auto decline or escalation still calls the server; a Full or Auto call parks a LUNA approval card; repeated requests duplicate mutations/messages; read-only metadata bypasses LUNA access. | Real bridge/admission/queue with authoritative server and chat readbacks. |
| Context isolation | App context masquerades as permission; updates start turns; removed/forked app context leaks into another chat. | Actual provider prompt/context capture and task lifecycle E2E. |
| Complete search | Only mounted/loaded rows are searched; activity metadata stands in for body text; inaccessible history says zero. | Actual normalized-history/search/reveal integration with native off-screen navigation. |
| Text/range correctness | Phrase split by emphasis is missed; folded Unicode produces wrong byte offsets; URLs or secrets are searchable. | Display-text/range projection through real Markdown rendering and authorized message data. |
| Streaming/rollback search | New text renumbers active result; deleted match jumps to another chat; old query result wins a race. | Actual streaming document deltas, search generations and native reveal. |
| Focus/accessibility | Cmd+F steals file Find; Escape traps focus inside WebView; Find changes composer draft or manual folds. | Native actions, keyboard navigation and state readback. |
| Resource/performance | Every historical card starts a browser; hidden views consume CPU; huge search blocks GPUI; cleanup removes active artifacts. | Native measurement and actual ownership/readback under representative history. |

## E2E scenarios and unique ownership

The proposed suite names are organizational hints, not existing commands. Choose the actual existing strongest boundary during IC-0. Consolidate shared setup; do not replay one invariant at every layer. Scripted peers must exercise real adapter, engine, journal and document paths with distinct provider wire shapes, never supply the very receipt/result being asserted.

| ID | Scenario and pass condition | Retained evidence |
| --- | --- | --- |
| H01 | For each of Codex/Claude/OpenCode, a normal/resumed chat discovers and calls both tools; a screenshot and typed publication arrive through that provider's real encoding. | Version/schema, sanitized native wire, full tool result, document reference and outcome. |
| H02 | Agent previews an interactive SVG/canvas chart whose script reports a deterministic check; published digest matches prepared bytes. Theme/width variants produce measured screenshots. | HTML/resource hashes, PNGs, viewport facts, console output and receipt. |
| H03 | Publish the same request twice; get one artifact/row. Reuse its key with changed input; get conflict. | Receipts, reference count and final document. |
| H04 | Stop during preparation/capture/commit, then restart engine. No late publication or orphan; uncertain success recovers the same reference. | Phase events, before/after store inventory and recovery result. |
| H05 | Embed authorized images; move original files; reopen/export still works. Missing/denied/symlinked/non-image/oversize inputs fail with the intended category. | Fixture hashes, authorization facts, exported bytes and errors. |
| H06 | Deny file access, session storage, host bridge, popup/navigation and private-network/WebRTC paths in visible and preview engines. Permitted prepared resources still load. | Sentinels/request counters, policy facts, screenshots and actual blocked observations. |
| H07 | Publish with preview runtime unavailable; valid card works but result says unverified. A failed/oversize preview returns bounded diagnostics and no fake screenshot. | Availability/error result and published fallback state. |
| H08 | Restart, archive/unarchive, rollback and delete with shared fork references; correct bytes survive until the final owner is removed. | Reference/journal snapshots and store readback. |
| H09 | Multiple visuals in long history; stream, prepend, resize, split, expand, collapse and open menus. Stable rows/anchors and no orphan native views. | Before/after IDs, viewport offsets, video/PNGs and live-view count. |
| H10 | Authorized second device fetches artifact; denied/unavailable host has a truthful state. No credential or private result is added to synced metadata. | Transport outcome, metadata snapshot and native state. |
| A01 | Real Codex plus fixture MCP server: declared app capture renders, initializes and receives original tool input/result. | Provider/server versions, resource digest, handshake log and screenshot. |
| A02 | Read-only app call returns actual server data; a Full mutation changes the server once with no prompt; an Auto mutation the provider's reviewer declines or escalates changes nothing and shows its reason; hidden/cross-server operations are refused. | Server operation log and before/after data, per-mode decisions and results. |
| A03 | Stop/rebind provider, navigate the page, remount view or use an old token. Stale calls fail; no extra provider or MCP connection is launched. Stored snapshot stays readable. | Session/view generations, process/connection facts and rejection results. |
| A04 | App proposes a message; decline adds nothing; confirm queues one attributed user message even after duplicate request. | Queue receipt and final transcript/message count. |
| A05 | Latest app context arrives on the next user turn as untrusted input, never starts a turn itself, respects caps and disappears after rollback. A fork gets no source context/bridge. | Sanitized actual outbound input and task/turn readbacks. |
| A06 | Expand/teardown/link/file-save operations respect negotiated capabilities and explicit user actions; exported bytes contain no bridge/session tokens. | Handshake capabilities, lifecycle result and saved-file hash. |
| A07 | Claude/OpenCode capability probes distinguish inspection from active interaction. Supported operations pass their own wire/live acceptance; unsupported paths show normal output and a specific reason. | Per-provider operation evidence and fallback captures. |
| S01 | Find matches user/assistant/code/ordinary plan text in the selected conversation, including oldest off-screen entry; other chats are excluded. | Canonical fixture transcript, expected independent matches and native revealed IDs. |
| S02 | Match across emphasis/link-label runs, Unicode case folding, emoji and multiline code; mapped highlights are correct. Raw destinations, HTML source, private results and secrets are absent. | Projected-range/result artifact and native highlights. |
| S03 | Stream/edit/prepend/rollback while searching and change query quickly. Active anchor survives or advances predictably; obsolete generations never update the bar. | Delta sequence, generation/match readback and captures. |
| S04 | Reveal a folded/off-screen match, wrap next/previous, close Find and manually alter folds/viewport. Only search-owned changes are restored. | Keyboard/action log, fold revisions and viewport state. |
| S05 | Cmd+F routes by active surface; palette/menu use selected chat; Escape/focus and composer draft survive app expansion and Find. | Native focus/action/draft readback and screenshots. |
| S06 | Incomplete/unavailable history shows Searching/Partial history rather than a complete zero; later loaded history contributes correct matches. | Actual history cursors/coverage state and final match result. |
| P01 | Representative 10,000-entry/10 MiB history plus multiple artifacts stays within measured search/UI/view budgets; off-screen pages cease active work. | Hardware, timings, memory/CPU samples, live-view count and source fixture digest. |

H06 includes genuine negative observations at the browser/network boundary; a source grep, disabled JS property or absent console log is not isolation evidence. H10 can use the existing real remote-RPC integration boundary for authorization; label it separately from physical two-device native evidence if only a controlled peer was used.

## Live acceptance campaign

Use one disposable project with non-sensitive data and one standards-based fixture MCP server whose requests and state can be inspected. Do not connect fixture mutations to production services. Where Executor owns shared integrations, preserve that registration/credential ownership; the feature's provider-owned app bridge must not create duplicate connections. Use the repository's existing QA isolation and normal provider authentication, not copied credential files.

1. **Codex HTML:** ask for an interactive chart with a slider and local image. The model calls both tools. Verify preview diagnostics, rendered interaction, source/export, restart and search-to-title/message.
2. **Claude HTML:** repeat through the actual Claude CLI bridge, including resumed chat and Stop during one preview. Retain independent wire/capture evidence; Codex proof is not Claude proof.
3. **OpenCode HTML:** repeat through the real HTTP/SSE provider session, verifying structured result/image delivery and continued chat operation after publication.
4. **Codex MCP App:** call the fixture tool; inspect its visible app, perform read-only and mode-settled mutations (Full once; Auto by the provider's reviewer), send one confirmed message, update context, stop/resume and inspect stale-bridge behavior.
5. **Native integrated Find:** navigate visible and off-screen matches in the same long conversation with streamed text and visual cards. Verify theme, scroll, focus, source/export and expanded app behavior together.
6. **Capability/platform limits:** record exactly which Claude/OpenCode app operations and OS renderer policies are verified. A fallback can pass its own honesty contract; it is not certification of the unavailable feature.

## Repeatable artifacts and commands

Each campaign writes to a scoped directory such as `target/qa/interactive-chat-surfaces/<run-id>/`. It contains `manifest.json`, exact command/exit-code logs, repository SHA, CLI/spec/runtime versions, scenario outcomes, fixture/source digests, sanitized protocol/engine readbacks, screenshots/video where relevant, performance facts and a SHA-256 manifest. Preserve the failure run as well as the passing repair. Never store secrets, raw personal MCP configuration, real customer data or signed access URLs in shareable evidence.

The manifest distinguishes planned, skipped, unsupported, offline-passed and live-passed scenarios. Include why a scenario was skipped and which required acceptance remains open. An automated screenshot fixture is not real-provider certification; a model acknowledgement is not proof of an interactive action. Verify server/file/document state after the action.

Implementation uses `scripts/cargo.sh check --locked -p <affected-package>`, focused owned E2Es, formatting and configured Clippy/CI equivalents. Replace placeholders with the actual suite names chosen in IC-0 and retain each command's original exit status in its log. Do not execute broad workspace checks without need. Before building, inspect the build budget and coordinate busy/budget-blocked runs.

For native QA, follow the four identity overrides and one-app rule in [QA infrastructure](../../reference/qa-infrastructure.md). Verify exact bundle process, expected IPC listener and `codesign --verify --deep --strict` for the selected QA bundle. Preserve/hash evidence before scoped QA cleanup. Reopen the primary LUNA Dev only after the QA app stops gracefully. Required developer refresh and build duration are recorded separately from fixture evidence.

## Release checklist

- All-provider HTML tools and actual native interaction passed; failures/cancellation/resume are covered.
- Codex MCP app has a verified live server round trip; Claude/OpenCode capabilities/fallbacks are accurately recorded.
- Artifact ownership, privacy, isolation, restart, export, fork/rollback/delete and remote-access boundaries passed.
- Find covers complete authorized history with correct Markdown/Unicode highlights and stable streaming navigation.
- Native virtualization, focus, accessibility and representative performance were measured.
- Required build, focused checks and format/lint passed; LUNA Dev refresh/process/port/signature verified.
- Evidence is repeatable and hashed; pending-task status reflects remaining work; reviewed source is committed/pushed and `origin/main` is read back.

This checklist is for the future implementation. A documentation-only addition of this package should validate links, source references and whitespace; it does not require a Rust rebuild or app refresh and must not be reported as feature verification.
