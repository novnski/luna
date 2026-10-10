# Remote/mobile failures and E2E acceptance

**Status:** future verification specification. None of the RP journeys below was executed during this research. Existing tests cited in the reports were inspected as source evidence, not rerun. The [existing M0–M4 validation plan](../control-this-mac/validation-plan.md) remains the owner of E01–E14; the RP IDs below expand distinct risks and should map to that owner rather than create duplicate suites.

## Authoring and execution rules

Follow [test-audit](../../../.codex/skills/test-audit/SKILL.md). Write failure modes before implementation. Prefer a single end-to-end/integration owner at the real host/phone boundary; extend an existing owner for a repeated invariant. Add no test-only production seam and no unit test after production code.

Use a local-only LUNA profile, one correctly isolated Mac app and a physical iPhone for the direct trust/network journey. Simulator or demo rendering is useful evidence of UI behavior only. Use scripted peers at the actual provider boundary for deterministic failures, followed by bounded real Codex/Claude/OpenCode turns where the capability is supported. Do not attribute provider/network failure to the UI before inspecting representative actual responses.

Never force-quit a user's app, mutate a live unrelated profile, operate an unowned router, or use a production account to fabricate a fixture. Gracefully close the exact current app before native QA, use all four QA identity overrides, and preserve evidence/data/cache through reviewed cleanup.

## Required artifact contract

Every executed journey produces a manifest with: scenario ID; timestamp/timezone; source SHAs; host/client binary versions/channel/protocol; command and exit status; isolated host/profile/task IDs; network route and relevant OS version; expected/actual result; correlation IDs/receipt/cursor; log/capture paths and SHA-256; retries/flakiness and limitations. Redact pairing/session/private-key bytes and sensitive workspace content.

State evidence levels explicitly: source inspected, deterministic real-boundary E2E, installed-client observation, physical LAN, physical cellular, deployed push. A command receipt certifies admission, not completed provider work. Notifications and route choice require their own received/destination evidence.

## First-release failures and observable acceptance

| ID | Owner / credible failure | Journey and required readback |
| --- | --- | --- |
| RP-01 | RM-00; incompatible native bridge or misleading reachability | Physical phone reaches harmless pinned descriptor; callback feeds chosen core; record identity/TLS and exact route before enabling commands. |
| RP-02 | RM-01; local-owner privilege exposed remotely | Enumerate allowed API/method+nested variants. Remote attempts local-only secrets/backup/root/lifecycle operations and gets rejection with no admission record or side effect. |
| RP-03 | RM-01/RM-02; expired/reused/revoked bootstrap | Pair successfully once; retry same QR, expire/cancel fresh code, use wrong pin/host/profile and malformed URL. All fail precisely; correct recovery uses a fresh authorized grant. |
| RP-04 | RM-02; redemption response lost or storage fails | Drop response after code consumed; fail Keychain write. Phone cannot claim paired; recover without leaked or duplicated authority; inventory explains any minted unused session. |
| RP-05 | RM-01; disable/revoke races admission | Two phones connected. Disable host, then revoke only one. New unauthorized admissions stop before socket teardown; correct peer/history survives; cached revoked data is marked according to policy. |
| RP-06 | RM-01; issued capabilities outlive revocation | Mint ticket, upload grant and artifact URL before revoke. Reuse each afterward; outcome matches explicit policy and expiry. Do not claim universal immediate asset denial without evidence. |
| RP-07 | RM-02/RM-03; forgotten host still trusted elsewhere | Phone Remove clears its scoped credentials/cache; separate server revoke readback invalidates authority. Do not present local cleanup as remote revocation. |
| RP-08 | RM-03; stale socket after suspension/network change | Background for short and long intervals, switch Wi-Fi/cellular, resume. Revalidate/authenticate, reconnect without retry storm, and show live only after authoritative readiness. |
| RP-09 | RM-03; stale/corrupt cache or wrong identity | Restart offline, corrupt cache, swap endpoint to another harmless host, change profile and delete task. Correct stale/offline/repair/deleted states; no cross-host transcript or credential use. |
| RP-10 | RM-04; lost acknowledgement duplicates a prompt | Drop accepted command response, reconnect/resend same ID, restart host during uncertain delivery. One admission and one intended execution; original receipt/outcome recovered. Changed payload with same ID is rejected. |
| RP-11 | RM-04; forged synced rows gain authority | Insert untrusted command/queue rows and false issuedBy. They cannot create exact host admission. Legitimate paired RPC creates its own authorized receipt and executes. |
| RP-12 | RM-04; read grant or nested variant bypasses checks | Read-only phone sends task mutate; permitted method carries disallowed variant or wrong project/task. Deny at host, with no grant record/side effect. UI gating alone is not the proof. |
| RP-13 | RM-04; queue edits race delivery | Phone/desktop concurrently edit/reorder/remove queue with attachments. Revision/lease and blocked-head policy preserve bytes/order and show conflict explicitly. |
| RP-14 | RM-04/RM-05; stale steer/restart/stop | Active turn changes or completes while phone submits. Capability and turn identity select correct accept/reject behavior; unrelated work remains running. |
| RP-15 | RM-04; accepted work dies on phone loss | Submit, disconnect or revoke phone after admission. Host follows explicit execution policy, durable outcomes remain, other tasks unaffected. |
| RP-16 | RM-05; approval response authorizes replacement action | Actual provider requests approval. Change/revoke/resolve request then replay phone response. Exact stale ID/revision rejected; permitted approve and deny each produce host readback. |
| RP-17 | RM-05; question answer loses options or target | Actual host input has options/free text. Resume after disconnect and answer exact request; duplicates/stale responses do not answer another task's question. |
| RP-18 | RM-05; phone permission/catalog differs from host | Compare model/account/options and requested/effective permission for each supported provider. Unavailable and unsupported choices are explicit; no stale fallback becomes runnable authority. |
| RP-19 | RM-01; restart/restore resurrects revoked grant | Restart/profile switch/authority-store corruption and scoped restore fixture. Stable identity or explicit recovery; expired/revoked sessions stay invalid; no second engine owns the profile. |
| RP-20 | RM-03/RM-04; snapshot/events/optimistic echo diverge | Race snapshot with event delivery; duplicate/gap/out-of-order/late old-session updates. Same accepted command and ordered stable transcript converge after resume or resnapshot. |
| RP-21 | RM-06; attachments mismatch task or bytes | Upload image/file, interrupt/cancel/retry, edit queued prompt. Hash correct, one reference, exact task ownership, no orphan leak after policy cleanup. |
| RP-22 | RM-06; file path/asset scope escapes | Traversal, symlink, absolute path, ignored-file and profile substitution, wrong task and expired asset access. Allowed previews work; escapes fail at host. |
| RP-23 | RM-03/RM-06/RM-07; large data stalls phone/engine | Representative long transcript, large tool result, wide/binary diff and bounded media. Record data size, memory/latency/scroll evidence; bounded streams don't stall host execution. |
| RP-24 | RM-07; availability status ignores host lifecycle | With no selected task, enable host/keep awake, change power, toggle another reason, sleep/wake and graceful quit. Real assertion/reason readback and honest offline/ready state. |
| RP-25 | RM-07; DNS/TLS success mistaken for outside reachability | Physical phone with Wi-Fi disabled reaches owned direct public endpoint. Record destination/TLS/identity, accepted controlled task and reconnect. No cloud/tailnet fallback. |
| RP-26 | RM-07; NAT/permission failure hidden or V1 relays secretly | Deny LAN permission/camera, isolate guest Wi-Fi, block ingress or use no-inbound fixture. Precise error, preserved local work, no undeclared relay/login/upload. |

