# Native iOS implementation tickets

Live status and claims belong to the shared queue. These tickets extend the
approved native/shared-core direction. New module names are proposals; inspect
current owners before editing. Define failures first and use test-audit's
strongest-boundary E2E contract. No post-implementation unit tests or test-only
production seams. Evidence and P01–P12 gates are in [verification.md](verification.md).

## IOS-00 — Establish the owned SDK/native facade and migration build

**P0; CORE-05, RM-00. Owners:** LUNA iOS project/build, proposed mobile facade,
public SDK bindings and native transport/Keychain adapters.

Introduce a LUNA-owned Rust/native boundary and generated versioned Swift
bindings, adapting useful Zeron structure. Prove one real typed descriptor/read
operation and cancellation through Swift platform networking and the shared SDK;
do not create a test-only echo API. RM-03 will own connection/cache/reconnect.
Preserve `com.novnski.luna.ios`, signing, current iOS 26 floor and existing
account/profile/cache/draft data. Define explicit old-edge/direct-profile
migration and downgrade handling. Wire simulator/device architectures and release
builds through LUNA's locked build wrapper/budget, not an unreviewed upstream
script or separate uncontrolled cache.

**Failure focus:** mismatched bindings/library, wrong actor/thread callback,
lost cancellation, copied upstream service/identity, destructive cache migration
or a second connection state machine.
**Gate:** P01 actual simulator/device build and typed native read/cancel, symbol/
ABI/schema identity and old-data migration evidence. Preserve license notices
for any selected source reuse; no whole-repository cherry-pick.

## IOS-01 — Deliver the shared native transcript pipeline

**P1; IOS-00, RM-03. Owners:** shared Markdown/text/layout modules and native
transcript host. Preserve existing LUNA row/part identity and input state.

Adapt immutable changed-entry projections, incremental parsing/layout, stable
rows, viewport queries, display models, frame coalescing and off-main preparation
from Zeron. Reuse LUNA's core/document types. Keep fonts, fallback glyph metrics,
UTF-16 ranges, bidirectional text and CoreText drawing consistent. Preserve
follow/gesture anchors, own-send runway, offscreen updates, tool/image expansion,
selection/copy/link activation and theme/width changes. Existing Swift incremental
caches/row reuse are the measured baseline, not presumed broken code.

**Failure focus:** duplicated/reordered parts, full-document work per token,
measurement/paint mismatch, scroll jumps, stale frame after navigation, and
unbounded image/display-list lifetime.
**Gate:** P02 baseline/candidate native traces and independent canonical row,
glyph/range/height/anchor readback across long real transcripts and physical
hardware. Upstream simulator numbers cannot certify LUNA.

## IOS-02 — Build the adaptive native shell, organization and appearance

**P1; IOS-00, RM-03. Owners:** app/scene routing, native phone/iPad containers,
session/project lists, settings/appearance and local presentation state.

Add native tabs/sections and iPad split navigation using the same host-owned
project/task records. Preserve draft-to-created-task handoff, selection and
per-scene state. Provide useful basic filtering/search, pin/section/archive
navigation where the host supports it, explicit disconnected/loading/missing/
failed states and actionable retry. Reuse LUNA themes for dark/light/system,
font/density preferences and reduced motion. Keep connected-host/profile identity
visible; appearance is local presentation, not authority.

**Failure focus:** fake/demo data shown on failure, a permanent opening state,
wrong chat after collapse/expand, lost draft or unread state, and theme/large text
hiding controls. Verify the actual LUNA failure before attributing Zeron's fixed
fixture fallback bug to it.
**Gate:** P03 phone/iPad rotation/multitasking/navigation and native AX/keyboard
readback, including invalid/deleted/offline routes and restart restoration.

## IOS-03 — Complete native composer, context and queue UX

**P1; IOS-00, RM-04, RM-05, RM-06. Owners:** native composer/context/attachment/queue
presentation; RM-04 retains command/outbox and setup semantics.

Expose send/follow-up/steer/Stop, queue edit/renew/reorder/remove/send-now and
draft restoration through the shared client API. Keep stable message/request,
task, checkout and host identities. Add typed file/image/context chips and host
metadata, bounded upload/retry/cancel and safe context discovery. Before a host
creates a thread, retain the draft; during accepted worktree setup, queue behind
that setup; on failed/cancelled setup, hold rather than execute in the main
checkout. Presentation must distinguish local draft, pending delivery, host
acceptance, execution and completion.

**Failure focus:** ghost success, duplicate sends after suspension, stale edit
lease, swallowed IME text, attachment lost on queue edit, wrong checkout after
setup, or file-looking text treated as permission.
**Gate:** P04/P05 native typing and real provider command/receipt readback across
lost replies, backgrounding, process death, reconnect and attachment transfer.
Reuse RM-04's failure owner instead of adding another queue backend.

## IOS-04 — Expose host capabilities, provider settings and interactions

