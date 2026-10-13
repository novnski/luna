# Pending implementation tasks from T3 remote/mobile research

This is proposed work. Task IDs below coordinate additions to the existing [Control this Mac M0–M4 plan](../control-this-mac/README.md); they do not create a second host architecture or execution engine. Current-source references live in the [backend audit](08-luna-backend.md) and [iOS audit](09-luna-mobile.md). T3 references are design evidence rather than an instruction to import its React Native stack or vendor infrastructure.

## Ownership and ordering

The [native iOS ownership refinement](../ios-parity/backlog-integration.md)
now separates RM host/shared-client contracts from full IOS presentation. The
catalog contains current dependencies. RM-03/04/05 require a minimal native/SDK
contract proof; IOS-01–04 own the complete native surfaces, and RM-07 requires
their acceptance. A thin probe never certifies the full phone product.

| Task | Phase / priority | Dependencies | Existing owner to extend |
| --- | --- | --- | --- |
| RM-00 | M0, P0 | None | Control this Mac inventory, failure specification and implementation baseline |
| RM-01 | M1, P0 | RM-00 | RemoteAuthStore, RemoteRpcService, headed engine host controller |
| RM-02 | M1, P0 | RM-01 | Desktop Connections and native phone enrollment |
| RM-03 | M2, P0 | RM-01/RM-02, CORE-05, IOS-00 | Host task/project projections and shared client runtime; full rendering IOS-01/02 |
| RM-04 | M3, P0 | RM-03 | Engine task commands, admission and durable receipts |
| RM-05 | M3, P0 | RM-04, CORE-04 | Host capabilities, policy and decision contract; full native interaction IOS-04 |
| RM-06 | M3–M4, P1 | RM-04, IOS-02 | Attachments, files, artifacts and first native file/diff surface |
| RM-07 | M4, P1 | RM-02–RM-06, IOS-01–04/09 | Overall native release, power and direct route acceptance |
| RM-08 | Expanded iOS wave, P2 | RM-07, IOS-02 | Existing terminal service and phone terminal UI |
| RM-09 | Expanded iOS wave, P2 | RM-06/RM-07, IOS-02 | Existing source control/worktree/review operations |
| RM-10 | Later, P2 | M0–M4 acceptance | Existing host browser/device/integration owners |
| RM-11 | Expanded iOS wave, P2 | RM-04/RM-05/RM-07, IOS-02 | Existing scheduler, settings, usage and maintenance APIs; read projections first |
| RM-12 | Later, P2 | M0–M4 acceptance + product decision | Owned activity/push/native extension service |
| RM-13 | Later, P2 | Reliable direct client + product decision | Optional route/account/host catalog; Android/browser clients |

P0 delivers the minimum authoritative connection and task-control boundary. P1 is still part of the already planned M0–M4 first release where stated. P2 is a recorded opportunity, not authorization to deploy infrastructure or expand V1. Pairing-only/read-only checkpoints must not be called a finished remote feature.

## RM-00 — Rebase the evidence and define failures before implementation

Confirm current LUNA/T3 revisions, user-facing scope, existing feature ownership, provider capabilities and native dependencies. Resolve any change to the existing Rust/UniFFI/native rendering direction through the architecture decision owner before editing; T3's cross-platform React Native implementation does not itself justify a rewrite.

Produce an endpoint-to-scope-to-command map for the proposed direct service, a wire/version policy, host/profile/cache identity model, and the failure scenarios in [validation](validation-plan.md). Prototype a harmless pinned-TLS descriptor/read endpoint with a physical phone before opening task mutation. Verify the proposed native transport callback can feed the chosen owned client core. Do not accept every self-signed certificate or expose local-owner IPC through a proxy.

Completion evidence: pinned baseline, reviewed contract map, physical LAN reachability and wrong-identity failures, native bridge result. Existing tests are inspected through test-audit; no new unit tests after code. This task's documentation is complete; its transport/native prototype is still pending.

## RM-01 — Host gateway, authority and lifecycle

Extend the existing per-connection RemoteRpcService and authority store inside the headed engine's runtime. Keep one owner of the selected data profile. Add the dedicated remote listener/global enable gate, bounded frames/uploads, authenticated HTTP/WebSocket, profile-owned host identity and TLS material. Local IPC retains its trusted local purpose.

