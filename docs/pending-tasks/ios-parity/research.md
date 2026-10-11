# Research findings and upstream deltas

This is a targeted architecture/feature audit at the immutable source cuts in
README. It builds on the earlier T3 lane reports rather than repeating their
entire investigation. An inventory counts **73 LUNA iOS files**, **135 selected
Zeron iOS/shared-client/rendering files**, and **1,441 T3 mobile/client/contracts
files**. Counts include tests and comments, exclude generated bindings/assets
where stated, and do not mean every file received a semantic review.

## Zeron: what is useful now

The official main fetch matched `40091d46`; there were no new commits to pull
after the earlier desktop review. Its iOS implementation still differs
substantially from LUNA's current branch.

Zeron's [iOS guide](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/apps/ios/README.md)
and [mobile architecture](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/docs/mobile-rewrite.md)
describe a UIKit shell over `zeron-client`, `zeron-mobile`, shared Markdown/text
crates and UniFFI. Current code includes:

- A shared client with immutable session/workspace snapshots, connection and
  delivery lifetimes; a mobile facade with explicit session operations.
- Background transcript layout, stable rows/display lists, prefix-sum offsets,
  viewport queries, CoreText painting and frame-coalesced delivery.
- Native phone/tab and iPad split-root controllers, searchable/foldable session
  lists, appearance/settings, native composer, image/file mentions, questions,
  leased queue editing and image/tool views.
- Remote voice with phone audio and a capability-advertising execution host,
  explicit dropped-call state, native media/session lifecycle and a voice Live
  Activity. This is a different feature from dictating text into a composer.
- APNs session notifications backed by Zeron's edge activity/registration path.

These are source implementations, not measured LUNA results. In particular,
upstream simulator timing numbers and the assertion that measurement/painting
“cannot disagree” are not portable guarantees. LUNA needs actual font-byte,
CoreText fallback, Unicode, Dynamic Type and physical-device acceptance.

### Recent Zeron changes to carry into failure contracts

