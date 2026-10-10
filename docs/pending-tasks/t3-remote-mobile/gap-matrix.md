# T3 mobile capabilities mapped to current LUNA and pending work

All T3 rows describe reachable/wired implementation in the pinned upstream source, not certification of stable store binaries. LUNA “existing” means source foundation, not a live phone E2E. In particular, iOS edge UI/ledger writes do not establish accepted host command execution. Read [baseline/limits](README.md), [T3 workflow gates](05-mobile-workflows.md), [LUNA backend authority](08-luna-backend.md) and [actual iOS routes](09-luna-mobile.md).

Evidence abbreviations below link the detailed reports and their exact SHA-pinned call-site references: [T1 architecture](01-architecture.md), [T2 auth](02-auth.md), [T3 transport](03-transports.md), [T4 runtime](04-mobile-runtime.md), [T5 workflows](05-mobile-workflows.md), [T6 tools](06-mobile-tools.md), [T7 platform](07-mobile-platform.md), [L8 backend](08-luna-backend.md), [L9 iOS](09-luna-mobile.md). Each row is a concrete comparison; absence of a mobile surface does not imply absence of the corresponding Mac feature.

## Connections, authority and host lifecycle

| Capability | T3 source behavior / evidence | Current LUNA source / evidence | Pending owner / release |
| --- | --- | --- | --- |
| Headed host endpoint | Desktop hosts same environment API as other clients; T1/T3 | Headed app serves local-owner IPC; paired remote listener is headless opt-in loopback; L8 | RM-01, M1/P0 |
| Local-only phone control | Direct environment pairing without Connect; T2/T3 | iOS normal path requires configured edge/account; direct root absent; L9 | RM-01/RM-02, M1/P0 |
| Stable host identity | Environment ID separate from route; T1/T4 | Device/profile/task identities exist; new direct contract must bind them; L8 | RM-00/RM-01, P0 |
| Host descriptor/version | Public descriptor plus exact protocol compatibility; T1/T4 | No complete headed direct descriptor/negotiation product established; L8 | RM-00/RM-03, P0 |
| LAN/public routing | Reachable direct endpoint; pairing doesn't route; T3 | Dedicated direct HTTPS/pinned route remains planned; L8/L9 | RM-01/RM-07, M1/M4 |
| QR/manual enrollment | QR scanner and explicit address/code submission; T4 | No direct Mac QR/enrollment route in iOS; L9 | RM-02, M1/P0 |
| Incoming pairing deep link | Production prefill/auto-connect deliberately dev-only; T4 | Auth URL scheme exists; direct pairing route absent; L9 | RM-02; avoid unreviewed auto-connect |
| One-time grants | Default short-lived token creates separate persistent session; T2 | Hashed pending pairings/session foundations exist; L8 | RM-01; reuse and migrate |
| Secure phone session | Bearer catalog and proof-key storage in SecureStore; T2/T4 | Account auth Keychain exists, paired-host record absent; L9 | RM-02, P0 |
| Grant/scope policy | Broad standard operating set; sensitive access/relay admin separate; T2 | Remote method policy limited to selected reads/files/terminal; task mutations denied; L8 | RM-01/RM-04, P0 |
| Authenticated actor binding | Environment auth + scoped server RPC; T1/T2 | Remote adapter forwards method/params; actor/client fields need authenticated host binding; L8 | RM-01/RM-04, P0 |
| Method plus nested intent checks | Server RPC scope and domain command validation; T1/T2/T5 | Existing task API isn't safe merely by allowlisting its method; L8 | RM-04, P0 |
| Session inventory/revoke | Host admin revokes sessions, closes live WS; T2 | Authority-store foundations exist; headed device/session UI missing; L8 | RM-01/RM-02, P0 |
| Local forget vs revoke | Phone forget is local cache/credential removal; T2/T4 | New direct client must explicitly distinguish them; L9 | RM-02, P0 |
| Assets after revoke | Signed URL can survive own TTL; T2 | New direct asset policy needs explicit lifetime/session binding; L8 | RM-01/RM-06; proposed decision |
| Keep host serving | Desktop/manual/service hosts; stopped machine stays offline; T1 | Daemon/headless foundations, task power assertion; separate headed remote wake reason pending; L8 | RM-07, M4 |
| Tailscale/SSH/Connect | Separate routes; mobile cannot establish desktop SSH; T3/T4 | SSH loopback route and optional edge relay exist, with distinct authority; L8 | RM-13 later; excluded V1 |
| Account/tunnel discovery | Connect owns identity/link/bootstrap/tunnel service; T3/T7 | Owned optional edge config is separate; no equivalent direct V1 dependency; L8/L9 | RM-13 later, explicit decision |
| Multiple hosts/routes | Shared registry, ordered/learned routes, identity checks; T4 | Synced device registry exists; direct saved-host runtime absent; L8/L9 | One Mac RM-02; broader RM-13 |
| Route mismatch safety | Public preflight + identity check; 10s auth cache caveat; T4 | Need paired TLS/host/profile validation, no copied safety guarantee; L8 | RM-01/RM-03 |
| Host/client channel mismatch | Source v2/beta distinct from stable store; explicit protocol failure; T1/T4/T7 | New direct protocol/release compatibility policy pending; L8 | RM-00/RM-03 |

