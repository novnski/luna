# Full Claude Code integration: implementation plan

Status: planned. No package below has been implemented or validated by this research. Read [the source baseline](research-baseline.json) and [validation plan](validation-plan.md) before execution.

## Product outcome

Users can choose Claude and use the same ordinary LUNA chat, attachments, tools, approvals, child chats, history and ongoing-work surfaces without losing native Claude capabilities or misrepresenting provider state. Native tools execute inside Claude. App tools execute through the engine's authenticated authority boundary. Hosted services remain separately identified and verified.

Do not revive Plan/Build controls or a replacement `/plan`. Do not resume old orchestration or create worktrees. Another agent is delivering shared LUNA changes; update this plan's baseline and reuse that work before implementation.

## Proposed boundaries

1. **Claude transport:** exact executable/account/cwd, native session, stream and administrative operations. A versioned capability interface sits behind the existing harness abstraction. Compare the direct Rust driver with an official SDK sidecar before choosing; do not replace the driver based solely on the other apps' language choice.
2. **Provider normalization:** canonical native IDs, ordered content, result/error and lifecycle semantics. Keep provider-private details inside the adapter and a host-owned evidence envelope. No tool execution belongs in the renderer.
3. **Engine authority and durability:** reuse task identity, queue, app-tool authorization, host output store, pending-input ownership, journal and recovery. Provider acknowledgments carry evidence levels rather than optimistic booleans.
4. **Shared product UI/RPC:** consume common LUNA capabilities and typed results. A Claude-specific capability gate selects the correct native operation; no Codex request names leak into Claude.
5. **Hosted/native integrations:** separate explicit adapters for remote/cloud/browser/computer-use services with account/platform constraints and their own evidence.

Suggested conceptual contracts, to fit into existing types rather than duplicate them:

| Contract | Minimum information |
| --- | --- |
| Capability snapshot | Provider/CLI/transport version, account/cwd/binding, source of evidence, native capability bits, configured/effective state, timestamp and limitations |
| Native event envelope | Event/schema version, provider session/message/turn/block/tool/task/parent IDs, origin, native payload reference and normalization outcome |
| Content reference | Owner/generation, ordered content kind/MIME, length/hash/storage reference, preview policy and missing/error state |
| Input delivery receipt | Engine command ID, native message UUID where available, local written/accepted/recorded/incorporated/unknown evidence and terminal reason |
| Pending interaction | Native request/tool IDs, owner/generation, typed context/schema, allowed decisions, cancellation and terminal response |
| Native task binding | Provider agent/task/workflow identity, lifecycle/generation, app lineage if independently created, output and usage |

Keep synchronized previews bounded and full permitted output in the host store. Keep secrets, authorization headers and provider-private reasoning out of synced documents and public artifacts.

## Sequence and dependencies

| Package | Priority | Dependencies | Scope / principal ownership |
| --- | --- | --- | --- |
| CC-00 Baseline and executable conformance | P0 | none | Harness discovery and compatibility artifacts |
| CC-01 Transport decision and control foundation | P0 | 00 | Claude driver/process/control API |
| CC-02 Lossless events, content and storage | P0 | 01 | Claude wire/normalizer + shared output contract |
| CC-03 Permissions, questions and cancellation | P0 | 01/02 | Claude controls + engine input/permission bridge |
| CC-04 All-turn image/attachment input | P0 | 01/02 | Harness input + composer/queue attachment contract |
| CC-05 Complete tool/results and structured output | P0 | 02/03 | Tool classification, host results, RPC/UI details |
| CC-06 Effective models/settings/usage and delivery | P1 | 01/02/03 | Discovery cache, runtime mutation and queue evidence |
| CC-07 Callable LUNA/Executor bridge | P0 | 01/02/03 | Engine runtime/app tools + Claude MCP delivery |
| CC-08 Native agents and background task graph | P1 | 02/05 | Agent/task normalization and shared transcript/task UI |
| CC-09 Persistent children and native peer messages | P1 | 07/08 | Task identity/lineage/messaging authority |
| CC-10 Native history and checkpoints | P1 | 01/02/06 | Provider history + engine binding/history UI |
| CC-11 MCP inventory/resources/apps | P1 | 05/07 | Effective integration inventory, native admin, resource UI |
| CC-12 Skills/plugins/hooks/memory/styles | P1 | 03/06/11 | Native configuration/dispatch and shared settings UI |
| CC-13 Workflows, teams and channels | P1 | 08/09/12 | Native coordination/task graphs and origin policy |
| CC-14 Native goals and scheduling | P1 | 06/08/12 | Native ongoing work + shared status/control UI |
| CC-15 Review and structured findings | P1 | 05/10 | Shared review/files/provenance and native review lane |
| CC-16 Cloud/remote/browser/computer integrations | P2 | 03/09/11/15 | Gated service/native adapters and handoffs |
| CC-17 Operational compatibility and account variants | P1 | 00–16 as applicable | Recovery, limits, auth/provider/OS release matrix |
| CC-18 Full certification and delivery | required | all claimed capabilities | Serial immutable review, native QA and verified delivery |