**P1; IOS-02, RM-05, CORE-04. Owners:** model/account/options picker, requested/
effective policy view, native questions and approvals.

Use the current target host's actual provider/model/account/options/capability
projection. Unknown installed provider plugins must appear without an iOS enum
or static catalog update. Stale/offline data is labeled; fallback names cannot
authorize a run. Map canonical permissions and exact request/revision/decision
IDs into native question and approve/deny flows. Preserve secret-input handling,
free text, changed requests, continued sessions and host-side settlement.

**Failure focus:** legacy sandbox/autoApprove broadens policy, wrong account or
host after navigation, unsupported model/reasoning offered as executable, or
old approval authorizes a new operation.
**Gate:** P06 actual allowed/denied provider-facing flows, new/missing plugin,
account/mode changes and stale/revoked/replayed decisions with native evidence.
The phone does not install desktop provider binaries or receive their secrets.

## IOS-05 — Add native Board, Side chats and helper supervision

**P1 expanded wave; RM-07, IOS-02, SW-10. Owners:** existing Board/chat/task-work
services plus a narrowly scoped paired-phone projection/command facade.

Build native Board filtering/detail/create/edit/start/continue/stop and clear
worker/reviewer phase, result, diff and evidence navigation. Preserve the shipped
fixed-parent main/Side-chat model; phone selection cannot reparent a worker.
Expose existing helper read/wait/scoped-stop readback using exact task/run/host
authority. Add only the required paired-phone operation policy—current Board
methods being local-only does not justify giving the phone owner IPC. Keep
GitHub/external writes on their existing explicit confirmation boundary.

**Failure focus:** duplicate worker on retry, hidden active reviewer, another
chat stopped, task view closure mistaken for Stop, reparenting, or stale board
state changing execution settings.
**Gate:** P07 real phone ticket/Side-chat/helper journeys with authoritative
Board receipts and retained existing engine E2E evidence.

## IOS-06 — Complete native rich content and extension-aware rendering

**P2 expanded wave; IOS-01, RM-06, IC-3, IC-4, CORE-14, CORE-15, ZI-03. Owners:**
native content projection/rendering; IC/CORE remain asset/bridge/schema owners.

Display typed tool details/current checklist, approved generated media, expanded
file/diff previews, HTML and MCP Apps through the shared content contract.
Implement native controls, fullscreen/inline lifecycle and supported video using
reviewed platform primitives. Respect the original provider/server session and
host capabilities for interactive actions; use no duplicate MCP connection.
Preserve unknown/disabled renderer fallbacks, asset ownership, source/export,
privacy, offscreen disposal and old/mobile reader compatibility. A desktop plugin
binary is not downloaded and executed on the phone.

**Failure focus:** media appears without authorized bytes, hidden WebView keeps
playing, native view overlays dialogs, stale app session writes, secret fallback
text, or renderer removal destroys a conversation.
**Gate:** P08 real phone interaction/playback/scroll/restart and declined/allowed
fixture app actions with hashes, pixels and denied asset readback. Core renderer
acceptance alone does not prove iOS presentation.

## IOS-07 — Add native conversation and workspace search/navigation

**P2 expanded wave; IOS-01, IOS-02, RM-06, IC-5, ZI-05. Owners:** native Find/
history/files routes consuming existing canonical search/data services.

Provide selected-conversation Find, project/history discovery and workspace text
search with clearly distinct scope. Reuse IC-5 text/range semantics and ZI-05
host search. Navigate to offscreen message blocks or the correct file/line using
stable source IDs and host/checkout revisions. Preserve composer focus/drafts,
manual fold state and query generation through streaming/navigation. Label
partial/offline history and unavailable host search honestly.

**Failure focus:** searches only mounted rows, Unicode offsets highlight wrong
text, old query opens another host's same-named path, or incomplete cache gives
a false zero. Do not index private tool output/app DOM by accident.
**Gate:** P09 native search-to-content/line against independent text/file oracles
and authorized complete-history fixtures, including cancellation and live changes.

## IOS-08 — Refine foreground attention and safe task navigation

**P2 expanded wave; RM-07, IOS-02, IOS-04. Owners:** existing host attention/
lifecycle service and phone presentation/deep-link handling.

Show current running, needs-input, failed and finished work with reliable
counts, seen/unread behavior and scope. Expose permitted pin/settle/snooze/wake/
archive/delete actions using existing host contracts; urgent live requests remain
actionable. Handle cold/warm task links only after resolving the saved host/profile
and revalidating the target. Foreground activity is sufficient for this task;
APNs delivery belongs to IOS-13/RM-12.

**Failure focus:** stale badges, tap-to-wrong-task, link conveys authority,
notification dismissal approves an action, hidden urgent question, or deleting
active/uncertain work.
**Gate:** P03/P07 native attention, link, lifecycle and restart cases with exact
host receipts and no unsolicited tab/focus changes.

## IOS-09 — Certify native first-release quality on real devices

**P1; IOS-01/02/03/04, RM-06. Owner:** native UI acceptance; RM-07 owns the
complete remote-product and public-route acceptance.

