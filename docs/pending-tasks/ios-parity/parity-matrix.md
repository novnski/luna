# iOS parity matrix

Evidence is **source-visible behavior**, not installed-binary or physical-device
certification. `Present-edge` means a current LUNA iOS path through its existing
edge/profile model; `Partial` means a subset or an incomplete end-to-end contract;
`New-direct` is work needed for the approved paired-Mac product; `Missing-iOS`
means no reachable implementation was found in the inspected phone route/view
inventory. Optional features remain explicitly separate from direct V1.

Source families: **Z-native** = Zeron client/mobile/layout and UIKit controllers;
**Z-delivery** = the durable delivery worker and recent suspension fixes;
**T-routes** = current T3 Stack and reachable screens; **T-runtime** = typed
client operations/outbox/host admission; **T-native** = share/dictation/awareness
implementations. See [research](research.md) and [source ledger](sources.md) for
immutable entry points. The earlier T3 lane reports remain useful for unchanged
paths; the seven changed referenced paths were re-read at the new cut.

| Capability | Reference evidence | Current LUNA iOS | Canonical owner / release |
| --- | --- | --- | --- |
| Shared native client and ABI | Z-native: Rust client + UniFFI; T-runtime: shared client with platform adapters | Swift models/sync and no owned Rust mobile facade in current workspace | IOS-00 builds facade; CORE-05 SDK; RM-03 owns connection/state runtime |
| Direct QR/manual paired Mac | T-routes Connections; Zeron source uses its mesh | New-direct | RM-01/RM-02; no duplicate IOS pairing ticket |
| Host/profile identity and Keychain | Both references have owned credential/lifecycle handling | Existing account Keychain; paired-host session is New-direct | RM-01/RM-02; IOS-00 migration boundary |
| Cached/offline/reconnect state | Z-delivery; T-runtime | Present-edge reconnect/cache; new direct readiness/repair still pending | RM-03; IOS-09 device acceptance |
| Sends across suspension, restart and closed chat | Z-delivery durable obligations; T-runtime stable operations | UI/ledger/escort exists; current host admission not proven by it | RM-04; IOS-03 presentation; IOS-09 physical races |
| New project/thread/worktree | Both expose project/new-session flows; T3 now holds work after failed setup | Partial source UI and registry fallback | RM-04 admission; IOS-03 real preparing/draft/held states |
| Streaming native transcript | Z-native display-list/viewport pipeline; T3 native/RN transcript | UIKit row reuse and incremental Swift parsing already present | IOS-01 measured shared/native migration |
| Text measurement and rendering agreement | Z-native shared fonts/CoreText fallback | Existing Swift layout and presentation tests | IOS-01: font bytes, Unicode/RTL/emoji/width/Dynamic Type proof |
| Phone/iPad shell and adaptive navigation | Z-native tabs/split root; T-routes columns | Existing navigation/project screens; incomplete native split/workbench | IOS-02; IOS-09 device matrix |
| Search/filter/group/pin/archive organization | Zeron ordered/searchable sections; T3 lifecycle menus | Basic project/all/archive views; fuller organization partial | IOS-02 basic organization; IOS-08 attention/lifecycle |
| Light/dark/system, fonts and appearance | Both have appearance settings | Current app forces dark; desktop has richer theme foundation | IOS-02 uses LUNA theme/settings owner |
| Native context chips and file/image metadata | Zeron mentions; T3 current host metadata | Image attachments exist; richer typed context partial | IOS-03; RM-06 metadata/assets |
| Queue/edit/reorder/remove/send-now | Both have native controls with durable client behavior | Present-edge UI/leases; direct path pending | RM-04 shared operations; IOS-03 native UX |
| Send vs follow-up vs steer vs Stop | Both have explicit active-run semantics | Present-edge command-row writes | RM-04; IOS-03; do not infer acceptance from local echo |
| Provider/model/reasoning/account selection | Host catalogs in both references | Partial; static/legacy fallbacks remain | RM-05 + CORE-04 capability source; IOS-04 actual picker |
| Host-installed custom provider plugin | Required by LUNA core vision; T3 uses host provider definitions | New LUNA core/mobile integration | CORE-04/08/21 + IOS-04; no provider binary on phone |
| Requested/effective permissions | T3 pending approvals and scoped controls | Legacy sandbox/autoApprove; canonical mode incomplete | RM-05 authority; IOS-04 native presentation |
| Questions and approvals | Question UI in both; T3 explicit approval actions | Questions present; explicit native approvals missing | RM-05 request/decision contract; IOS-04 screen/actions |
| LUNA Board/ticket/reviewer workflow | LUNA desktop is the owner; T3/Zeron surfaces are not equivalent Board implementations | Missing-iOS | IOS-05 adds scoped native Board surfaces using existing engine |
| Side chats/helper supervision | T3 agents route; LUNA desktop has fixed-parent lineage | Missing/partial iOS hierarchy/control | IOS-05 consumes SW-10 and Board identity/lifecycle |
| Current checklist and rich tool bodies | Zeron shared mobile tool drawing; T3 tool/plan display | Generic tool rows; active checklist/richer forms incomplete | ZI-03 normalized status; IOS-06 presentation |
| Images, files and attachment previews | Both have attachment/image paths | Images/generated images present; general host file workbench missing | RM-06 files/diffs; IOS-06 rich content |
| HTML replies and MCP Apps | T3 HtmlRenderWebView/McpAppWebView/fullscreen route | Missing-iOS; shared feature remains pending | IC owns host bridge/storage; IOS-06 owns native client integration |
| Video/unknown plugin content | LUNA CORE content/renderer goal; full upstream video behavior not certified here | Missing generic renderer; fallback required | CORE-14/15 + IOS-06; desktop plugin binaries remain host-owned |
| Conversation body Find | T3/LUNA planned feature contracts; native complete-history requirement | Missing-iOS | IC-5 contract → IOS-07 native navigation/highlights |
| Cross-chat/project history and workspace text search | T3 route/search and host file services; Zeron session search | Partial metadata browsing; full scopes missing | IOS-07; existing history owner, ZI-05 and RM-06 |
| Exact revision files/diff and comment-to-agent | T3 mobile review/file routes | Missing-iOS apart from limited PR identity display | RM-06 first read-only workbench; RM-09 advanced actions |
| Terminal | T3 ThreadTerminal route and host PTY operations | Missing-iOS | Existing RM-08, scheduled in expanded wave after RM-07 |
| Commit/push/branches/worktrees and PR state | T3 Git routes; local diff is distinct from hosted PR code/review | Missing-iOS beyond badge | Existing RM-09, expanded wave after RM-07 |
| Settings/usage/schedules/diagnostics | T3 settings routes; Zeron settings/diagnostics subset | Missing broad phone operations UI | Existing RM-11, read-first expanded wave after RM-07 |
| Browser/device streaming | T3 explicit preview sessions, not generic desktop sharing | Missing-iOS | RM-10 remains separately selected; no new duplicate ticket |
| Foreground attention and safe deep links | Both have task activity/routing | Basic status; broad needs-input/result navigation partial | IOS-08 with core-owned attention |
| Share sheet into a durable draft | T-native incoming-share inbox | Missing-iOS | IOS-10; native signed extension, explicit destination/send |
| Dictation and microphone selection | T-native on-device adapter and priority/settings | No matching current feature found | IOS-11 Deferred; device/locale capability and user choice |
| Live conversational voice | Z-native host-owned Codex call, phone audio and Live Activity | No matching current feature found | IOS-12 Deferred; distinct host/provider/transport contract |
| APNs / Live Activities / widgets | Zeron edge push/voice activity; T3 awareness/widget service | No equivalent delivery path certified | RM-12 service contract + IOS-13 Deferred native client |
| Build/signing/update/data migration | Zeron Rust build/bindings; T3 native fingerprint and release variants | Owned Xcode/TestFlight workflow exists; no Rust mobile build yet | IOS-00 build/data; IOS-09 native acceptance; IOS-14 release assessment |
| Accessibility, hardware keyboard and physical performance | Reference source has relevant controls; full quality remains a device question | Existing tests/QA are useful, not proof of complete parity | IOS-09 first-release gate; IOS-14 expanded regression matrix |

## Completion rules

One implementation owner can have multiple consuming views. RM-03/04/05 own
client state and authoritative command/capability contracts; IOS-01–04 own the
full native presentation. Their contract gates use a minimal native/SDK vertical
slice, then RM-07 requires the complete IOS acceptance. The overlap is an
explicit decomposition, not two teams implementing separate state machines.

Terminal/Git/phone administration reuse RM-08/09/11. They are now selected for
the later iOS parity wave, after direct V1. Browser/device streaming, additional
routes and hosted awareness remain separately scoped. Full hosted PR review on
the phone is not inferred from T3's local review/comment and “open PR URL” flows.

Every required row needs actual host/client behavior and retained evidence.
“Present-edge,” a code route, simulated provider response, or successful QR
pairing cannot close the complete native product. Deferred rows remain visible
in the final assessment and do not become an artificial parity percentage.
