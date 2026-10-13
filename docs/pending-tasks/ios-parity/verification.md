# Native and physical acceptance

These are planned gates. This documentation update runs catalog/link/hash and
checkpoint verification only. It does not certify an app build or mobile runtime.
Follow the repository test-audit skill and preserve existing useful iOS tests.

## Failure contracts and strongest owners

| Gate | Contract / credible regression | Owner and retained proof |
| --- | --- | --- |
| P01 | Wrong SDK/FFI/library version or architecture, actor-unsafe callbacks, orphan subscriptions, copied bundle/service IDs, destructive cache/draft/profile migration. | IOS-00: simulator plus actual-device build identities, generated-binding/library hashes, typed read/cancel through native transport, old-data/restart fixtures and signing readback. |
| P02 | Streaming reorders/duplicates rows, font/layout disagreement, invalid UTF-16 hit ranges, offscreen rebuilds, frame arriving after chat switch, scroll/runway jump. | IOS-01: independently captured source messages and row/anchor/range oracle, exact font bytes, native pixels/AX/recordings and baseline/candidate traces. |
| P03 | Unknown/deleted route shows fabricated content or never settles; split/collapse opens wrong chat; drafts/selection lost; inaccessible light/large-text layout; cold link assumes a grant. | IOS-02/08: real native phone/iPad navigation, explicit failed/offline/retry states, stable host/task/draft readback and keyboard/VoiceOver evidence. |
| P04 | Duplicate send, wrong worktree after setup, stale queue lease, invalid context chip, upload bytes lost, keyboard/IME/selection corruption. | IOS-03 plus RM-04 owner: real native interactions and independent command/checkout/provider/attachment receipts; no second outbox fixture. |
| P05 | Suspension before/after local persistence, upload ACK, host admission or reply loses work; detached chat stops delivery; revoked grants replay a queued operation. | RM-03/RM-04 with IOS-03/09: controlled loss/termination/network cases, stable request IDs, provider execution count, authoritative receipt repair and retained draft/bytes. |
| P06 | Static fallback model becomes runnable; wrong host/account; stale question/approval authorizes changed action; an Auto/Full task shows an approval card; unknown plugin provider breaks UI; secret enters history. | RM-05 + IOS-04: actual host catalogs and provider-facing allowed/denied/changed/revoked cases (approvals from a legacy Ask chat or LUNA ceiling grant), an Auto/Full task with no approval card, visible requested/effective policy and sanitized identity/readback. |
| P07 | Board retry starts another worker; phone reparents Side chat; unrelated helper stops; seen/unread state lies; closing a view becomes Stop; active deletion succeeds. | IOS-05/08 and existing Board/task-work owners: exact ticket/chat/run/parent/host receipts, native navigation and preserved unrelated work. |
| P08 | Unscoped asset/path fetch, unknown content loses message, hidden video/WebView lives forever, malicious page gains native APIs, stale MCP app session mutates, full private result leaks. | IOS-06 plus IC/CORE owners: real authorized media/HTML/app interaction, hash/frame/control readback, cross-task refusal, offscreen teardown and disabled/old-reader fallback. |
| P09 | Only visible rows searched, partial history reported complete, wrong Unicode/file range, stale query navigates another host, private app/tool data indexed. | IOS-07 consuming IC-5/ZI-05: independent text/file oracle, offscreen native navigation, live edits/streaming, cancel/offline/incomplete cases. |
| P10 | Native migration worsens input/open/frame/memory/energy, large Dynamic Type hides controls, keyboard/rotation loses input, callbacks outlive controller, simulated quality mistaken for physical behavior. | IOS-09/14: matched device/OS/build/data workloads, latency quantiles, process/graphics/memory/energy and native AX recordings; explicit budgets and blockers. |
| P11 | Share payload cleared too early, duplicated after crash, wrong destination, unbounded/escaping temporary file, denied/cancelled share silently sent. | IOS-10: actual signed OS share extension, durable inbox/content hashes, cold/warm/termination/replay, explicit send/decline and cleanup readback. |
| P12 | Mic/model/locale unavailable, route changed mid-recording, live call silently reconnects/replays, push token registered but no delivery, stale widgets look live, wrong-host tap, revoked account still receives events. | IOS-11/12/13 + RM-12: selected physical audio/provider/APNs/widget flows with exact package/device/service/call/target IDs, refusal/interruption/rotation/expiry and independent delivery evidence. |