Packages describe ownership boundaries, not authorization to spawn parallel agents. Shared proto/engine files must have one owner during a concrete implementation cycle. Parallel work is safe only after contracts are frozen and the user authorizes delegation.

## CC-00 — establish a reproducible baseline

Refresh LUNA HEAD and compare this audit to the other agent's merged features. Capture the selected Claude executable path/version/hash and sanitized help, CLI/SDK version compatibility, provider account identifier, cwd and effective configuration provenance. Run harmless initialization/discovery against that exact binary when implementing, without starting a model turn merely to list capabilities.

Inspect startup flags, permission alias behavior, native command availability, task/result schema, UUID echoes, interrupt receipts and native history support. Document conflicts such as TaskOutput's removal versus the older tools-table entry. Reject unavailable operations accurately; static fallback rows must not imply live account support.

**Deliverable:** machine-readable capability ledger and sanitized native initialization capture. **Gate:** E01/E02 artifacts identify the same binary/account/cwd used for real sessions. Relevant files: `crates/harness/src/claude/{mod,catalog,wire}.rs`, engine model/command discovery and account metadata.

## CC-01 — choose and implement a versioned transport

Compare two bounded prototypes using the same native CLI and fixture workload: keep Rust's streaming JSONL/control driver, or use a small official TypeScript SDK bridge supervised by Rust. Measure supported operations, startup/idle overhead, process cleanup, reconnect/cancellation, native settings inheritance, account scoping, package size and update compatibility. The sidecar must use the existing selected executable and may not launch an independent agent loop with a replaced system prompt.

Prefer the direct driver if its conformance evidence covers the required public/native operations. Choose a sidecar if maintaining undocumented control details leaves real parity gaps. If selected, define a narrow framed JSON interface, lock dependency/runtime versions, verify packaging on supported architectures and keep supervisor ownership in LUNA. Do not use the removed SDK V2 preview API.

**Deliverable:** decision record plus versioned session/control interface. **Gate:** E01/E03 demonstrate launch, warm input, native resume, deterministic shutdown and explicit unsupported controls. Relevant files: Claude harness/process modules; any sidecar directory is a new, separately reviewed architecture decision.

## CC-02 — preserve native events and complete content

Extend wire decoding beyond the current narrow enum. Decode full nested content, native UUID/origin/parent fields, envelope result metadata and all message families in the matrix. Unknown frames must be counted and safely retained rather than silently mapped to nothing. Invalid semantic frames produce a visible compatibility error with recoverable evidence; non-semantic progress may use a generic view.

Reconcile partial/full blocks with stable native item keys. Feed ordered content and complete results into the existing host journal/output store before document projection. Bound synchronized previews, expose content references and distinguish empty from unavailable data. Never replay a tool based on a missing UI output.

**Deliverable:** end-to-end content/event contract and coverage ledger. **Gate:** E04/E05 preserve hashes through real subprocess → engine → lazy RPC → UI → restart. Relevant files: `crates/harness/src/claude/{wire,normalize}.rs`, `crates/proto`, `crates/engine/src/{sessions,run_journal}.rs`, document/transcript projection.

## CC-03 — interactive controls and effective permissions

Build one correlated dispatcher for approval, question, form/URL elicitation, supported native dialogs and cancellation. Retain command/path/decision reason, native tool ID and scoped permission suggestions. Use the engine as sole pending-input owner, persist recovery state and guarantee one terminal response. Timeout/cancellation must not become an implicit permission grant.

Validate the actual Ask/Auto/Full mappings and sandbox enforcement against native behavior and managed policy. Native Full remains explicit bypass; availability-gated Auto is not equivalent to Full. Show any unsupported or downgraded protection. Keep unknown controls from hanging the child. Enforce the cancelled Plan/Build decision without automatically allowing plan-exit permissions.