Remote grants need explicit read, task-operate, attachment/file and later sensitive-operation policy. Carry the authenticated device principal into engine admission as typed server-owned context; never trust caller-supplied actor/client fields. Check both method and nested command variant, target host/profile/project/task, current grant, request revision and effective host permission at admission. Reuse current hashed pairing/session foundations. Define their on-disk migration, backup/restore behavior, expiry, rotation and corrupt-store recovery; a restored doc/backup must not revive revoked authority.

Revocation/disable must deny new admission before teardown, invalidate active transports, and specify assets/uploads/tickets issued before revocation. Decide whether artifact access is session-bound or has a documented short capability lifetime; T3's signed asset URL exception is a reason to specify this precisely.

Source ownership: existing crates/rpc remote_auth/server/client, crates/engine lib/rpc/doc_host and profile authority store; proposed dedicated gateway modules follow the existing plan's component map. Completion: host process/port/TLS/authority readback and RP-02–RP-06/RP-19 failures, with accepted local task work preserved.

## RM-02 — Desktop connection settings and phone pairing

Deliver Control this Mac enable/disable, Add QR/code, refresh, last connected, paired devices/sessions, Revoke, keep-awake preference and precise route status. Device registry presence, permission authority and notification registration are different records.

Phone supports native QR and explicit manual address/code entry through one validated operation. Bind trust to host identity/key and profile; validate the endpoint before spending a code. Persist the device session in Keychain, route metadata separately, and clear expired/invalid credentials with an explicit re-pair state. Do not auto-connect to arbitrary incoming deep links. Support denied camera/local-network permission, QR cancellation, lost redemption reply and a fresh-code recovery.

V1 is Control this Mac. Do not add outbound machine provisioning, load balancing, hosted identity login or SSH tabs. Keep any existing edge profile explicit and preserve app bundle identity, signing and data. Completion: actual phone pairing, restart, expired/replayed code, wrong host/pin and local forget versus server revoke evidence.

## RM-03 — Authoritative projections, cache and connection runtime

Expose host-owned project/thread list and detailed selected-task snapshots plus incremental updates. Separate cheap sidebar state from full transcript, and identity from connection address. Include protocol/capability version, provider readiness, task status, cursor/revision and freshness.

Build one owned thin-client connection/state model usable by native platform adapters; do not maintain divergent UI-only protocol implementations. Phone lifecycle adapters supply foreground/background, network change, cancellation and secure storage. Reconnect by authenticated revalidation and cursor resume or explicit snapshot repair. A stale socket, cached row or successful public descriptor is not proof of current authenticated readiness.

Key caches by host/profile/task and validate on load. Bound cache retention, streaming queues/tool output and snapshot work. Protect against late replies from the old session, sequence gaps/duplicates, canceled snapshot requests and mismatched host. Keep drafts through disconnect but distinguish local draft, pending delivery, accepted receipt, execution and completion.

Completion: same Mac tasks appear on phone, long transcripts stay usable, background/network/restart recovery preserves stable row ordering, and stale cached data is clearly labeled. Extend existing transcript/queue tests rather than duplicating renderer behavior checks.

The RM-03 acceptance is the shared-runtime/projection boundary through a minimal
native read surface. Full native layout/gesture/performance and shell migration
belong to IOS-01/02 and remain required by RM-07. Consume IOS-00's owned SDK/FFI
instead of a second Swift-only connection implementation.

## RM-04 — Project/task command admission and outbox reconciliation

Expose existing host operations for create/open/continue/archive/restore and run/follow-up/queue edit/remove/reorder/steer/interrupt through explicit remote authority. Never turn a remotely inserted registry/doc row into permission. The current exact local executable-grant guard must remain effective; the new host RPC creates its admission record only after authorization.

Every mutating request needs a stable command ID, typed target, canonical payload identity, actor and required revision/lease. Same ID/same payload returns its original outcome; same ID/changed payload is rejected. Record durable admission/receipt before acknowledging. Recover an uncertain send by reading its receipt/state, not by making a fresh prompt ID. Accepted work follows host policy after phone disconnect/revocation; transport closure should not stop unrelated tasks.

Project creation/worktree choices reuse existing owners. Account/host selection must not silently migrate a task. Offline delivery needs an explicit policy per operation: a prompt may be queued, but stale approval/steer/Git mutation cannot be blindly replayed.