| Commit | Observed change | LUNA disposition |
| --- | --- | --- |
| [`e799aecc`](https://github.com/zeronsh/zeron/commit/e799aecc16c863b987839a626574f7a0339d4275) | Persisted viewer-delivery work; room ACK and host wake are separate obligations; resume delivery independent of which chat is open; bounded concurrent draining and background handoff. | RM-03/RM-04 own direct-client durable delivery; IOS-03/09 verify suspension/restart. Adapt the invariant, not Zeron's edge nudge endpoint. |
| [`8e6da959`](https://github.com/zeronsh/zeron/commit/8e6da95908ee536012dba4f3268ca4bd87fe602b) | Failed session opening no longer substitutes a fixture conversation; phone/iPad routers return to a truthful error state. | IOS-02/09 require explicit missing/offline/failed route behavior. LUNA's current `SessionView` instead shows an opening state; the identical upstream bug is not established in LUNA. |
| `73bd3c84` | Image/file mentions as composer chips with canonical references. | IOS-03 consumes the host attachment/context contract; text that resembles a path is not an asset grant. |
| `672cc144` | Searchable popovers and ordered sidebar sections across current source. | IOS-02 uses current host identity/ordering instead of copying a second task database. |
| `1f7b74a7` | Checklist changes touch shared mobile tool rendering as well as desktop work. | ZI-03 owns normalized checklist status. IOS-06 consumes it. A desktop “dedicated panel” commit title is not proof of a dedicated iOS panel. |

Zeron's current delivery worker in
[`crates/client/src/live/delivery.rs`](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/crates/client/src/live/delivery.rs)
still references its edge/room model. LUNA's approved direct client must use
host admission and receipts through the new SDK. Copying the whole upstream
client would reintroduce an incompatible authority and hosted-service dependency.

## T3: broader mobile workflows and the fresh delta

T3 was refreshed from local `50647de014` to `477282263a` (29 commits). The previous
LUNA audit used `a8bdfdb541`; 14 commits follow that audit. A freshness check of
its 201 concrete referenced paths found seven changed paths (one URL-template
placeholder was excluded). Those changed command,
route, authorization, orchestration and install-document paths were re-read;
unchanged paths retain the earlier pinned investigation as supporting evidence.

The current [native route table](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/Stack.tsx)
still exposes task/project flow, connections, terminal, local review/comments,
file tree/file/attachment, browser/device previews, fullscreen MCP Apps, queue,
agents, Git overview/commit/branches and settings. Its settings cover appearance,
notifications, usage, schedules, accounts, diagnostics and storage. Route presence
plus current client wiring is source evidence, not proof of a successful live action.

| Fresh observation | Why it matters |
| --- | --- |
| [`477282263a`](https://github.com/pingdotgg/t3code/commit/477282263a9115f911380b5aec7d4973267e4edc) keeps a thread's worktree and queues sends during setup; requests before server creation remain drafts; failed/cancelled setup holds queued work instead of falling into the main checkout. | RM-04 owns admission/preparation/outbox semantics. IOS-03 displays the actual draft/preparing/held/accepted states. Test late sends and cancelled setup independently. |
| [`0442f5bfcc`](https://github.com/pingdotgg/t3code/commit/0442f5bfcc) adds host filesystem metadata for file chips; `filesystemGetMetadata` is explicitly filesystem-read scoped and dispatched to the host service. | RM-06 and ZI file owners supply typed host metadata; IOS-03/06 render it. A client extension or guessed suffix does not establish file type/path authority. |
| `9afde1c6db` includes Pi native sessions in usage reports and mobile usage presentation. | RM-11 needs host/provider-specific usage capability, units and freshness. This is not a request to restore retired Pi in LUNA; future provider plugins remain dynamic through CORE-04/IOS-04. |
| The local fast-forward also includes microphone ordering/settings and newer source-control presentation work already present at the previous audit cut. | Include them in parity, but do not present them as new since the prior T3 report. |

T3's [microphone settings](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/features/settings/SettingsMicrophoneRouteScreen.tsx)
handle permission changes and remembered wired/Bluetooth/built-in/CarPlay
preference. Its [native dictation adapter](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/native/voiceTranscription.ios.ts)
checks supported device/locale and reports unavailable iOS 26 speech capability.
Its [share inbox](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/features/sharing/incoming-share-inbox.ts)
serializes durable mutation and deduplicates native payload delivery. These are
useful independent native workflows, not reasons to import React Native or T3
service credentials into LUNA.

## LUNA: preserve real foundations and fix missing connections

The [current iOS guide](../../../apps/ios/README.md) describes SwiftUI with UIKit
transcript reuse and Swift mirrors of the current edge/registry/session model.
Existing source includes project/session creation, archive, images, questions,
queue controls, generated-image support, reconnect logic and incremental
Markdown caching. It would be incorrect to call all of that absent or to assume
the entire document is reparsed on every token without measuring current code.

Concrete gaps at this source cut:

1. [`SessionStore.swift`](../../../apps/ios/Luna/Sync/SessionStore.swift#L550)
   appends run/steer/interrupt/input commands into a synced Loro ledger. The
   current host requires exact local executable admission. A visible Send or
   queued row is not proof the real provider executed it. RM-04 remains the owner.
2. [`HarnessCatalog.swift`](../../../apps/ios/Luna/Models/HarnessCatalog.swift)
   contains static provider/model/effort fallbacks, including legacy labels.
   [`Entities.swift`](../../../apps/ios/Luna/Models/Entities.swift#L345) still
   models legacy sandbox/autoApprove inputs. IOS-04 must consume actual host
   accounts/models/options and requested/effective permissions, including unknown
   plugin providers, rather than treating fallback catalog data as runnable.
3. [`LunaApp.swift`](../../../apps/ios/Luna/App/LunaApp.swift) currently fixes the
   dark appearance and routes through sign-in/org/home; the inspected view tree
   has no direct paired-Mac enrollment or comprehensive native settings/workbench.
   Preserve existing local/demo behavior while adding the direct profile.
4. The tracked iOS view inventory does not expose native Board, Side-chat/helper
   supervision, explicit approvals, terminal, Files/diff workbench, full history/
   conversation Find, usage or automation management comparable to desktop/T3.
   Some backend operations exist; phone authority, projection and actual UI
   remain separate implementation steps.
5. The Xcode project targets iOS 26 and owns `com.novnski.luna.ios`. T3's source
   targets iOS 18 with newer-feature guards. Preserve LUNA's current deployment
   floor for the native migration; lowering it is a separate compatibility
   decision, not an automatic parity requirement.

## Platform and product boundaries

Apple's background execution extension is bounded; a phone socket cannot be
promised to run indefinitely after suspension. Persist intent and reconcile
receipts on foreground rather than relying on a live view/controller.
[Apple background execution](https://developer.apple.com/documentation/uikit/extending-your-app-s-background-execution-time).

On-device speech availability depends on supported device/locale/model state;
query it and handle preparation/cancellation. Push-driven Live Activities need
the appropriate APNs/server/token lifecycle. Current Apple APIs can support
remote starts on eligible versions; a T3 source flow that arms an activity in
the foreground is not a universal iOS limitation.
[SpeechTranscriber](https://developer.apple.com/documentation/speech/speechtranscriber),
[ActivityKit push lifecycle](https://developer.apple.com/documentation/ActivityKit/starting-and-updating-live-activities-with-activitykit-push-notifications).

The parity plan preserves direct/no-relay V1. Optional notification/voice
services require their own owned architecture and release decision. No upstream
APNs keys, bundle IDs, App Groups, hosted endpoints or signing assets are adopted.