**Deliverable:** complete interactive contract and policy readback. **Gate:** E06/E07 cover concurrent requests, abort/late answer, managed deny and sandbox-unavailable behavior. Relevant files: Claude controls, `crates/engine/src/permissions.rs`, RPC/input bridge, shared proto and question/approval UI.

## CC-04 — use one typed input path for every turn

Unify initial, warm, queued, steer and resumed inputs. Include actual image bytes and MIME in every supported route, preserving content order and attachment ownership. Retain user message correlation IDs, native UUID evidence and an attachment manifest. Validate image-only input, corrupt/unsupported/oversized files and missing paths before dispatch; show an actionable error rather than silently skipping bytes.

Use the shared composer/attachment implementation being delivered by the other agent. Keep local files/path references distinct from images, and confirm any document input against the actual Claude contract. Image retries cannot mutate or duplicate a previously submitted user message silently.

**Deliverable:** typed Claude input serializer across all routes. **Gate:** E08 proves first/warm/active/resumed vision with the same fixture hash and native readback. Relevant files: Claude wire/session input, shared harness request types, engine durable queue and composer attachment bindings.

## CC-05 — complete tool fidelity and structured results

Implement the built-in/tool-family matrix, including notebook/LSP/tasks/workflows/resources/findings and generic future tools. Preserve native arguments without reducing typed tools to only one or two fields. Extract canonical result content and native metadata once, retaining errors, output files, citations and rich assets. Derive diffs only from supplied native evidence or a labelled host file checkpoint; never fabricate before/after content.

Connect lazy output/diff/asset RPCs and shared transcript details to host-owned references. Keep schema-validated output distinct from ordinary assistant prose and surface native validation failure. MCP structured content and image/resource blocks must survive just like text.

**Deliverable:** complete results in shared tool details, plus machine-readable coverage rows. **Gate:** E04/E05/E09 cover success/error/empty/mixed/large/root/child results and structured output. Relevant files: Claude normalizer, common `ToolCall`/result types, host output RPC, transcript tool and asset views.

## CC-06 — discovery, live controls, usage and truthful delivery

Keep current live model/command discovery, fix stale comments and cache keys, then add effective account/cwd/native capability readback. Validate effort/thinking/fast/context combinations instead of assuming all models share a ladder. Mutable settings/model/permission changes need native acknowledgment and readback; persistent writes are separate from session-only changes.

Retain cache tokens, cost/model/context data, rate-limit warnings/rejections/reset and API retry state when supplied. Include background spend without double counting. Unknown cost/context values remain unknown.

For queue/steer, distinguish stdin written, native accepted/recorded and model incorporation evidence. Use native UUID/receipt capability where available. Preserve the current unconfirmed state on older runtimes; no text-equality matching or automatic ambiguous replay. Correlate interruption and result boundaries with pending inputs.

**Deliverable:** effective capability/usage snapshots and delivery evidence. **Gate:** E02/E10/E11 exercise live changes, duplicate text, interruption/reconnect and honest fallback. Relevant files: Claude discovery/catalog/session, engine model cache, queue/registry, usage and settings UI.

## CC-07 — attach callable LUNA tools and reuse Executor

Create a per-session, authenticated bridge for the engine's existing status/cross-chat/app-tool catalog. Reuse engine authorization checks and tool schemas; do not merely rename `ClaudeInstructionsOnly`. Bind calls to task/project/account/session/binding generation, scope expiry and human-authorized actions. Verify catalog discovery plus one real harmless call, then verify revocation on session close and rejection of stale/cross-project calls.

Use Executor for external integrations and preserve existing connections/policies. An ephemeral engine-owned app-tool bridge is scoped to a running session, not a duplicate global upstream MCP registration. Document its transport and ownership explicitly. Native tools remain native; only LUNA-defined tools cross this bridge.

**Deliverable:** verified Claude runtime tool delivery and continuity on resume. **Gate:** E12 covers callable discovery, mutation scope, errors, cancellation, stale identity and multi-session isolation. Relevant files: `runtime_contract.rs`, `cross_task_tools.rs`, task/session authority, RPC and Claude MCP configuration.

## CC-08 — native child/task lifecycle

Keep working parent-tool routing and expand it into a native task graph: root session, spawning tool, agent ID, task ID/type/generation, workflow parent, model, usage, status and output reference. Support nested agents, background Bash, progress/update/notification ordering and late messages after root completion. A root result must not erase still-running background tasks.