Completion: RP-10–RP-15 and RP-20 races show one execution, correct queue semantics and explicit stale/unsupported errors for Codex, Claude and OpenCode where supported.

Add the latest T3 setup race: before host creation retain the draft; accepted
worktree preparation queues follow-ups behind setup; failed/cancelled setup
holds them instead of dispatching in the main checkout. Adapt Zeron's durable,
viewport-independent delivery/ACK lessons to LUNA receipts, not its edge nudge
protocol. IOS-03 owns the complete native composer/queue UX over this contract.

## RM-05 — Provider capabilities, effective policy, approvals and questions

Use the Mac's actual harness/account/model/options catalog. Phone static fallbacks cannot masquerade as host capability. Show requested and effective permissions where they differ, locked provider ownership, supported steer/restart/fork options and unavailable features. Provider credentials remain on the execution host.

Add approval projection and native approve/deny/action UI for the requests that still reach a person — legacy Ask chats and LUNA ceiling grants; Auto and Full never produce one ([permission modes](../permission-modes/README.md)). Current iOS question controls do not supply approval parity. Carry exact outstanding request ID/revision, command context, allowed decisions and target task. Reject stale, revoked, replayed and changed-command responses. Preserve question options/free text and continuation behavior through reconnect. Resolve the phone's legacy sandbox/autoApprove model against canonical host permissionMode before shipping.

Completion: RP-16–RP-18 exercise actual provider-facing question and approval flows (approvals from a legacy Ask chat or ceiling grant) with a controlled permitted action, refusal and stale-response rejection, plus an Auto/Full task whose phone view shows no approval card. Preserve final command/request/decision trace; a generic dialog fixture is insufficient.

RM-05 owns host capability/request/decision admission and its native/SDK probe;
IOS-04 owns the complete picker/question/approval presentation. Static fallback
catalogs never establish runnable capability, including for new host provider
plugins. Both layers remain required for RM-07.

## RM-06 — Attachments, context, files and diffs

Reuse image UX and host attachment storage; add bounded task-owned file/media upload with content hash, retry/cancel, resumable behavior only where specified, and staging/orphan cleanup. Authorization must cover task and destination, not just an arbitrary filesystem path. Keep attachment references and exact bytes through queued edits and lost replies.

Deliver workspace tree/search/read previews and checkout/task-associated diff before write/Git controls. Support generated image/artifact retrieval and explicit file-link scope. Prevent traversal, symlink escape, ignored-file policy bypass, task/profile substitution and unbounded large/binary output. Existing desktop ignored-file trust policy is not an automatic default for a separately paired phone.

Workspace context selection, slash commands/skills and rich tool bodies use host/provider capability. HTML/MCP Apps renderer or search work stays under [interactive chat surfaces](../interactive-chat-surfaces/README.md); phone integration consumes its reviewed contract.

Expose bounded typed file metadata from the owning host for native file/context
chips; a client-guessed suffix or path is not an authorization fact. IOS-03/06
consume it. RM-06 owns the first phone file/diff workbench within IOS-02's shell;
advanced native content and interactive app lifecycle belong to IOS-06.

Completion: hash-verified image/file upload, wrong-task/path rejection, exact revision diff, file/media preview and large-output behavior on a physical phone. This does not require a phone text editor: T3's inspected file surface is itself read-only.

## RM-07 — Native first release, host availability and direct outside access

Complete the existing native rendering migration while retaining stable transcript identity, scroll anchoring, safe-area/keyboard behavior, dynamic type and screen-reader controls. Measure representative long transcripts and large tool output before blaming networking or selecting a new renderer.

Coordinate keep-awake reasons with existing task assertions; observe external power, no selected task, disabled host and quit/sleep/wake. A saved credential cannot keep a quit host alive. Respect graceful quit and single GUI app rules during QA.

Nearby access needs real local-network permission and reachable HTTPS. Off-network access needs owner-configured direct ingress/DNS/firewall, TLS verification and a physical cellular test. Handle uncontrolled NAT/ISP restrictions honestly. No Tailscale/DERP/Cloudflare tunnel, WorkOS room, push relay or R2 fallback may appear implicitly.

Completion: all applicable M0–M4/RP-01–RP-26 gates, retained evidence and actual LUNA Dev handoff. Follow AGENTS builds, budget and app verification during implementation; this research does not require a rebuild.