## Later capability acceptance

| ID | Owner / credible failure | Journey and required readback |
| --- | --- | --- |
| RP-27 | RM-08; terminal grant/session lost | Read-only observe denies input. Authorized write/resize/history/reconnect and leave/revoke obey session policy; unrelated processes survive. Retain PTY output + session ID trace. |
| RP-28 | RM-09; Git action targets wrong checkout | Review/commit selected files with concurrent edit; branch/worktree mutation and push rejection. Verify actual status/log/ref/diff and confirmation outcome. |
| RP-29 | RM-09; fork/restore/review provenance lost | Fork/continue/open parent/restore with unsupported and stale history cases. Same host/source identity and explicit irreversible-action semantics. |
| RP-30 | RM-09; PR creation confused with full review | Create/open linked PR where supported. Verify remote identity. Only certify hosted PR Code/comment/merge controls if separately implemented and tested. |
| RP-31 | RM-10; browser control leaks or races | Two phones acquire/release control; test host localhost, dialogs, upload/download and grant removal. Visible owner and scope; no generic desktop claim. |
| RP-32 | RM-10; simulator/HTML/MCP boundary bypassed | Open permitted device/app session; unsupported SDK/malformed bridge request/revoked session fails. Renderer/bridge owner verifies isolation, exact assets and outstanding request identity. |
| RP-33 | RM-11; scheduler accidentally lives on phone | Create/edit/pause/run/delete existing host schedule, phone offline/restart. Host execution uses exact saved model/worktree; stale schedule conflicts surface. |
| RP-34 | RM-11; phone settings/account/usage target wrong host | Mutate scoped setting, read usage, rotate supported provider auth and deny missing privilege. Explicit host/account and actual resulting configuration; no secrets in captures. |
| RP-35 | RM-11; update drops draft/work or creates version loop | Host/client update with working turn/pending outbox, incompatible protocol and failed update. Safe policy, recoverable state, final binary/version and reconnect evidence. |
| RP-36 | RM-12; push registration/awareness goes stale | Owned APNs/FCM deployment, token rotation, offline sign-out, revoked account, duplicate event, cold tap. Received push/thread identity and stale-expiry behavior. No receipt-only push certification. |
| RP-37 | RM-12; voice inserts into wrong draft | Unsupported OS/locale/platform/permission, cancel and navigation during recording. Correct draft/cancellation and actual transcriber outcome; no Android parity from generic recorder. |
| RP-38 | RM-12; incoming shares/widgets fail after cold start | Share media/text while offline, cold start/retry/duplicate; correct retained inbox/target. Widget/activity expiry is visibly stale; no background socket guarantee. |
| RP-39 | RM-13; optional route changes host/authority | Fail over and return across accepted routes, address reassignment inside identity cache, broken credential renewal. Same host/target and explicit transport; record actual underlay separately. |
| RP-40 | RM-13; hosted browser gets insecure/auth wrong | Browser pairing/CSRF/origin/cookie/HTTPS/version/revoke flows with owned host. HTTP LAN backend cannot be advertised as universally reachable from HTTPS hosted page. |
| RP-41 | RM-13; Android/tablet parity inferred from shared core | Native Android and tablet keyboard/landscape/background/network/screen-reader tests on actual clients. Record platform-specific missing features. |

## Release decision

Complete M0–M4 only after applicable first-release journeys pass with retained artifacts and reviewed exceptions. A source inventory, public app listing, demo screenshot, valid socket or queued receipt cannot certify operational parity. Later RP-27–RP-41 remain pending until their product scope, ownership and infrastructure decisions are made.