## State, recovery and core task workflows

| Capability | T3 source behavior / evidence | Current LUNA source / evidence | Pending owner / release |
| --- | --- | --- | --- |
| Shared connection runtime | Common client-runtime + platform adapters; T1/T4 | Rust/Swift/edge foundations; direct thin-client/native facade proposed; L8/L9 | RM-03; follow existing architecture owner |
| Cached offline state | Secure credentials separate from versioned SQLite data, explicit freshness; T4 | Org/chat2 snapshot cache and edge reconnect exist; L9 | RM-03; migrate identity/recovery |
| Foreground/network resume | Network path wake, probe vs socket replacement; T4 | NWPathMonitor/room kick/edge probe source paths; L9 | RM-03; direct revalidation |
| Reliable prompt delivery | Operation-owned outbox and stable command receipts; T1/T5 | Engine durable commands/admission exist; iOS synced rows not proof; L8/L9 | RM-04, M3/P0 |
| Transcript snapshot/stream | Shell/thread split and sequence/cursor repair; T1/T4 | Loro docs and UIKit transcript reuse exist; direct stream absent; L9 | RM-03, M2/P0 |
| Project/thread lists | Environment/project filters and search; T5 | Home All/project/archive source UI through edge registry; L9 | RM-03/RM-04 for direct authority |
| Create project/task | Host folder/create/clone/scratch workflow; T5 | iOS create project/session UI, registry fallbacks; backend ownership exists; L9 | Core RM-04; cloning/scratch only if required |
| Workspace/base branch | Root/new/existing worktree host choices; T5 | iOS root/new worktree/ref choices and host operations exist; L9 | RM-04 basic; RM-09 advanced |
| Provider/model/options | Host catalog, provider-specific choices, locked handoff; T5 | iOS catalogs with static fallback; canonical model/account/policy gaps; L9 | RM-05, M3/P0 |
| Effective permissions | Scoped operation and host/provider runtime options; T2/T5 | Legacy phone sandbox/autoApprove must map to canonical permissionMode; L9 | RM-05, P0 |
| Send/follow-up/queue | Native composer + operation outbox; T5 | UI/document enqueue source; paired task mutation denied; L8/L9 | RM-04, P0 |
| Queue edit/reorder/remove | Native queue controls, capability guarded; T5 | Queue lease/client paths and host owner exist; direct authority pending; L9 | RM-04, P0 |
| Steer/restart/stop | Capability/active-turn controlled actions; T5 | Engine operations exist; iOS steer/interrupt ledger writes not certified; L8/L9 | RM-04/RM-05 |
| Questions | Request options/free text + host response; T5 | Native QuestionPanel/respondInput source exists; direct admission missing; L9 | RM-05 |
| Approvals | Native approval actions tied to request/host; T5 | Desktop approval contract exists; iOS approval UI/action absent; L9 | RM-05, new phone work |
| Rich tool results | Mobile transcript renders host projection; T5 | Existing tool/input/image/text rows and expansion/copy; L9 | RM-03/RM-06 fidelity/recovery |
| Images/files/context | Image/file pickers, host file/thread refs, skills/commands; T5 | Photo subset exists; generic document/context picker absent; L9 | RM-06, M3–M4 |
| Archive/pin/settle/snooze | Broad capability-gated thread lifecycle; T5 | iOS archive shelf; host lifecycle features should be reused individually; L9 | Core archive RM-04; broad RM-09 |
| Fork/merge-back/helpers | Fork/merge-back source; observe/open native subagents; direct creation UI not found; T5 | Mac Side chats/review/fork owners exist; iOS surface missing; L8/L9 | RM-09; no second orchestration |

## Developer tools and operations