## Reuse current tests and artifacts

- Existing `LunaTests`: ChatFrames, RegistryCore/Doc, DeviceRelayClient,
  NetworkReliability, MessageQueue, ComposerEditor, TranscriptFollow/Layout/
  Presentation, GeneratedImage and ChangeRequestTracking own useful contracts.
  Read their actual assertions before moving/removing them during the native port.
- Existing `LunaUITests`: ProjectSession, MobilePolish and Appshot scenarios
  exercise real UI boundaries. Keep current passing behavior and replace a
  fixture only when its new boundary has stronger independently observed proof.
- Existing engine task-command, queue, provider, Board, task-work, tool-output,
  identity and recovery suites remain the domain owner. Phone tests add native
  transport/FFI/lifecycle/presentation failures rather than replaying every engine
  assertion in Swift.
- Zeron's layout/CoreText/scroll and client live/demo tests provide scenario
  ideas and source mechanisms. Their reported results are not LUNA test results.
- T3's setup/outbox, shared inbox and microphone tests identify real failures;
  adapt their contracts, not snapshots coupled to React Native implementation.

Before new/changed tests, record observable behavior, credible regression,
existing coverage gap and the primary owner. No post-code unit tests or
production helpers whose only purpose is a test. Baseline failures must be
reproduced and understood; do not change expectations just to make migration green.

## Required device and lifecycle matrix

Use a representative iPhone and iPad for required native claims, including a
lower-end supported device when performance is part of the gate. Record iOS,
hardware, build/SDK/library/provider versions, exact host profile and app bundle.
Retain simulator evidence separately. Test portrait/landscape, split/multitasking,
large text, VoiceOver, reduced motion, light/dark, hardware keyboard and CJK/IME.

For command delivery, suspend or terminate at meaningful boundaries: draft saved,
thread not yet created, server-owned setup, upload persisted, admission sent,
reply lost, queued edit held and decision stale. Reopen another chat before
resuming. Check actual host/provider receipts and counts; a restored bubble is
not delivery evidence. Pairing, nearby routing and actual cellular direct access
retain RM-07's physical acceptance—no implicit relay or hosted-sync fallback.

Record cold/warm open, typing/event-to-paint latency and P50/P95/P99 frame/input
behavior, peak/settled memory, energy and resource release on the same workloads.
Choose numerical budgets from current LUNA measurements before the renderer
replacement. Include long real transcripts and representative large tool/media
payloads. Separate network latency, host work, parser/layout and painting cost.

## Build, identity and release discipline

Keep owned app IDs, signing and user data. New native targets/App Groups/entitlements
must belong to LUNA. Preserve the current iOS 26 floor unless a separate decision
changes it. Use the project Xcode targets and existing signing/release workflow;
Rust build phases must use the locked LUNA wrapper and shared budget/cache rules.
Record exact simulator/device architecture and binding/library compatibility.

Do not use production credentials, real support mutations or live checkout/Git
writes as disposable fixtures. Use a controlled host/profile/repository for
mutating acceptance and retain independent readback. When Mac GUI hosting is
needed, obey one-LUNA-GUI and graceful-quit rules; after meaningful Mac changes,
refresh and verify LUNA Dev. A phone test does not authorize force-quitting the
user's Mac app or replacing their phone data.

Each E2E retains commands, exit codes, hashes, source/device/build identity,
expected and observed state, sanitized logs, receipt/event correlation and the
required pixels/AX/video/network evidence. Hash reviewed artifacts and remove
only scoped disposable fixtures. Store/App Store Connect changes or publishing
require the actual release instruction; this plan performs neither.

IOS-14's required matrix includes the selected expanded terminal/Git/operations
work. Optional audio/awareness, RM-10 and RM-13 remain reported gaps until selected.
An unavailable device, provider capability, signing entitlement, service or CI
runner is a blocker/limitation for that claim, not an automatic pass.
