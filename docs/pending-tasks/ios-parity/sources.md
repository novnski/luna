# Sources, freshness and evidence

## Repository snapshots

- LUNA: `71d5db137e5bbfda13893278e558a7b0ce34ea79`.
- Official Zeron: `40091d469aa8a3f1ab1ec7cb404957b742940278`.
- T3 Code: `477282263a9115f911380b5aec7d4973267e4edc`.
- Prior T3 mobile audit: `a8bdfdb54181fb5c9d43f2747112c137a804d016`.
- Prior local T3 checkout: `50647de0143e68fde051eb1a0ccf4fe63f9947b2`.

Zeron's official main was fetched directly over SSH and already matched local
main. The configured personal `origin` was not changed or pushed. T3 was fetched
and fast-forwarded locally; the existing untracked
`docs/research/codex-comparison.md` retained its SHA-256. Neither upstream
repository received an agent-authored commit or push.

[Repository/source inventory](evidence/sources.json) and
[per-file hashes](evidence/files.jsonl) record the selected paths, counts and
source versions. The inventory is not an exhaustive semantic review. Assets and
generated bindings are excluded where specified; physical lines include tests.
The previous T3 audit referenced 201 concrete unique paths; seven changed since that
cut and their deltas were re-read, including the server's new metadata scope and
worktree/setup queue behavior. Its example URL's `<path>` placeholder is excluded.
See [freshness check](evidence/freshness.json).

[Planning verification](evidence/planning-verification.json) records local
link/source-hash checks and a fresh queue-store restore. The
[queue update](evidence/queue-update.json) records preserved evidence and the
three intentional status changes. The [SHA-256 manifest](evidence/sha256.json)
covers retained evidence; these results do not certify future native acceptance.

## Primary entry points

| Topic | Pinned source |
| --- | --- |
| Zeron native architecture | [iOS README](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/apps/ios/README.md), [mobile rewrite](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/docs/mobile-rewrite.md), [mobile manifest](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/crates/mobile/Cargo.toml) |
| Zeron transcript/native shell | [layout](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/crates/mobile/src/layout/mod.rs), [transcript list](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/apps/ios/Zeron/Transcript/TranscriptListView.swift), [split root](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/apps/ios/Zeron/Shell/SplitRootController.swift) |
| Zeron command/delivery | [mobile session facade](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/crates/mobile/src/client_ffi/session.rs), [delivery worker](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/crates/client/src/live/delivery.rs), [native composer integration](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/apps/ios/Zeron/Session/CoreSessionSource.swift) |
| Zeron voice/push | [remote voice](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/apps/ios/Zeron/Voice/RemoteVoiceController.swift), [notification client](https://github.com/zeronsh/zeron/blob/40091d469aa8a3f1ab1ec7cb404957b742940278/apps/ios/Zeron/App/PushNotifications.swift) |
| T3 mobile reachability | [Stack](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/Stack.tsx), [settings](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/features/settings/SettingsRouteScreen.tsx), [app config](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/app.config.ts) |
| T3 current composer/admission | [composer state](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/state/use-thread-composer-state.ts), [orchestrator](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/server/src/orchestration-v2/Orchestrator.ts), [authorization](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/server/src/auth/RpcAuthorization.ts) |
| T3 rich content | [HTML surface](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/features/threads/HtmlRenderWebView.tsx), [MCP App surface](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/features/threads/McpAppWebView.tsx), [fullscreen app route](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/features/threads/McpAppFullscreenScreen.tsx) |
| T3 shares and dictation | [share inbox](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/features/sharing/incoming-share-inbox.ts), [microphone settings](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/features/settings/SettingsMicrophoneRouteScreen.tsx), [native transcriber](https://github.com/pingdotgg/t3code/blob/477282263a9115f911380b5aec7d4973267e4edc/apps/mobile/src/native/voiceTranscription.ios.ts) |
| LUNA current iOS | [App root](../../../apps/ios/Luna/App/LunaApp.swift), [session writes](../../../apps/ios/Luna/Sync/SessionStore.swift), [catalog](../../../apps/ios/Luna/Models/HarnessCatalog.swift), [models](../../../apps/ios/Luna/Models/Entities.swift), [native rows](../../../apps/ios/Luna/Transcript/NativeTranscriptTable.swift), [Xcode project](../../../apps/ios/Luna.xcodeproj/project.pbxproj) |
| Current LUNA owners | [Control this Mac](../control-this-mac/README.md), [T3 audit](../t3-remote-mobile/README.md), [core/SDK](../extensible-core/README.md), [interactive content](../interactive-chat-surfaces/README.md), [test audit](../../audits/2026-10-09-test-audit/README.md) |

## Platform references

Apple documentation read on 11 October 2026:

- [Background execution extension](https://developer.apple.com/documentation/uikit/extending-your-app-s-background-execution-time): finite background time does not guarantee an indefinitely live socket. Persist and reconcile delivery.
- [SpeechTranscriber](https://developer.apple.com/documentation/speech/speechtranscriber): query actual device/locale support before offering the native speech path.
- [ActivityKit push lifecycle](https://developer.apple.com/documentation/ActivityKit/starting-and-updating-live-activities-with-activitykit-push-notifications): remote updates/start/end involve supported OS behavior, tokens and an APNs service. A reference app's narrower flow is not a universal platform limitation.

Native source, simulator behavior, physical-device behavior, service delivery
and store-distributed binaries are separate evidence levels. No competitor or
LUNA app was launched, no account/service was changed, and no runtime performance
claim was produced in this planning pass.