| Capability | T3 source behavior / evidence | Current LUNA source / evidence | Pending owner / release |
| --- | --- | --- | --- |
| Terminal | Host PTY read/operate, native view and bounded history; T6 | Host remote terminal methods exist, no phone terminal; ownership/home fallback need review; L8/L9 | RM-08 later, separate grant |
| File tree/read previews | Tree/search/text/media preview, read-only; T6 | Folder browse for project creation; file backend foundations, no phone tree/viewer; L8/L9 | RM-06, M4 |
| Phone workspace editor | Not wired in inspected T3 file route; T6 | No editor in inspected iOS routes; L9 | No T3 parity requirement; optional separate decision |
| Changed-file/turn diff | Native local Git diff/review and comment-to-agent; T6 | Desktop/read RPC foundations; no phone diff route; L8/L9 | RM-06 basic; RM-09 richer |
| Commit/push/PR create | Selected-file commit and pull/push/PR actions; T6 | Host Git/worktree services exist; no phone action route; L8/L9 | RM-09 later |
| Hosted PR Code/reviewer/merge | No mobile equivalent established; T6 | No phone route established; L9 | Do not inflate T3 mobile parity |
| Browser session preview | Host browser stream/input/control + transfer; T6 | Desktop browser owner exists; no phone session/grant product; L8/L9 | RM-10 later |
| Device simulator preview | Existing thread sessions; narrow proxy/control, no general setup drawer; T6 | No iOS phone viewer/grant route established; L9 | RM-10 later; host prerequisites |
| Generic Mac remote desktop | Not found as a T3 native mobile feature; T6 | No phone remote-desktop surface established; L9 | Separate Computer Use decision |
| HTML/MCP Apps | Mobile route exists subject to bridge/capability; T5/T6 | Existing pending renderer/app bridge owner; phone route absent; L9 | RM-10 consumes existing plan |
| Settings/project defaults | Host-targeted defaults + local phone preferences; T5 | Host settings exist; no phone settings surface; L8/L9 | RM-11 later |
| Usage/limits | Host/provider usage with windows/environment choice; T5 | Host diagnostics/usage foundations, no phone screen; L8/L9 | RM-11 later |
| Provider account management | Scoped supported auth flows; setup exceptions; T5 | Host/provider management exists; phone lacks route; L8/L9 | RM-11 sensitive later |
| Scheduled tasks | Real mobile editor and host scheduler operations; signature-edit exceptions; T5 | Scheduler/automation exists on Mac; no phone route; L8/L9 | RM-11; reuse scheduler |
| Host/provider updates | Scoped checks/update/restart with capability; T5/T7 | Existing maintenance services; direct phone authority/UX pending; L8 | RM-11 later |

## Native product, awareness and other clients

| Capability | T3 source behavior / evidence | Current LUNA source / evidence | Pending owner / release |
| --- | --- | --- | --- |
| Foreground attention | Connected request/activity state; T5/T7 | Existing task/question status UI; new direct state needed; L9 | RM-03/RM-05, V1 |
| Background push | Host activity -> relay -> native APNs/FCM tokens; T7 | No phone push registration/route established; L9 | RM-12 later, owned service decision |
| Live Activities/widgets | iOS foreground-start activity; Android native card/widget, distinct limits; T7 | No native extension target established; L9 | RM-12 later |
| Voice | iOS supported26+ local transcriber; Android fallback null; T7 | No phone voice route established; L9 | RM-12; choose actual platform strategy |
| Incoming share | Signed native extensions/intent -> durable inbox -> target; T7 | Photo picker only; no share extension route; L9 | RM-12 later |
| Clipboard/haptics/media | Concrete native controls and file/media viewers; T6/T7 | Copy/photos/generated-image rendering subset; L9 | RM-06/RM-07; broader RM-12 |
| Accessibility/tablet | Source labels/reduced-motion/tablet config, no full certification; T7 | Native source/UI foundations; physical audit unperformed; L9 | RM-07/RP-23; later platform coverage |
| Diagnostics/cache controls | Startup diagnostics, local cache settings; T5/T7 | Source logging/cache foundations, no phone settings route; L9 | RM-03 essential status; RM-11 richer |
| Mobile binary/OTA lifecycle | Signed native variants, runtime fingerprint and safe update flow; T7 | Existing iOS/TestFlight workflow, new direct compatibility acceptance pending; L9 | RM-00/RM-07, no copied Expo stack |
| Android client | React Native/native Android source; platform limits listed; T7 | No implemented Android client established in this audit | RM-13 later, explicit product choice |
| Browser companion | Hosted/local web directly connects to HTTPS environment; T3 | Direct browser client is existing plan's later scope; L8 | RM-13 later |

## Prioritization and duplicate prevention

The first missing product is the authorized direct Mac-to-iPhone path: RM-00–RM-07. The most important correctness gap is authenticated host admission, including principal binding and exact intent. A network listener or UI port alone does not close it.

The broader T3 inventory becomes later scoped work: terminals, Git/admin/scheduler screens, browser/device sessions, awareness/native extensions and other clients. Existing LUNA host owners remain the implementation owners; see [tasks](implementation-plan.md). Capabilities absent from T3 mobile itself are recorded as limits, not silently promoted to mandatory parity. No row is marked delivered by this research.