Establish iPhone/iPad device/OS/build identity and representative real host data.
Exercise Dynamic Type, VoiceOver, contrast/reduced motion, keyboard/IME, hardware
keyboard, orientation/split, selection, long transcript, tools/images and queue/
question overlays. Measure input/open/frame latency, memory/CPU/energy and cleanup
against the recorded baseline. Exercise foreground/background/termination and
network changes while drafts/transfers/decisions are in flight. Fix current
failures at their owner, retaining existing proven optimizations.

**Gate:** P01–P07/P10 required native scenarios pass with actual device evidence
and measured budgets. A simulator screenshot cannot close physical networking,
audio, power or device frame-pacing requirements. RM-07 consumes these artifacts.

## IOS-10 — Add durable native share intake

**P2 expanded wave; IOS-03, RM-06, RM-07. Owners:** owned iOS Share extension/
App Group, durable draft inbox and existing attachment/context API.

Receive supported text/URLs/images/files from the share sheet, persist a bounded
draft and temporary bytes, choose an exact destination and send only deliberately.
Use stable content/delivery IDs to deduplicate retries and cold-start replays.
Preserve source bytes until durable ingestion; distinguish consumed, cancelled,
rejected and expired drafts. Native extension signing/entitlements are LUNA-owned.
This is an OS app extension, distinct from LUNA's desktop plugin runtime.

**Failure focus:** source cleared before persistence, duplicate send after
relaunch, wrong host selected, temporary file escapes scope or denied permission
silently loses a share.
**Gate:** P11 actual signed share-sheet → inbox → destination → draft/send with
content hashes, refusal/cancel/replay/cleanup and app-termination evidence.

## IOS-11 — Optional on-device dictation and microphone preferences

**Deferred; SW-16, IOS-03. Owners:** native audio/speech adapter and composer.

Select the supported device/locale/model policy, then implement explicit
record/stop/cancel/transcribe-to-draft. Adapt T3's availability checks and
microphone ordering/route-change UX using native APIs. Handle model preparation,
OS permission revocation, Bluetooth/wired route changes, interruptions and empty/
partial results. Preserve the user's existing draft and cursor. Do not turn a
dictation result into an automatic send or silently upload audio to another service.

**Gate:** P12 real supported-device/locale and unavailable/denied/interrupted
cases, with audio lifetime and draft readback. This is not live conversational voice.

## IOS-12 — Optional host-owned conversational voice

**Deferred; SW-16, RM-12, IOS-04, RM-07. Owners:** selected host/provider voice
contract, native media/session controller and exact call identity.

Choose the actual provider/protocol and trust/transport requirements from current
capability evidence. Zeron's phone-audio/host-agent architecture is a reference,
not proof LUNA exposes that provider operation. Preserve host credentials,
explicit call start/end, microphone/output routes, mute/interruption, drops,
proximity and permitted background audio. Reconnect only under a reviewed
continuation policy; a dropped call cannot duplicate a provider session or action.

**Gate:** P12 actual host/provider/phone call and refusal/drop/stop/route cases.
Any Live Activity support consumes IOS-13's owned activity contract. Keep this
out of direct V1 until separately selected and certified.

## IOS-13 — Optional push, Live Activities and widgets

**Deferred; RM-12, IOS-08, RM-07. Owners:** owned awareness/registration service
and native notification/widget targets, preferences and safe navigation.

Implement only selected notification/activity/widget surfaces using committed
host activity, per-device registration and owned APNs topics/keys/App Groups.
Handle permission, token rotation/unregister, account/host changes, expiry,
foreground/current-chat suppression and validated taps. Choose foreground-arm
versus supported remote-start behavior explicitly. Widget/activity snapshots are
bounded projections with stale/expired states, never an execution engine.

**Gate:** P12 actual signed physical-device delivery/update/end and denied/revoked/
stale/cold-start paths with independent host/service/APNs correlation. A token
registration or visible fixture does not pass. Preserve direct V1's no-relay boundary.

## IOS-14 — Review expanded parity and release readiness

**P1 release gate; RM-07/08/09/11, IOS-05/06/07/08/09/10. Owner:** native release
assessment and final required-feature matrix.

Assemble the required source-mapped feature set and independent owner evidence.
Run real phone/tablet workflows on a signed build, including terminal, Git and
ordinary operations from their existing RM owners. Verify old-data upgrade,
SDK/host version mismatch, cache/draft survival and realistic performance after
restart. Reuse owned TestFlight tooling and validate signing/artifacts; publishing
or changing store/account settings still needs the appropriate release instruction.

**Gate:** every required row passes or the ticket remains blocked with precise
gaps. Record optional IOS-11/12/13, RM-10 and RM-13 as deferred/unavailable unless
selected; then add them to the required set before claiming completion. Keep
binary/build/device/provider/source hashes, actual test outcomes and release
limitations. Do not claim full T3/Zeron parity from a partial first release.