## RM-08 — Phone terminal, separately scoped

Expose existing host terminal operations with explicit terminal grant and task/session identity. Native phone view supports snapshot+stream, bounded history, write/paste/special keys, resize, reconnect, read-only observation and clear/close. Decide session lifetime on leaving a screen, reconnect and phone revoke without killing unrelated processes. Avoid a second terminal backend. RP-27 owns this future acceptance.

## RM-09 — Git/worktrees/review and broader thread lifecycle

Reuse host branch/worktree/fork/rewind/review/commit/push/PR operations. Add guarded phone controls only when capability and checkout identity permit. Confirm destructive checkout/reset/restore/delete; preserve drafts during repository changes. Specify selected-file commit behavior, concurrent change conflicts and remote rejection.

T3 mobile's local diff/comment-to-agent surface is distinct from hosted PR review. Do not invent a PR Code/reviewer/merge requirement from T3 mobile evidence. LUNA's Board, Side chats and review owners stay authoritative; phone navigation/commands consume them. RP-28–RP-30 verify future flows and exact resulting Git state.

## RM-10 — Browser/device/integration remote sessions

Expose only explicitly granted host browser/device sessions with visible ownership, acquisition/release, bounded streaming and input. T3's browser preview and simulator viewer are not generic desktop sharing. Define privacy, host OS permissions, file picker/download transfer and permission isolation before implementation.

Interactive HTML/MCP Apps follow the existing renderer/bridge owner; remote phone integration must use its capability and asset/request contracts. Broader Mac Computer Use is its own reviewed scope. RP-31/RP-32 cover these later capabilities.

## RM-11 — Phone operations, scheduling and administration

Reuse scheduler/automation, project defaults, usage, provider management and updater owners. Add phone views progressively: read projections first; typed mutation with sensitive scopes and host capability next. Updating/restarting a running host requires current task/draft/outbox readback, an explicit safe lifecycle and reconnect/version recovery. A scheduled task still runs on the selected host; phone presence is not the scheduler.

Keep local phone preferences/cache cleanup separate from host data deletion. Credential/account changes and access administration are higher authority than ordinary task control. RP-33–RP-35 own acceptance; do not build another scheduler or update service.

## RM-12 — Awareness, voice, sharing and native extensions

This remains the optional cross-client service/registration/privacy contract.
Native share intake is IOS-10; dictation, live voice and push/widgets are
IOS-11/12/13. Their detailed native implementation is not duplicated here.

Foreground attention belongs in direct V1. APNs/FCM delivery and Live Activities/widgets are later explicit service work: choose an owned service, signing/account model, minimal activity schema, expiry, retries/dedup, per-device registration/rotation/unregister and data retention. A direct socket cannot promise reliable iOS background execution.

Do not copy T3 credentials/endpoints. Distinguish push-device registration from host sessions and data-plane pairing. Choose whether a direct-only installation may opt into an independent awareness service, then document the boundary before shipping.

Voice needs a platform/locale/provider strategy; T3 source's Apple transcriber does not prove Android voice. Sharing needs extension/App Group signing, durable inbox, dedup, target choice and owned temporary-file cleanup. Widgets need explicit stale/expired presentation. RP-36–RP-38 own these future behaviors.

## RM-13 — Optional routes and other clients

After direct M0–M4 succeeds, decide separately on multi-host catalog, ordered/learned routes, Tailscale, SSH, owned cloud tunnel/discovery, browser and Android. Preserve the same engine authority across transports; never turn account login into local-owner access. Validate route identity, TLS and credential scope anew; cheap public descriptor caching must not be described as a credential-safety guarantee.

T3 Connect-equivalent work is a hosted-service program: identity broker, environment ownership, tunnel allocations/DNS/connectors, proof-bound credentials, renewal, offline availability, operations/cost control and optional awareness. Source feasibility is not deployed capacity or pricing. Browser HTTPS/session/CSRF/origin constraints and Android lifecycle/native modules require their own acceptance. RP-39–RP-41 cover later routes/clients.

## Completion and handover

Define failures first, implement at the existing owner, run locked scoped build/check/format plus strongest-boundary E2E, retain command/exit/log/artifact/hash, independently review the result, refresh/verify LUNA Dev after meaningful Mac code, then use commit-and-push. Do not certify a whole milestone from a UI screenshot, paired socket, command row or simulated provider receipt.