Connect inspect/resume/message/stop only where the native operation is supported, with exact task ownership. Revalidate cwd/account authority after native workspace transitions; do not automatically create worktrees.

**Deliverable:** dependable native task/subagent UI and recovery. **Gate:** E13 covers nested/detached/error/stopped tasks, reused IDs, late notifications and restart. Relevant files: Claude normalizer, engine subagent sinks/task state and transcript/task UI.

## CC-09 — persistent LUNA children and native peer origin

Extend the shared app child-chat feature to Claude through CC-07. A persistent child gets a fresh provider session and independent binding/lineage; it is not an imported native subagent. Preserve authorized branch/cwd choices and distinguish creation from background native tasks.

Native SendMessage/ListAgents and cross-session arrivals retain sender origin/authentication evidence separately. Sender-authored labels are not trusted identity or authorization. Prevent recursive creation/messaging outside the user's scope, duplicate children after retries and stale parent recovery. Do not message arbitrary external chats merely because a native tool asks.

**Deliverable:** provider-independent child lineage plus correctly attributed native peers. **Gate:** E14 proves restart, idempotent creation, independent resume, destination permission and peer-message trust boundaries. Relevant files: task identity/lineage/cross-task tools, native event origin and sidebar/chat views.

## CC-10 — native history and file checkpoints

Implement scoped native session list/read/info/name/fork operations through the chosen transport. Map LUNA entries to real Claude message UUIDs and validate fork/truncating resume boundaries, including queued messages/task notifications. Use a new native session for a fork; never mutate the source binding by accident.

Add file checkpoint enablement, preview and rewind where supported. Separate conversation rewind from native file rewind and local Git restore. Persist intent and snapshot before mutation, update bindings atomically and surface uncertain native outcomes for reconciliation. Do not market every shell-created file as SDK-checkpoint-restorable.

**Deliverable:** safe provider-aware history controls. **Gate:** E15 validates source/destination IDs, forked prefix, unchanged source, dry-run file scope and crash recovery. Relevant files: Claude native maintenance interface, `history_operations.rs`, `task_identity.rs`, shared history proto/RPC/UI.

## CC-11 — effective MCP inventory, resources and widgets

Connect configured source/scope, effective session attachment, health/auth and tool catalog to the shared inventory feature. Keep observed init names separate from verified ownership. Expose native reconnect/toggle/replace only when supported, with result readback and no credential duplication.

Preserve text/image/structured/resource result content. Implement gated MCP Apps metadata and `ui://` resource read only after minimum SDK/runtime capability checks. Render widgets in an isolated host, restrict resource origins and bind widget tool actions to the owning session's authority. Unknown rich output still needs a useful safe fallback.

**Deliverable:** trustworthy inventory and rich MCP results. **Gate:** E16 verifies Executor reuse, disconnected/auth-required state, reload/name collisions, resources and widget isolation. Relevant files: shared integration settings/inventory, Claude controls, host blob store and resource/widget renderer.

## CC-12 — native skills, plugins, memory, hooks and styles

Discover effective Claude roots/scopes/precedence for memory/rules, skills/commands, plugins, agents and output styles. Reuse existing shared settings components but preserve provider-specific source and namespace. Use native command expansion for skill mentions, including mixed image/prose input. Invalidate discovery on cwd/account/plugin changes and native commands-changed events.

Respect user/project/local/managed settings and native hook execution. Show useful hook input/progress/denial/error with identity and privacy bounds. Plugin management must read back actual installed/enabled/effective state; reloads should not duplicate MCP connections. Distinguish session settings from persistent file writes.

**Deliverable:** effective native extensions/configuration connection. **Gate:** E17 covers collisions/scopes, skill invocation, live reload, managed deny, hook interruption and LSP failures. Relevant files: native discovery/dispatch/configuration, engine integration settings and shared picker/settings UI.

## CC-13 — workflows, teams and channels

Map native workflow phases/members/results to the task graph; preserve repeated-progress dedupe, approval, pause/resume and usage-limit transitions. Keep team support explicitly experimental and gated; preserve teammate identities, shared tasks and shutdown state without fabricating resume semantics the native runtime does not provide.

Channels and native peer events keep their non-human origin and policy. Do not upgrade a webhook or another agent's request into human permission. Observe native workspace/cwd changes and reject stale authority. Disabled capabilities should still decode imported transcripts safely.

**Deliverable:** advanced native coordination with accurate limitations. **Gate:** E18 covers interleaving/member retries, team opt-in, channels, cancellation and late completion. Relevant files: native task/event adapter, shared task graph and request-origin/authority handling.

## CC-14 — native goals, cron, monitors and wakeups

Verify native goal command/control/status availability before connecting the shared goal UI. Preserve the condition, evaluator verdict/reason, native follow-up turns, paused/error/cleared state and resume semantics. Do not claim a Codex goal budget has a native Claude equivalent; unsupported budget controls need a stated host policy or unavailable state.

Separate session cron/Monitor/wakeups from app automation and hosted routines. Preserve native IDs/timing/lifetime, background output and disable/stop readback. Avoid duplicate schedules on resume. No implementation may recreate the cancelled Plan/Build workflow.

**Deliverable:** ordinary-chat-compatible ongoing work with truthful status. **Gate:** E19 covers bounded goal completion, rejection/pause/clear, background work, cron resume/disable and monitor deadline. Relevant files: native command/control/task adapter, shared goal/automation surfaces and engine queue state.

## CC-15 — review and findings

Connect native structured findings to LUNA's shared review surface with immutable source revision, file/line/severity/reason and result ownership. Keep ordinary chat review available without a mode toggle. Native cloud ultrareview has separate invocation/job/cost/result semantics; do not send a fabricated `review/start` to Claude.

**Deliverable:** provider-aware review/finding flow and explicit native-service availability. **Gate:** E20 preserves findings for a pinned branch, replay/reopen, empty/failed result and stale revision. Relevant files: tool/result normalization, common review/files/provenance and native review adapter.

## CC-16 — connect hosted and device integrations

Create a sub-ledger for Chrome, computer use, Remote Control, cloud sessions/teleport, Projects, artifacts, cloud routines and ultrareview. For each: verify an embeddable supported interface, account/org/OS gates, invocation/attach lifecycle, permissions, external effects, native IDs, result/assets and stop/revoke/readback. Start with inventory and safe native handoff where no embedded interface is documented; track the missing interface explicitly rather than asserting full integration.

Do not scrape private credential stores or reverse-engineer a proprietary service to make the matrix green. Publishing/sharing/scheduling affects external state and must remain within concrete user authorization. Browser/computer-use QA uses disposable pages/fixtures and one GUI app.

**Deliverable:** independently certified service integrations or exact externally blocked rows. **Gate:** E21 verifies each claimed live surface; unavailable access is a recorded blocker, not a pass. Relevant files: service adapters and native identity/capability/resource handling, with an architecture review before adding new runtimes/backends.

## CC-17 — operational behavior and compatibility

Validate supported CLI versions and host architectures, native/cloud provider configurations, account/cwd isolation, managed settings, auth expiration, proxy/network failures, rate limits, idle child exit and restart recovery. Maintain fixture/schema coverage on each compatibility bump. Bound retained artifacts and support a diagnosable generic fallback for future events.

Show actionable native sign-in/health/update paths without opening secrets. Keep cancellation and process ownership deterministic, including shell children and late native notifications. Define telemetry/diagnostic behavior and privacy explicitly; no unsolicited network export of user content.

**Deliverable:** release compatibility ledger, operations notes and recovery evidence. **Gate:** E22 plus the applicable E01–E21 suites pass for each claimed supported configuration.

## CC-18 — certify and deliver

Review one immutable commit at a time. Resolve merge conflicts against actual shared contracts, run required focused compile/format checks and production-path E2E/native QA, preserve and hash artifacts, refresh the primary app, verify exact process/IPC/signature, and deliver verified main through the repository workflow when implementation delivery is authorized.

Scope cleanup to task-created disposable fixtures/bundles/checkouts after confirming no worker/process needs them. Preserve primary target cache, current app/userdata, source/Git state and other agent outputs. A cancelled feature is not delivered; an account/API blocker is not tested support. Final certification lists all supported/gated/excluded/unverified rows and evidence paths.

## Definition of full parity

Every capability row has an accurate native availability state and a verified LUNA outcome, or a clearly identified external-interface/account blocker or explicit user product exclusion. Every advertised native tool/control has a completion/unsupported/cancel path. Supported images/results survive all turn types and restart. Child/task/history identities stay consistent. The same claimed user flows pass for Claude and Codex through shared LUNA surfaces, using their respective native contracts.

A release cannot claim unqualified full Claude parity while required rows remain unimplemented or unverified. Core parity, advanced native parity and hosted integration coverage should be reported separately until the entire scope is certified.
