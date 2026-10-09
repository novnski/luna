# L10 test audit: iOS, QA tooling, visual fixtures and CI

- Lane: `L10-ios-qa-tooling` (branch `test-audit/L10-ios-qa-tooling`), baseline `e4a0f534`.
- Scope: 155 files. 17 iOS test files (`apps/ios/LunaTests/**`, `apps/ios/LunaUITests/**`), the iOS
  `-e2e` rig `apps/ios/Luna/App/E2ERunner.swift`, 60 `crates/ui/examples/*` fixture files
  (6 with inline tests), 71 QA/verify/e2e/prepare scripts under `scripts/`, 6 workflows.
- Baseline: 128 Swift test declarations (114 `LunaTests`, 14 `LunaUITests`; 2,583 lines),
  23 inline Rust tests in fixtures (1,696 lines of `mod tests`), 54,407 fixture lines,
  14,967 script lines, 934 workflow lines, 175 rig lines.
- Ledger unit: one Swift/Rust test declaration; one script or fixture for QA tooling (or one
  scenario inside it when the scenario differs); one workflow.
- Method: read every test in full plus its production owner and peers (Rust/edge
  conformance vectors), routing (`.github/workflows/*.yml`, scripts, docs) and history.
  For every script: every `cargo test` target and name filter was resolved against the tree
  (a filter matching nothing exits 0); every RPC method a driver calls was checked against
  the engine; every fixture path and evidence env var it relies on was checked for a consumer.
  Nothing was compiled or run (coordinator-owned).
- Result: 7 + 128 + 23 + 131 = 289 rows. Marks: R 271, F 9, C 0, D 9.

## Ledger

### Workflows and iOS rig

| Mark | Test | Evidence |
| --- | --- | --- |
| R | `.github/workflows/ui-tests.yml` | Routes OpenCode/sync/engine regressions, `luna-ui --lib`, macOS browser/preview fixtures, `gpui_macos frame_source_recovery`, `-only-testing:LunaTests`, Linux browser. Every named target and filter resolves (`native_diff_font_geometry`, `notify::tests::`, `opencode::`, 4 engine targets). No change needed by this lane. |
| R | `.github/workflows/windows.yml` | Harness native-fixture, UI native/headless split, packaging, `gpui_windows` layout, engine/auth targets, startup/lifecycle/rendering scripts. All targets resolve. |
| R | `.github/workflows/preview-tests.yml` | `luna-preview` on Linux/macOS and edge typecheck/test. Routing intact. |
| R | `.github/workflows/deploy.yml` | Manual edge/landing deploy; reruns edge typecheck + tests before `wrangler deploy`. Release contract, not a test owner. |
| R | `.github/workflows/release.yml` | Release packaging + version guard + signed updater manifest. Release contract. |
| R | `.github/workflows/testflight.yml` | `build-for-testing` compiles LunaTests/LunaUITests before archive/upload; processing-state verification. Release contract. |
| R | `apps/ios/Luna/App/E2ERunner.swift` | `-e2e`/`-e2e-live` launch rig (AppModel.swift:84-95): the only iOS-side probe of the live workspace room, device relay `ListFolders`/`ListModels`, relay-created spaces and the command plane against `wrangler dev` + mock-harness headless engine; writes `Documents/e2e.log`. Log-only (never fails the process) and step 4 needs an out-of-repo "e2e-big-doc" seeder (never committed; `git log -S`), so that step can only time out. Retained: deleting needs AppModel.swift edits and no other iOS live-stack probe exists. Follow-up. |

### iOS unit tests (`apps/ios/LunaTests`, CI `ios-tests`)

| Mark | Test | Evidence |
| --- | --- | --- |
| R | `AppshotTests::testDesktopAppshotTransportRendersSourceWithoutObservedText` | Privacy contract: desktop appshot context (XML-escaped app/title) becomes card metadata, observed text never reaches the visible prompt or queue text. Only owner of the iOS appshot parser (`Composer/Appshots.swift`). |
| R | `AppshotTests::testPhoneQueueEditKeepsContextAndAttachmentOnlyMessage` | Queue edit must re-append hidden appshot context and use the attachment-only placeholder for empty text; regression would leak or drop context on phone edits. |
| R | `AppshotTests::testAmbiguousAndMalformedContextNeverBecomesAVisiblePrompt` | Security/robustness: duplicate, malformed and DTD/entity payloads yield no presentation and no leaked text. |
| R | `AppshotTests::testPlainAttachmentsAndUserTextRemainUnchanged` | Negative control with a real input (plain text, ordinary image) so the parser cannot over-strip. |
| F | `ChangeRequestTrackingTests::testContractDecodesEveryState` | Was a Swift encode->decode round trip of a plain derived `Codable` struct (junk: cannot see wire drift). Now decodes the host's literal camelCase JSON for `open`/`merged`/`closed` and compares to the expected value, pinning the phone decoder to the shape `crates/proto` `checkout_change_request_status_round_trips_all_states_as_camel_case` and `device_routing.rs` `assert_public_change_request_payload` pin on the Rust side. |
| R | `ChangeRequestTrackingTests::testLocalAndRemoteChatsDeduplicateByHostCheckoutPath` | Watch-target dedup per (device, cwd); regression doubles host watches. |
| R | `ChangeRequestTrackingTests::testDemandRequiresActiveBranchAndFallsBackToOwningSpace` | Archived/no-branch chats create no demand; space fallback must match device; unsupported devices excluded. |
| R | `ChangeRequestTrackingTests::testSnapshotMustMatchHostBranchAndCheckout` | Stale snapshot (other branch/checkout) must not label a chat; legacy chats without checkout still resolve. |
| R | `ChangeRequestTrackingTests::testSuccessfulNoneReplacesVisiblePullRequest` | Authoritative "no PR" must clear a previous badge (entities.rs contract comment). |
| R | `ChangeRequestTrackingTests::testUnknownMethodAndAccessibilityLabelsAreExplicit` | Old-host fallback detection (`unknown method:` text) and VoiceOver label; both user-visible. |
| R | `ChatFramesTests::testPinsTheWireLayout` | Cross-language chat2 frame vector shared with `crates/sync/src/chat_frames.rs` and `edge/src/chat-frames.test.ts`. |
| R | `ChatFramesTests::testRoundTripsAndRejectsMalformed` | Codec rejects empty, truncated, non-object and oversized headers; real input space. |
| R | `ChatFramesTests::testStateHeaderParsesServerShape` | Parses the server state header shape; rejects missing keys. |
| R | `ChatFramesTests::testPlanCatchUp` | Table of the catch-up decision ported from `chat_client.rs plan_catch_up`, incl. the 2026-08-10 seeded-room regression. |
| R | `ComposerEditorTests::testSendingMarkedTextClearsNativeStorageWithoutLosingFocusOrNextDraft` | IME marked-text commit + clear on send in a real `UITextView`; XCUITest `typeText` cannot produce marked text, so `MobilePolishTests::testSendClearsComposerAndKeepsTheNextDraft` does not cover it. |
| R | `DeviceRelayClientTests::testUnaryStillResolvesOk` | Unary RPC reply routing after the stream protocol was added. |
| R | `DeviceRelayClientTests::testStreamDeliversItemsInOrderAndItemKeepsItPending` | Stream items keep the request pending; duplicate terminal frame ignored. |
| R | `DeviceRelayClientTests::testStreamErrFinishesWithRpcError` | `err` frame surfaces as `RelayError.rpc` (old-host detection relies on it). |
| R | `DeviceRelayClientTests::testSocketCloseFailsEveryPendingRequest` | Socket loss fails unary and stream waiters (no hang). |
| R | `DeviceRelayClientTests::testCancellingConsumerLeavesOtherRequestsAlone` | Consumer cancellation removes only its stream. |
| R | `DeviceRelayClientTests::testInterleavedFramesRouteByRequestId` | Interleaved multi-request demux. Uses test-only `unaryCount`/`streamCount` accessors (production file, other owner; follow-up). |
| R | `GeneratedImageTests::testImagePartDecodesFromSyncedDocumentAndSurvivesJSONRoundTrip` | Session-doc image part decode (document schema). |
| R | `GeneratedImageTests::testMalformedImagesBecomeVisibleErrors` | Table: relative/empty path, empty name, SVG/empty MIME become visible errors, never silent drops. |
| R | `GeneratedImageTests::testImageRowsPreserveOrderOwnerIdentityAndMetadataCorrections` | Row ids stable across status, version changes on owner/metadata correction, turn start. |
| R | `GeneratedImageTests::testRemoteOwnerPrecedesHostWithoutDuplicates` | Fetch device order (owner, then host). |
| R | `GeneratedImageTests::testDecoderBoundsDimensionsAndMemoryAndVerifiesActualFormat` | Security/memory bounds: downscale to 2048, reject mismatched format, >4096 px, oversized bytes. |
| R | `GeneratedImageTests::testGeneratedCacheCannotReuseAnUnvalidatedAttachment` | Cache isolation between unvalidated attachments and validated generated images, per device. |
| D | `HarnessCatalogTests::testCodexFallbackStartsWithAstraAndExposesItsTraits` | Copied inventory: expected id/label/ladder/option ids are the literals of the static Swift table it reads. Cannot detect drift from a host (Rust no longer has a bundled Codex catalog: `crates/harness/src/codex/catalog.rs` header). Remaining proof: `testDefaultReasoningMatchesDesktopPreference` and `testChoiceFallsBackToAdvertisedDefault` still exercise the same default model, its `high` ladder and its `serviceTier` choices (`fast` -> "Fast", stale -> `default`). Unlocks: test only. |
| R | `HarnessCatalogTests::testDefaultReasoningMatchesDesktopPreference` | Port of desktop `default_reasoning` (high, then medium); small pure function owned here on iOS. |
| R | `HarnessCatalogTests::testChoiceFallsBackToAdvertisedDefault` | Option choice fallback to advertised default for a stale selection. |
| R | `MessageQueueTests::testEmptyQueueEditPreservesAttachmentsAndDiscardsOnlyTextOnlyRows` | Edit-to-empty policy (attachment-only placeholder vs delete). docs/reference/message-queue.md names this suite. |
| R | `MessageQueueTests::testTerminalEditFailuresRetainBothDraftsAndAllowLocalRecovery` | `missing`/`lost` lease results keep both drafts and stay terminal after later failures. |
| R | `MessageQueueTests::testRetryableEditFailuresKeepProtectionAndAttachmentPolicy` | `conflict`/`unavailable` keep the lease and protection. |
| R | `MessageQueueTests::testEnqueueStampsTheRowAndShowsItImmediately` | Phone-written row fields (issuer, time, hold default) on the real Loro doc. |
| R | `MessageQueueTests::testMovingByArrowsAndToAnIndex` | Movable-list reorder bounds. |
| R | `MessageQueueTests::testRemovalWaitsForHostAcknowledgementAndSuppressesCompetingActions` | Removal waits for host ACK and blocks competing send/move while pending. |
| R | `MessageQueueTests::testFailedAndMalformedRemovalKeepTheRowAndSurfaceError` | Failed/malformed/offline replies keep the row and surface an error. |
| R | `MessageQueueTests::testSendNowUsesHostMethodAndLeavesProjectionToSync` | `SendQueuedMessageNow` method name and no local projection. |
| R | `MessageQueueTests::testPrimaryActionRespectsCapabilitiesAttachmentsAndDeliveryGates` | Capability, pending and delivery-gate policy table. |
| R | `MessageQueueTests::testProtectedRowsCannotBeDeliveredEvenThroughDirectStoreCall` | Safety: review-gated rows never dispatch even via direct store call. |
| R | `MessageQueueTests::testExternalKeyboardCommandInvokesSubmitWithoutChangingText` | Cmd+Return submit binding on the real text view. |
| R | `MessageQueueTests::testHarnessMustAdvertiseMidTurnSteering` | Steering is unknown unless both capability fields arrive; only `step-boundary` steers mid-turn. |
| R | `MessageQueueTests::testRowsWrittenByAnotherDeviceDecode` | Cross-device document schema (desktop-written rows, missing hold flag stays steerable). |
| R | `MessageQueueTests::testHoldForTurnEndRoundTripsAndCanBeDisabled` | Hold flag written both ways (the `false` case is unique to this test). |
| R | `MessageQueueTests::testDeliveryGatesDecodeAndUnknownKindsFailClosed` | Unknown gate kinds fail closed to review-required. |
| R | `MessageQueueTests::testLabelsAndNeighbours` | Queue label, one-line preview and neighbour bounds used by the panel. |
| R | `MessageQueueTests::testLegacyAttachmentTrailerIsNotExposedAsQueueText` | Legacy attachment trailer hidden; literal text untouched. |
| R | `NetworkReliabilityTests::testVersionTripleParsesPlainAndSuffixed` | Port of proto `version_triple` (accepted inputs). |
| R | `NetworkReliabilityTests::testVersionTripleRejectsJunk` | Rejected inputs incl. `0.2.12rc1`. |
| R | `NetworkReliabilityTests::testVersionGateComparison` | Gate comparison at the minimum version boundary. |
| R | `NetworkReliabilityTests::testPendingRefRoundTrip` | `pending://` ref wire format shared with uploads.rs. |
| R | `NetworkReliabilityTests::testParseRefRejectsForeignAndMalformedRefs` | Ref parser rejects foreign/malformed refs. |
| R | `NetworkReliabilityTests::testStashSaveLoadDelete` | Upload stash persistence on disk (storage). |
| F | `NetworkReliabilityTests::testChunkSizeWireInvariants` | Removed the tautology `(b64 / 4 * 3) % 3 == 0` (true for every integer). Kept the real invariants: base64 chunk `% 4 == 0` and Cloudflare 1 MiB envelope headroom (same as `crates/ui/src/attachments.rs` UPLOAD_CHUNK_B64_CHARS). |
| R | `NetworkReliabilityTests::testAttachmentDeadlineFormula` | Desktop-parity whole-attachment deadline incl. the 15-minute cap (`attachment_deadline` in attachments.rs). |
| R | `NetworkReliabilityTests::testEchoFrameRoundTrip` | Relay echo kind wire value (`device_room.rs ECHO_KIND`). |
| R | `NetworkReliabilityTests::testPendingOwnsTracksLifecycle` | `owns(id:)` false after `fail(id:)`; the timeout task keys link teardown on it. Unique to this test. |
| R | `NetworkReliabilityTests::testWorktreeSpecOmittedWhenNil` | RunRequest legacy wire shape for old hosts. |
| R | `NetworkReliabilityTests::testRetryReissuesADeadSendAttemptExactlyOnce` | Exactly-once retry on the real command ledger doc (fresh id, same message id, no duplicate). |
| R | `NetworkReliabilityTests::testWorktreeSpecEncodesCamelCase` | RunRequest worktree camelCase keys (proto agent.rs). |
| R | `RegistryHlcTests::testOrdersByMsCounterDeviceLexicographically` | Cross-language registry vector (edge registry-core.test.ts, crates/doc registry.rs). |
| R | `RegistryApplyOpTests::testCreatesRowsViaUpsertNeverViaUpdate` | Cross-language vector. |
| R | `RegistryApplyOpTests::testFieldLevelLwwNewerWinsOlderLosesTiesLose` | Cross-language vector (idempotent re-push). |
| R | `RegistryApplyOpTests::testSameMsConflictsSettleByDeviceIdDeterministically` | Cross-language vector. |
| R | `RegistryApplyOpTests::testNullFieldValuesDeleteTheFieldStillClocked` | Cross-language vector. |
| R | `RegistryApplyOpTests::testDeleteTombstonesOnlyWhenCausallyNewerThanTheRow` | Cross-language vector. |
| R | `RegistryApplyOpTests::testDeleteOnMissingRowPlantsGuardTombstone` | Cross-language vector. |
| R | `RegistryApplyOpTests::testPerFieldClockOverridesPreserveOriginalCausality` | Cross-language vector. |
| R | `RegistryApplyOpTests::testRowToSeedOpRoundTripsTombstones` | Cross-language vector. |
| R | `RegistryApplyOpTests::testConvergesOverEveryCausallyValidArrivalOrder` | Cross-language convergence over all 6 permutations. |
| R | `RegistryApplyOpTests::testDropsAnUpdateThatOutrunsItsRowsCreate` | Cross-language vector. |
| R | `RegistryValidateOpTests::testAcceptsWellFormedOpsAndRejectsMalformedOnes` | Cross-language validation table. |
| R | `RegistryMaxClockTests::testReturnsTheNewestClockAcrossFieldsAndTombstone` | Cross-language vector. |
| R | `RegistryDocPersistenceTests::testPersistenceBlobRoundTrips` | iOS persistence blob (storage): cursor, gcFloor, pending ops with null deletes, clock, in-flight reset. |
| R | `RegistryDocPersistenceTests::testFreshDocSendsNullHelloCursor` | Wire contract: a fresh replica sends a null hello cursor. |
| R | `HlcMonotonicClockTests::testNeverEmitsAtOrBelowTheLastClockAcrossWallClockRegression` | Monotonic HLC across wall-clock regression. |
| R | `HlcMonotonicClockTests::testCounterOverflowCarriesIntoMs` | Counter overflow carry (boundary). |
| R | `HlcMonotonicClockTests::testPersistedClockSurvivesRestartMonotonically` | Restart monotonicity through persisted clock. |
| R | `TranscriptFollowTests::testStreamGrowthAndKeyboardResizeDoNotReleaseFollow` | Follow state machine (docs/mobile-polish.md names it as independent of layout). |
| R | `TranscriptFollowTests::testTouchWithoutMovementDuringGrowthPreservesFollow` | Touch without drag keeps follow. |
| R | `TranscriptFollowTests::testReadingHistoryStaysReleasedDuringStreamingAndResize` | Released follow survives streaming and resize. |
| R | `TranscriptFollowTests::testMovingUpInsideReengageBandStillReleases` | Re-engage band direction rule. |
| R | `TranscriptFollowTests::testMovingTowardBottomReengagesAndContinuesFollowing` | Re-engage on approach to bottom. |
| R | `TranscriptFollowTests::testExplicitJumpRearmsEvenAfterLargeHistoryScroll` | Jump button re-arms. |
| R | `TranscriptLayoutTests::testGeneratedImageArrivalResizesTheRowAndKeepsTheTailVisible` | Real SwiftUI/UIKit layout keeper (CI); generated image resize keeps the tail pinned. |
| R | `TranscriptLayoutTests::testAccessibilityPagesThroughVirtualizedHistory` | VoiceOver paging through the virtualized table. |
| R | `TranscriptLayoutTests::testEarlyHistoryScrollDoesNotLeaveBlankSpaceBelowTail` | No blank viewport while scrolling during hydration. |
| R | `TranscriptLayoutTests::testToolGroupsRevealAndCollapseThroughIntermediateHeights` | Animated disclosure with tail attached; diagnostic attachments. |
| R | `TranscriptLayoutTests::testToolToggleReversalWhileStreamingKeepsTailAttached` | Reversals while streaming, per-frame motion bound. |
| R | `TranscriptLayoutTests::testToolDisclosureKeepsHistoryHeaderAnchored` | History anchor stable under disclosure. |
| R | `TranscriptLayoutTests::testAppendingBlockAlsoRefreshesTheCompletedParagraph` | Regression: the finished paragraph receives its final words when a new block arrives in the same chunk. |
| R | `TranscriptLayoutTests::testRealKeyboardAndComposerMoveWithPinnedTranscript` | Real keyboard motion keeps transcript and composer together. |
| R | `TranscriptLayoutTests::testInterruptedKeyboardMotionKeepsTranscriptAttached` | Interrupted keyboard motion. |
| R | `TranscriptLayoutTests::testKeyboardKeepsStreamingRunwayPromptAnchored` | Runway prompt anchored during keyboard motion + streaming. |
| R | `TranscriptLayoutTests::testKeyboardKeepsHistoryReadingAnchorStill` | Reading position unchanged by keyboard. |
| R | `TranscriptLayoutTests::testAnimatedComposerResizeKeepsTranscriptAttachedThroughoutMotion` | Composer resize motion. |
| R | `TranscriptLayoutTests::testRowsRemainRealizedBeneathTopSafeArea` | Rows realized behind the header. |
| R | `TranscriptLayoutTests::testPendingSendAnimatesAndRetainsRunwayThroughAdoptionAndReopen` | Send glide, runway through adoption and reopen. |
| R | `TranscriptLayoutTests::testWarm600TurnOpenRealizesTailWithoutScroll` | 600-turn warm open realizes tail (CI owner; `MobilePolishTests::testHugeTranscriptKeyboardFoldsAndWarmReopen` is the manual app-level twin). |
| R | `TranscriptLayoutTests::testDelayed600TurnHydrationRealizesTailWithoutScroll` | Late hydration (CI owner; UI twin `testDelayedHydrationWithoutScrolling`). |
| R | `TranscriptLayoutTests::testRepeatedComposerAndKeyboardResizesKeepTailVisible` | Repeated resizes. |
| R | `TranscriptLayoutTests::testLargeTailShrinkClampsWithoutUserScroll` | Shrink clamps without user scroll. |
| R | `TranscriptLayoutTests::testStreamingAppendKeepsPinnedTailVisible` | Streaming append. |
| R | `TranscriptLayoutTests::testStreamingAcrossRepeatedKeyboardResizesKeepsTailVisible` | Streaming + resizes. |
| R | `TranscriptLayoutTests::testCompactLandscapeViewportStillRealizesTail` | Landscape viewport. |
| R | `TranscriptLayoutTests::testAccessibilityTextResizeKeepsTailVisible` | Dynamic Type resize. |
| R | `TranscriptLayoutTests::testNarrowViewportKeepsTailVisible` | Narrow viewport. |
| R | `TranscriptLayoutTests::testLocalRunwaySurvivesCompletionAndResizeThenHandsOffToLongReply` | Local runway lifecycle. |
| R | `TranscriptLayoutTests::testRepeatedWarmReopensResetReleasedFollow` | Warm reopen resets released follow. |
| R | `TranscriptLayoutTests::testSingleLongTurnStreamsAcrossReusedRows` | Row reuse for one long turn. |
| R | `TranscriptPresentationTests::testReattachedStreamingTextStartsVisibleAndOnlyNewTextFades` | Veil: reattach shows existing text, fades only new text. |
| R | `TranscriptPresentationTests::testNewSuffixIsRegisteredBeforeItsFirstRender` | Veil registration order (no flash). |
| R | `TranscriptPresentationTests::testVisibleSuffixNeverDimsAcrossBurstsOrMarkdownLengthChanges` | Monotonic alpha across bursts/markdown length changes. |
| R | `TranscriptPresentationTests::testDesktopUserMessageCollapseThresholds` | Desktop-parity collapse thresholds (400 chars / 6 lines). |

### iOS UI tests (`apps/ios/LunaUITests`, manual `xcodebuild -only-testing`, not in CI)

| Mark | Test | Evidence |
| --- | --- | --- |
| R | `AppshotUITests::testRemoteAppshotsRemainReadableAndOpenOnPhone` | Real-app appshot cards, lightbox, queue gallery, landscape, screenshots (docs/appshots.md, docs/screenshots/appshots). Every identifier exists in the app. |
| R | `MobilePolishTests::testImmediateScrollAfterRepeatedSessionOpens` | Real gesture scroll after reopen; screenshots (docs/mobile-polish.md). |
| R | `MobilePolishTests::testLongUserMessageShowMoreAndLess` | Show more/less survives reopen. |
| R | `MobilePolishTests::testStreamingComposerAndCancelledBackSwipeKeepTranscriptVisible` | Cancelled back swipe while streaming. Curly `Here’s` matches because swift-markdown smart punctuation is on by default. |
| R | `MobilePolishTests::testSendClearsComposerAndKeepsTheNextDraft` | Real typing incl. emoji/CJK/multiline clears and keeps the next draft. |
| R | `MobilePolishTests::testGlassProjectSelectorSelectionAndDismissal` | Project filter selection/dismissal. |
| R | `MobilePolishTests::testHugeTranscriptKeyboardFoldsAndWarmReopen` | App-level twin of the CI layout keepers with screenshots. |
| R | `MobilePolishTests::testDelayedHydrationWithoutScrolling` | App-level late hydration. |
| R | `MobilePolishTests::testToolGroupRepeatedOpenAndClose` | Accessibility value Expanded/Collapsed. |
| R | `MobilePolishTests::testToolRailAndComposerPicker` | Model picker from composer. |
| R | `MobilePolishTests::testQuestionPanelAndNewSessionFlow` | Question panel + new session. |
| R | `MobilePolishTests::testLocalSendRunwayAndLandscape` | Send button above keyboard in landscape, tail inside transcript. |
| F | `ProjectSessionUITests::testAccountWithoutProjectsOffersProjectCreation` | Removed `XCTAssertFalse(app.buttons["new-projectless-session"].exists)`: the identifier left the app in the same commit that wrote the line (666d8cf0), so it passes for an unrelated reason. Kept the real assertion (`New space…` offered). |
| F | `ProjectSessionUITests::testProjectSessionStillUsesItsCheckoutAndCanSend` | Removed vacuous `XCTAssertFalse(app.buttons["session-host"].exists)` (SessionHostPickerSheet deleted in 666d8cf0). Checkout/ref controls, context label and send remain. |

### Inline Rust tests in fixtures (not routed by CI or any script; see Follow-ups)

| Mark | Test | Evidence |
| --- | --- | --- |
| R | `crates/ui/examples/activity-attention-fixture.rs::tests::fixture_accounts_and_auth_are_root_scoped_without_native_credentials` | Hermeticity: provider homes under the root and an in-memory credential broker (dropping `use_in_memory_credentials_for_tests()` would reach the platform Keychain). Path equalities are partly literal copies; the broker check is the contract. |
| D | `crates/ui/examples/activity-attention-fixture.rs::tests::approval_fixture_uses_decision_labels_not_an_input_placeholder` | Identity copier: asserts back the literals `request_entry` builds (`["Deny","Allow once","Cancel"]`, `approval.then(..)`); cannot detect drift from production approval labels, which the harness/engine approval suites own. Unlocks: test only (`request_entry` still seeds the fixture). |
| R | `crates/ui/examples/child-activity-fixture.rs::tests::fixture_has_no_real_provider_or_completed_parent` | Mock harness, scenario rows present, in-memory broker, root-scoped homes, `guarded_root` rejects an arbitrary temp root (real guard logic). |
| D | `crates/ui/examples/drafts-fixture.rs::tests::fixture_startup_loads_saved_settings_and_fresh_roots_default` | Added for 466caa80 (fixture used `UiSettings::default()`), but never calls fixture code: reverting drafts-fixture.rs:1817 to `default()` keeps it green. Its assertions duplicate `crates/ui/src/settings.rs::tests::round_trip` (persists `FollowUpBehavior::Steer`, `load == saved`) and `composer_send_behavior_is_opt_in_for_old_and_partial_settings` (default `Queue`), both in CI. Unlocks: test only. |
| R | `crates/ui/examples/drafts-fixture.rs::tests::l11_initialization_is_isolated_registered_and_restart_idempotent` | Fixture seed identity/idempotency on a real EngineCore; no other owner of the L11 seed. |
| R | `crates/ui/examples/drafts-fixture.rs::tests::l11_async_seed_fresh_and_reopen_preserves_ready_state` | Seed receipts, attachment ownership in SQLite, user edits survive reseed. |
| R | `crates/ui/examples/drafts-fixture.rs::tests::l11_legacy_draft_metadata_repair_is_exact_and_once_only` | Fixture repair routine is exact and once-only. |
| R | `crates/ui/examples/drafts-fixture.rs::tests::l11_async_seed_recovers_after_history_without_duplicate_turn` | Interrupted seed resumes without a duplicate turn. |
| R | `crates/ui/examples/drafts-fixture.rs::tests::l11_compact_control_exposes_running_completed_and_unknown` | Control-file protocol for the L11 compact control (used by `scripts/l11-fixture-control.sh`). |
| R | `crates/ui/examples/drafts-fixture.rs::tests::l11_synthetic_codex_controls_production_automatic_context_projection` | Fixture control actions drive the synthetic Codex automatic-context events; the projection contract itself is owned by `crates/engine/tests/task_commands.rs::automatic_codex_context_events_gate_real_queue_drain_and_restart`. Kept for the fixture plumbing. |
| R | `crates/ui/examples/drafts-fixture.rs::tests::l11_ipc_compact_consumes_script_control_and_applies_receipt` | Real IPC compact through the fixture control file. |
| R | `crates/ui/examples/drafts-fixture.rs::tests::turn_recovery_initialization_matches_request_and_is_restart_idempotent` | Turn-recovery seed does not rewrite cwd/config on reopen. |
| R | `crates/ui/examples/drafts-fixture.rs::tests::fixture_control_ignores_baseline_and_consumes_each_sequence_once` | L09/L10 control-file sequence semantics (baseline ignored, once-only). |
| R | `crates/ui/examples/drafts-fixture.rs::tests::fixture_control_drives_one_tool_boundary_then_done_without_eof` | Synthetic harness emits one tool boundary then Done; replay ignored. |
| R | `crates/ui/examples/history-operations-fixture.rs::tests::fixture_auth_is_in_memory_only` | Hermeticity (in-memory broker). |
| R | `crates/ui/examples/history-operations-fixture.rs::tests::unsupported_task_backfills_visible_handoff_without_provider_invocation` | Fixture backfill never invokes the synthetic provider; reseed idempotent across restart. |
| R | `crates/ui/examples/history-operations-fixture.rs::tests::fixture_seed_uses_only_synthetic_completed_history` | Seed shape (completed turns with checkpoints, permission rows). |
| R | `crates/ui/examples/history-operations-fixture.rs::tests::archive_round_trip_preserves_independent_child_and_fake_metadata_restart` | Fixture metadata app-server state across fork/archive/restart; missing ids invent nothing. |
| R | `crates/ui/examples/history-operations-fixture.rs::tests::disabled_metadata_assembly_never_invokes_a_provider_fallback` | Fixture assembly without metadata stays Pending (`ProviderAcceptanceUnknown`) and never calls the metadata client. |
| R | `crates/ui/examples/history-operations-fixture.rs::tests::fixture_provider_persists_collision_safe_boundary_forks` | Synthetic provider fork ids are collision-safe across reopen. |
| R | `crates/ui/examples/session-history-fixture.rs::tests::fixture_auth_is_in_memory_and_root_scoped` | Hermeticity (in-memory broker, homes under root). |
| R | `crates/ui/examples/task-organization-fixture.rs::tests::fixture_auth_broker_is_hermetic_and_has_no_platform_fallback` | Hermeticity; the only non-engine caller of `set_in_memory_store_locked_for_tests`. |
| R | `crates/ui/examples/task-organization-fixture.rs::tests::l20_opt_in_seed_uses_only_synthetic_registered_git_checkouts` | L20 opt-in seed: real Git checkpoints, shared vs isolated restore, idempotent reopen. |

### QA fixtures and scripts (one row per file; scenario notes inline)

| Mark | Test | Evidence |
| --- | --- | --- |
| R | `crates/ui/examples/accounts-fixture.rs` | Interactive Accounts fixture with optional `LUNA_PROVIDER_QA_OUTPUT` PNG captures (fails on capture error); cited by ui-polish audit design-system findings. |
| R | `crates/ui/examples/activity-attention-fixture.rs` | Hermetic L15 native QA fixture run by `scripts/run-macos-l15-fixture.sh`; seeds approval/input attention rows; 18 check sites; referenced by docs/qa/appearance-audit-2026-09-29.md, error-feedback README. Inline tests in the section above. |
| R | `crates/ui/examples/app-icon-fixture.rs` | Production appearance controller -> native AppKit application icon E2E. Drives the production paths it names (compile is the coordinator check); 6 check sites; pass receipt/readback. Refs: docs/design/ui-polish-audit-2026-10-06/findings/design-system.md, docs/reference/app-icons.md. |
| R | `crates/ui/examples/appearance-audit.rs` | Appearance audit: measured WCAG 2.2 contrast for every UI family, theme variant, accent and surface preference through the production appearance path; writes results + pass receipt, failure file on breach; 3 check sites. Refs: docs/design/chat-workbench/appearance-audit.md, docs/qa/appearance-audit-2026-09-29.md +8; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/appearance-fixture.rs` | Isolated end-to-end rendering of the production Appearance page. Drives the production paths it names (compile is the coordinator check); 6 check sites; pass receipt/readback. Refs: docs/design/chat-workbench/appearance-audit.md, docs/design/ui-polish-audit-2026-10-06/findings/design-system.md +2; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/appshots-fixture.rs` | Appshots layout evidence fixture; the `appshots-fixture` feature gates 50 examples; docs/appshots.md + docs/screenshots/appshots. |
| R | `crates/ui/examples/board-authoring-fixture.rs` | Headless GPUI E2E for Discuss with AI (A6). Drives the production paths it names (compile is the coordinator check); 7 check sites; pass receipt/readback. Refs: docs/design/project-board/pb0-contract.md. |
| R | `crates/ui/examples/board-chat-ui-fixture.rs` | Headless GPUI E2E for ticket identity on chat surfaces (Board slice A10. Drives the production paths it names (compile is the coordinator check); 3 check sites; pass receipt/readback. Refs: crates/ui/src/shell/board/chat_badges/fixture.rs, docs/design/project-board/pb0-contract.md. |
| R | `crates/ui/examples/board-detail-fixture.rs` | Headless GPUI E2E for the Board detail slice (A9). Drives the production paths it names (compile is the coordinator check); 10 check sites; pass receipt/readback. Refs: docs/design/project-board/pb0-contract.md. |
| R | `crates/ui/examples/board-github-fixture.rs` | Headless GPUI E2E for "From GitHub issue" (A7). Drives the production paths it names (compile is the coordinator check); 32 check sites; pass receipt/readback. Refs: docs/design/project-board/pb0-contract.md. |
| R | `crates/ui/examples/board-page-fixture.rs` | Headless GPUI E2E for the Board page (project board slice A8). Drives the production paths it names (compile is the coordinator check); 116 check sites; pass receipt/readback. Refs: docs/design/project-board/pb0-contract.md. |
| R | `crates/ui/examples/browser-fixture.rs` | CI keeper: ui-tests.yml `macos-frame-recovery` (via run-macos-browser-fixture.sh, `result.txt` required) and `linux-browser` (test-linux-browser.sh, X11+Wayland); 53 check sites; screenshots uploaded. |
| R | `crates/ui/examples/browser-fixture/linux.rs` | Linux half of browser-fixture (X11/Wayland native input); CI `linux-browser` job. |
| R | `crates/ui/examples/browser-fixture/transcript_links.rs` | Transcript link -> Browser coverage inside browser-fixture; documented in docs/transcript-browser-links.md; CI via browser-fixture. |
| R | `crates/ui/examples/chat-chrome-fixture.rs` | Headless GPUI E2E for three pieces of chat chrome. Drives the production paths it names (compile is the coordinator check); 4 check sites; pass receipt/readback. Refs: 2026-10-08 audit coverage. |
| R | `crates/ui/examples/chat-mermaid-fixture.rs` | Isolated native GPUI transcript/adapter E2E, with PNG + cache readback. Drives the production paths it names (compile is the coordinator check); 6 check sites; pass receipt/readback. Refs: docs/chat-mermaid.md, docs/design/ui-polish-audit-2026-10-06/findings/timeline.md +1. |
| R | `crates/ui/examples/chat-placement-fixture.rs` | Headless GPUI E2E for explicit sidebar placement. Drives the production paths it names (compile is the coordinator check); 74 check sites; pass receipt/readback. Refs: docs/design/harness-evolution/slice-3-verification.md, docs/design/persistent-fork-threads/verification.md +2; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/chat-reference-search-bench.rs` | Failure-first E2E/performance acceptance for the `#` chat reference index (`crates/ui/src/composer/chat_reference_search.rs`), written against the real module before it is wired into the composer. Drives the production paths it names (compile is the coordinator check); 2 check sites; pass receipt/readback. Refs: docs/design/agent-delegation-ux/verification.md; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/chat-references-fixture.rs` | Headless GPUI E2E for grouped chat references and clean child results. Drives the production paths it names (compile is the coordinator check); 2 check sites; pass receipt/readback. Refs: docs/design/agent-delegation-ux/verification.md, docs/design/ui-polish-audit-2026-10-06/findings/composer.md; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/chat-workbench-fixture.rs` | Headless GPUI E2E for the central chat workbench. Drives the production paths it names (compile is the coordinator check); 401 check sites; pass receipt/readback. Refs: docs/design/chat-workbench/appearance-audit.md, docs/design/chat-workbench/verification.md +16; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/child-activity-fixture.rs` | Hermetic L22 child-activity fixture; guarded `/private/tmp/luna-l22-*` root; 20 check sites; only its header documents it (follow-up). Inline test in the section above. |
| R | `crates/ui/examples/child-chats-pane-fixture.rs` | Headless GPUI E2E for child chats in the right pane. Drives the production paths it names (compile is the coordinator check); 56 check sites; pass receipt/readback. Refs: docs/design/ui-polish-audit-2026-10-06/findings/shell.md, docs/reference/related-chats-redesign.md; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/chrome-shortcuts-fixture.rs` | Headless GPUI E2E for the chrome controls' shortcut tooltips and the shortcuts they name. Drives the production paths it names (compile is the coordinator check); 25 check sites; pass receipt/readback. Refs: own header only. |
| R | `crates/ui/examples/claude-coordination-fixture.rs` | Headless GPUI fixture for CC-13/CC-16 surfaces. Drives the production paths it names (compile is the coordinator check); 10 check sites; pass receipt/readback. Refs: own header only. |
| R | `crates/ui/examples/claude-extensions-fixture.rs` | Headless GPUI E2E for CC-12 (Claude extensions, hooks and typed skills). Drives the production paths it names (compile is the coordinator check); 20 check sites; pass receipt/readback. Refs: docs/design/ui-polish-audit-2026-10-06/findings/design-system.md, docs/design/ui-polish-audit-2026-10-06/findings/timeline.md. |
| R | `crates/ui/examples/claude-history-fixture.rs` | Headless GPUI E2E for CC-10 Claude history in the shared rewind dialog. Drives the production paths it names (compile is the coordinator check); 11 check sites; pass receipt/readback. Refs: own header only. |
| R | `crates/ui/examples/claude-prompts-fixture.rs` | Headless GPUI fixture for the CC-03 Claude permission/dialog prompts. Drives the production paths it names (compile is the coordinator check); 2 check sites; pass receipt/readback. Refs: crates/ui/src/composer/claude_prompts_e2e.rs. |
| R | `crates/ui/examples/claude-review-fixture.rs` | Headless GPUI E2E for Claude Code review (CC-15). Drives the production paths it names (compile is the coordinator check); 12 check sites; pass receipt/readback. Refs: crates/ui/src/shell/native_review.rs. |
| R | `crates/ui/examples/claude-tool-results-fixture.rs` | Headless GPUI rendering of Claude tool results fetched from the host journal. Drives the production paths it names (compile is the coordinator check); 4 check sites; pass receipt/readback. Refs: 2026-10-08 audit coverage. |
| R | `crates/ui/examples/codex-prompts-fixture.rs` | Headless GPUI fixture for the codex-adapter prompts. Drives the production paths it names (compile is the coordinator check); 2 check sites; pass receipt/readback. Refs: crates/ui/src/composer/codex_prompts_e2e.rs. |
| R | `crates/ui/examples/command-palette-fixture.rs` | Coordinator override: retained. `docs/screenshots/command-palette/README.md` documents it as the way to reproduce those captures, and no other visual fixture renders the palette; a manual visual fixture is QA tooling, not an assertion-free test. Original lane note: Manual screenshot window (no checks, no artifact; "press Cmd+K" by hand). Palette behavior keepers run in CI: `crates/ui/src/shell.rs` `command_palette_groups_actions_chats_and_content_and_restores_focus`, `command_palette_default_enter_*`, `command_palette_replacement_hides_stale_content_and_keeps_append_rows`, `command_palette_keeps_keyboard_target_stable_across_async_dedup`, `command_palette_rejects_delayed_query_project_and_profile_results` + `shell/command_palette.rs` search/Enter tests. Unlocks: file only (auto-discovered example, no Cargo entry). Last touched by rebrand 68e63c9f; added #399. |
| R | `crates/ui/examples/composer-parity-fixture.rs` | Headless GPUI E2E for composer parity between the main chat and a side chat (a persistent fork) open beside it in a workbench split, plus the fork's ordinary tab header. Drives the production paths it names (compile is the coordinator check); 105 check sites; pass receipt/readback. Refs: crates/ui/examples/side-chat-pane-fixture.rs, docs/design/harness-evolution/slice-2-ui.md +10; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/drafts-fixture.rs` | L08-L11 composer-draft/context QA fixture driven by `scripts/run-macos-l10-fixture.sh`, `run-macos-l11-fixture.sh`, `qa-ui-keyboard.mjs`; control-file protocol + seeded restart state; inline tests in the section above. |
| R | `crates/ui/examples/empty-project-fixture.rs` | Native empty-project screen fixture with isolated synthetic state. Drives the production paths it names (compile is the coordinator check); 15 check sites; pass receipt/readback. Refs: docs/design/ui-polish-audit-2026-10-06/findings/design-system.md, docs/design/ui-polish-audit-2026-10-06/findings/shell.md +2. |
| R | `crates/ui/examples/goal-card-fixture.rs` | Self-contained headless GPUI E2E for Codex goals in the composer. Drives the production paths it names (compile is the coordinator check); 2 check sites; pass receipt/readback. Refs: crates/ui/src/composer/goals_e2e.rs, docs/design/ui-polish-audit-2026-10-06/findings/composer.md +1; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/history-operations-fixture.rs` | Hermetic L13 native-history fixture (`scripts/run-macos-l13-fixture.sh`); synthetic Codex-shaped provider; inline tests in the section above. |
| R | `crates/ui/examples/integrations-picker-fixture.rs` | Self-contained headless GPUI E2E for the installed integration picker. Drives the production paths it names (compile is the coordinator check); 2 check sites; pass receipt/readback. Refs: docs/design/ui-polish-audit-2026-10-06/findings/composer.md, docs/design/ui-polish-audit-2026-10-06/mocks/CP/CP-05-before.svg +1. |
| R | `crates/ui/examples/live-usage-fixture.rs` | CC-06 headless screenshots of the Usage page's "Live sessions" section and the stream-observed Claude limit fallback. Drives the production paths it names (compile is the coordinator check); 4 check sites; pass receipt/readback. Refs: own header only. |
| R | `crates/ui/examples/macos-resource-profile.rs` | UI-only replay of `scripts/resource-profile.mjs` frames.json (feature `resource-profile`); docs/performance-macos.md; measurement tool, not a pass/fail test. |
| R | `crates/ui/examples/mcp-inventory-fixture.rs` | Headless GPUI E2E for CC-11 Claude MCP inventory in the production Settings > Providers "Skill library and MCP" card. Drives the production paths it names (compile is the coordinator check); 4 check sites; pass receipt/readback. Refs: docs/design/ui-polish-audit-2026-10-06/findings/design-system.md. |
| R | `crates/ui/examples/native-agents-fixture.rs` | Headless GPUI E2E for Claude native agents (CC-08 / E25). Drives the production paths it names (compile is the coordinator check); 17 check sites; pass receipt/readback. Refs: docs/design/ui-polish-audit-2026-10-06/findings/surfaces.md, docs/design/ui-polish-audit-2026-10-06/findings/timeline.md; 2026-10-08 audit coverage. |
| D | `crates/ui/examples/new-project-fixture.rs` | Manual screenshot window (no checks/artifact, no doc/script reference) fed by feature-gated production seams `Shell::fixture_project_responses` (shell/spaces.rs) and `Pickers::fixture_model_catalog` (pickers.rs). Keepers (CI `luna-ui --lib`): spaces.rs `devices_locations_folders_and_back_clear_stale_state`, `standalone_opens_local_home_and_paths_require_return`, `local_home_lists_children_and_manual_open_keeps_add_target`, `stale_folder_and_drive_results_cannot_replace_current_device`, `canceled_move_origin_cannot_leak_into_reopened_add_project`. Unlocks: `project-palette-fixture` feature, its `[[example]]` entry and both seams (cross-lane request 1). |
| R | `crates/ui/examples/new-thread-target-fixture.rs` | Headless GPUI E2E for the new thread's target controls in the frame's bottom row. Drives the production paths it names (compile is the coordinator check); 23 check sites; pass receipt/readback. Refs: 2026-10-08 audit coverage. |
| R | `crates/ui/examples/preview-fixture.rs` | CI keeper: ui-tests.yml "Project discovery, stable URLs and HMR in native WebKit" (`result.txt` required); docs/preview-networking.md. |
| D | `crates/ui/examples/project-selector-fixture.rs` | Manual review window (no checks/artifact). Its documented expectation "trigger shows `anara @ Build server`" (#400) is obsolete: the sidebar redesign removed the machine from the trigger. Stronger keeper: `sidebar-polish-fixture` failure cases 7-8 (`anara @ Studio Mac` / `anara @ Build server` only in the dropdown, pass receipt) + `sidebar-redesign-fixture` picker cases. Unlocks: file only (auto-discovered). |
| R | `crates/ui/examples/queue-image-steer-fixture.rs` | Self-contained headless GPUI fixture for CC-04 queued image steering. Drives the production paths it names (compile is the coordinator check); 2 check sites; pass receipt/readback. Refs: docs/design/ui-polish-audit-2026-10-06/findings/composer.md. |
| R | `crates/ui/examples/related-chats-fixture.rs` | Headless GPUI E2E for sidebar drag-reorder and the right pane's related chat lists (Child chats, Side chats) built from the sidebar's task row. Drives the production paths it names (compile is the coordinator check); 44 check sites; pass receipt/readback. Refs: docs/design/persistent-fork-threads/verification.md, docs/design/ui-polish-audit-2026-10-06/findings/shell.md +1; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/session-history-fixture.rs` | L14 session-history/search fixture (`scripts/run-macos-l14-fixture.sh`); inline test in the section above. |
| R | `crates/ui/examples/settings-gallery-fixture.rs` | Settings gallery: every Settings page of the production `Shell`, captured at the verification matrix (Interface size × window width × theme) against an in-process synthetic engine. Drives the production paths it names (compile is the coordinator check); 13 check sites; pass receipt/readback. Refs: crates/ui/src/shell/settings_gallery.rs; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/side-chat-pane-fixture.rs` | Headless GPUI E2E for a persistent fork (side thread) as an ordinary thread in the chat workbench. Drives the production paths it names (compile is the coordinator check); 27 check sites; pass receipt/readback. Refs: crates/ui/examples/related-chats-fixture.rs, docs/design/persistent-fork-threads/verification.md +4; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/sidebar-polish-fixture.rs` | Headless GPUI E2E for three sidebar fixes. Drives the production paths it names (compile is the coordinator check); 26 check sites; pass receipt/readback. Refs: docs/design/sidebar-redesign-2026-10-08/spec.md; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/sidebar-redesign-fixture.rs` | Headless GPUI E2E for the sidebar redesign. Drives the production paths it names (compile is the coordinator check); 58 check sites; pass receipt/readback. Refs: docs/design/sidebar-redesign-2026-10-08/spec.md. |
| R | `crates/ui/examples/sidebar-sections-fixture.rs` | Headless GPUI E2E for the sidebar's list sections. Drives the production paths it names (compile is the coordinator check); 40 check sites; pass receipt/readback. Refs: docs/design/sidebar-redesign-2026-10-08/spec.md; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/startup-fixture.rs` | Startup landing acceptance (13 cases, PNG + JSON readback + `<label>.pass`); runner `scripts/qa-startup.sh` (now covers all 13). |
| R | `crates/ui/examples/task-organization-fixture.rs` | L12 task-organization fixture (`scripts/run-macos-l12-fixture.sh`); 70 check sites, hover capture JSON/PNG; inline tests in the section above. |
| R | `crates/ui/examples/timeline-fixture.rs` | Headless GPUI E2E for the chronological chat timeline. Drives the production paths it names (compile is the coordinator check); 2 check sites; pass receipt/readback. Refs: crates/engine/tests/timeline_order_e2e.rs; 2026-10-08 audit coverage. |
| R | `crates/ui/examples/tool-icons-fixture.rs` | Headless GPUI E2E for the activity chain's tool icons. Drives the production paths it names (compile is the coordinator check); 5 check sites; pass receipt/readback. Refs: 2026-10-08 audit coverage. |
| R | `crates/ui/examples/usage-fixture.rs` | Headless screenshots of the compact Usage card and optional local settings. Drives the production paths it names (compile is the coordinator check); 14 check sites; pass receipt/readback. Refs: crates/engine/tests/usage_refresh.rs, docs/design/ui-polish-audit-2026-10-06/findings/design-system.md +3. |
| R | `crates/ui/examples/windows-render-fixture.rs` | Static Windows renderer target; the pixel checks live in `scripts/test-windows-rendering.ps1` (windows.yml `native-gui`). |
| R | `crates/ui/examples/workbench-performance-fixture.rs` | Native workbench performance baseline (harness evolution slice 6). Drives the production paths it names (compile is the coordinator check); 5 check sites; pass receipt/readback. Refs: docs/design/harness-evolution/slice-6-ui.md, scripts/qa-workbench-performance.mjs; 2026-10-08 audit coverage. |
| R | `scripts/e2e-smoke.sh` | Only two-device proof over a real edge (wrangler dev) + two headless engines + `luna-rpc` e2e_driver (projects-first, verified); ARCHITECTURE.md/docs/chat2-sync.md. Logs kept only with LUNA_E2E_KEEP_LOGS=1 (follow-up). |
| F | `scripts/prepare-github-pr-qa.mjs` | Pending-receipt poll called `ReadTaskCommandReceipt`, an RPC that never existed (only this script, b6dad9fd); now `GetTaskCommandReceipt {commandId}` like the other drivers. Native PR preflight: fake gh calls, no POST, checkout identity; writes native-preflight.json. |
| R | `scripts/prepare-updates-ui-e2e.sh` | Prepares the human-driven Updates-screen release fixture; docs/qa/update-e2e; never touches apps. Refs: docs/qa/update-e2e/2026-09-28.md. |
| R | `scripts/qa-agent-delegation-ux.mjs` | Repeatable, real-provider acceptance. Requires the isolated signed QA app. Every RPC it calls exists in crates/rpc (scanned); 36 failure sites; pass receipt/readback. Refs: docs/design/agent-delegation-ux/verification.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-agent-workspace-controls.mjs` | Real-provider acceptance in this feature's isolated signed desktop profile. Every RPC it calls exists in crates/rpc (scanned); 46 failure sites; pass receipt/readback. Refs: docs/qa/agent-workspace-controls-2026-10-05.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-automations-live.mjs` | Disposable signed app -> scheduler worker -> ordinary native Codex/Claude run. Every RPC it calls exists in crates/rpc (scanned); 42 failure sites; pass receipt/readback. Refs: docs/design/harness-evolution/slice-5-verification.md, docs/design/harness-evolution/slice-5.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-backend-audit.sh` | Aggregate runner (fmt, compile, doc/providers/engine targets); all named targets exist; results.tsv + result.txt. Refs: docs/qa/backend-audit-2026-09-26.md. |
| R | `scripts/qa-chat-mermaid.sh` | Runs chat-mermaid-fixture `fixture` + `reopen` (both write `.pass`, verified); SHA256SUMS. Refs: docs/chat-mermaid.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-chat-workbench.mjs` | Native acceptance driver: only this task's isolated signed QA app/profile. Every RPC it calls exists in crates/rpc (scanned); 54 failure sites; pass receipt/readback. Refs: docs/design/chat-workbench/verification.md, docs/design/persistent-fork-threads/verification.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-child-chats.mjs` | Actual isolated engine E2E rejecting manual child creation, then the Codex adapter's per-process `luna` MCP endpoint driven by a deterministic Codex peer (scripts/fixtures/luna-mcp-codex.py) acting as Codex's MCP client. Every RPC it calls exists in crates/rpc (scanned); 17 failure sites; pass receipt/readback. Refs: docs/qa/child-chats.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-child-models-native.mjs` | Drives only this task's isolated, signed QA app. Every RPC it calls exists in crates/rpc (scanned); 20 failure sites; pass receipt/readback. Refs: docs/reference/chat-placement-and-child-models.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-claude-writer-guard.py` | No-model live E2E of Claude session listing and the LUNA writer guard via crates/harness/examples/claude_writer_guard_probe.rs; evidence JSON in docs/audits/2026-10-08-claude-review/evidence; cited by claude-code-advanced-certification.md. |
| R | `scripts/qa-contained-ui-alignment.mjs` | Native UI acceptance readback, restricted to this disposable QA identity. Every RPC it calls exists in crates/rpc (scanned); 44 failure sites; pass receipt/readback. Refs: docs/design/contained-ui-alignment/verification.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-context-transfer-live.mjs` | Real signed app -> owning engine -> native provider. Every RPC it calls exists in crates/rpc (scanned); 65 failure sites; pass receipt/readback. Refs: docs/design/harness-evolution/slice-2-verification.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-execution-summary-live.mjs` | Real signed app -> owning engine -> native provider. Every RPC it calls exists in crates/rpc (scanned); 46 failure sites; pass receipt/readback. Refs: docs/design/harness-evolution/verification.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-goals-live.mjs` | Coordinator-only live E2E. Uses the existing signed app and a fresh ordinary fixture repository. Every RPC it calls exists in crates/rpc (scanned); 30 failure sites; pass receipt/readback. Refs: 2026-10-08 audit coverage. |
| R | `scripts/qa-history-audit.sh` | Aggregate usage/history runner; `usage::` filters and the two `--exact --ignored` live readbacks verified; results.tsv. Refs: docs/qa/history-audit-2026-09-26.md. |
| R | `scripts/qa-integrations-picker.mjs` | Read-only actual engine/adapter discovery E2E. Every RPC it calls exists in crates/rpc (scanned); 15 failure sites; logs/captures. Refs: docs/qa/integrations-picker/README.md. |
| R | `scripts/qa-mcp-prompts.sh` | MCP elicitation runner; 3 exact test names + codex `computer_use` verified; LUNA_MCP_E2E_ARTIFACT_DIR consumed by tests. Refs: docs/reference/mcp-elicitation.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-native-inputs.sh` | Runs harness + engine `native_inputs`; LUNA_NATIVE_INPUTS_EVIDENCE consumed; exit codes + SHA256SUMS. Refs: docs/reference/native-composer-inputs.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-native-review.mjs` | Coordinator-only: built LUNA engine -> adapter -> deterministic native peer -> durable document. Every RPC it calls exists in crates/rpc (scanned); 20 failure sites; pass receipt/readback. Refs: docs/qa/native-review.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-opencode-diff.mjs` | Normalized-artifact diff for PD-14 parity runs (validation-plan §1 item 8). Every RPC it calls exists in crates/rpc (scanned); 17 failure sites; pass receipt/readback. Refs: docs/reference/qa-infrastructure.md, scripts/qa-opencode-lib.mjs +1; 2026-10-08 audit coverage. |
| R | `scripts/qa-opencode-driver.mjs` | The UI's actions over one engine connection, for the OpenCode in-app QA drivers. Every RPC it calls exists in crates/rpc (scanned); 15 failure sites; pass receipt/readback. Refs: docs/reference/qa-infrastructure.md, scripts/qa-opencode-live.mjs +1; 2026-10-08 audit coverage. |
| R | `scripts/qa-opencode-env.sh` | Isolated OpenCode 2 QA profile + wrapper (no RPC); prerequisite of every OpenCode tier (qa-opencode-t2.py, run-macos-dev.sh, qa-infrastructure.md "OpenCode QA"); refuses OpenCode v1 (PD-15). |
| R | `scripts/qa-opencode-fake-engine.mjs` | Rehearsal-only fake engine (tier R0) so `qa-opencode-scenarios.mjs rehearse`/`rehearse-faults` exercise runner plumbing offline; proves nothing about LUNA by design (header). |
| R | `scripts/qa-opencode-keepalive-proxy.py` | Fault wrapper reproducing the S1 lost-answer bug at the harness boundary (qa-infrastructure.md); a tool, not a pass/fail test; keeps its own request log. |
| R | `scripts/qa-opencode-lib.mjs` | Shared plumbing for the OpenCode in-app QA drivers (qa-opencode-live.mjs, qa-opencode-scenarios.mjs, qa-opencode-diff.mjs). Every RPC it calls exists in crates/rpc (scanned); 22 failure sites; pass receipt/readback. Refs: docs/reference/qa-infrastructure.md, scripts/qa-opencode-driver.mjs +3; 2026-10-08 audit coverage. |
| R | `scripts/qa-opencode-live.mjs` | OpenCode QA live driver (OC-01): signed disposable LUNA QA app -> real OpenCode (T2 mock provider or T3 real model) -> evidence per docs/reference/qa-infrastructure.md "OpenCode QA" and validation-plan §2. Every RPC it calls exists in crates/rpc (scanned); 14 failure sites; pass receipt/readback. Refs: docs/reference/qa-infrastructure.md, scripts/qa-opencode-lib.mjs; 2026-10-08 audit coverage. |
| R | `scripts/qa-opencode-manifest.py` | Evidence manifest/hash helpers shared by qa-opencode-env.sh, -lib, -live, -scenarios, -t2 (OC-01); fails on identity/hash mismatch. |
| R | `scripts/qa-opencode-mock-provider.py` | Scripted OpenAI-compatible provider for the T2 tier (stdlib only); request log is the artifact; used by qa-opencode-t2.py and qa-infrastructure.md. |
| R | `scripts/qa-opencode-scenarios.mjs` | OpenCode in-app scenario runner: S1..S8 through the signed LUNA QA app's RPC surface (the RPCs the UI uses), with a provider argument so the same steps run against Claude Code for the PD-14 parity diff. Every RPC it calls exists in crates/rpc (scanned); 62 failure sites; pass receipt/readback. Refs: docs/reference/qa-infrastructure.md, scripts/qa-opencode-diff.mjs +3; 2026-10-08 audit coverage. |
| R | `scripts/qa-opencode-t2.py` | OpenCode T2 rig: real `opencode` v2 binary + mock provider, isolated (OC-01). Every RPC it calls exists in crates/rpc (scanned); 5 failure sites; pass receipt/readback. Refs: docs/reference/qa-infrastructure.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-opencode-usage-reconcile.py` | Reconciles LUNA OpenCode usage history with `opencode stats` (OCE-13); companion of crates/engine/examples/usage_opencode_readback.rs; exits non-zero on mismatch. |
| R | `scripts/qa-opencode-usage.mjs` | QA-batch usage readback (GetUsageSnapshot + `opencode stats`); identity checks delegated to qa-opencode-lib.mjs (QF-09); writes go-usage-<label>.json. |
| R | `scripts/qa-permission-detail.sh` | Approval-detail runner (also step of qa-thread-approvals.sh); exact names verified; LUNA_PERMISSION_RECEIPT read by fake peer. Refs: scripts/qa-thread-approvals.sh; 2026-10-08 audit coverage. |
| R | `scripts/qa-project-defaults-live.mjs` | Repeatable native-app setup/readback for two exact isolated QA identities. Every RPC it calls exists in crates/rpc (scanned); 42 failure sites; pass receipt/readback. Refs: docs/qa/project-defaults-2026-09-26.md, docs/qa/project-first-submit-recovery-e2e.md +1. |
| R | `scripts/qa-project-defaults.sh` | Project-defaults runner; `project_defaults_tests` and `project_defaults::engine_epoch_tests` (8 tests) verified; evidence env consumed. Refs: docs/qa/project-defaults-2026-09-26.md, docs/qa/project-defaults-cases.md +2. |
| R | `scripts/qa-project-first-submit-recovery.mjs` | Read-only half of the native first-submit E2E. Every RPC it calls exists in crates/rpc (scanned); 19 failure sites; logs/captures. Refs: docs/qa/project-first-submit-recovery-e2e.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-project-only.mjs` | Headless engine: projectless create refused ("project is required"), project sessions work (666d8cf0 contract); only header documents it. Refs: own header only. |
| R | `scripts/qa-project-remove-restore.mjs` | Newest driver (26f4b096): remove project keeps sessions, re-add restores; failure modes written first; only header documents it. Refs: own header only. |
| R | `scripts/qa-provider-audit.sh` | Aggregate provider/account runner; targets `provider_lifecycle_audit`, `usage_account_selection`, `agent_accounts::` verified; stops on first failure. Refs: docs/qa/provider-audit-2026-09-26.md. |
| F | `scripts/qa-related-chats-live.mjs` | Real-provider native fork context/identity across restart (keeper for the retired qa-sidebar.py). Retired scenario "only explicit End closes the live side chat" (`UnloadSideChat` -> `ended`), contradicted since 29da9bdd; keeper `side_chats.rs::legacy_view_close_does_not_write_lifecycle_or_stop_a_turn`. |
| F | `scripts/qa-related-chats.mjs` | Headless `luna` process + Mock: idempotent create, conflict, 6 leaseless forks, isolated send without replay, Stop, no heartbeat, restart, 53-row pagination. Repaired: published fork rows now carry the parent project (`spaceId === space`; side_chats.rs asserts `row["spaceId"] == "space-side"`) instead of `!spaceId`. Retired scenarios "explicit End" and "ended state survives restart": End/unload lifecycle removed by 29da9bdd; keeper `side_chats.rs::legacy_view_close_does_not_write_lifecycle_or_stop_a_turn`. |
| R | `scripts/qa-same-chat-rewind.mjs` | Coordinator-owned real-provider E2E. Creates an ordinary disposable repo. Every RPC it calls exists in crates/rpc (scanned); 42 failure sites; pass receipt/readback. Refs: docs/qa/same-chat-rewind.md; 2026-10-08 audit coverage. |
| D | `scripts/qa-sidebar-agents.py` | Imports qa-sidebar.py; its `prepare` step also creates a projectless parent (rejected since 666d8cf0). Native subagent wire contract keeper: `crates/engine/tests/codex_subagents.rs` (CI ui-tests.yml); OpenCode/Claude native tasks: `qa-opencode-scenarios.mjs`. Unlocks: whole file. |
| D | `scripts/qa-sidebar-protocol.py` | Loopback self-test of qa-sidebar.py's Rpc/Transcript classes only; no product path; its subject is deleted. Unlocks: whole file. |
| D | `scripts/qa-sidebar.py` | Entry point gone: creates a projectless parent (`Mutate createChat` without `spaceId`), rejected since 666d8cf0 ("A project is required to create a session"); also asserts side-chat `UnloadSideChat` -> `state == "unloaded"`, removed by 29da9bdd (view close no longer mutates lifecycle). Keepers: `scripts/qa-related-chats-live.mjs` (real-provider native fork context, parent unchanged, restart) + `crates/engine/tests/side_chats.rs` (`persistent_forks_*`, `legacy_view_close_does_not_write_lifecycle_or_stop_a_turn`). Unlocks: whole file. |
| F | `scripts/qa-startup.sh` | Runner omitted 4 cases the fixture accepts (`archived-parent-child`, `active-parent-child` from a6084a58; `no-project`, `ready-empty` from dd80eb3d), so folded-child boot and SU-02 splash regressions were never repeatably run. Case list extended; first-launch + restart per case; SHA256SUMS + result.json. |
| R | `scripts/qa-steering-status.sh` | Steering/terminal-status runner; codex `steer`, two exact interruption tests, message_queue `steer` verified; adapter-events evidence. Refs: 2026-10-08 audit coverage. |
| R | `scripts/qa-task-confirmations.mjs` | Native E2E fixture for the task confirmation flow. Every RPC it calls exists in crates/rpc (scanned); 17 failure sites; pass receipt/readback. Refs: docs/qa/task-confirmations-2026-09-29.md. |
| R | `scripts/qa-task-work-live.mjs` | Signed disposable app -> real Codex/Claude -> truthful shared work and exact native stop. Every RPC it calls exists in crates/rpc (scanned); 29 failure sites; pass receipt/readback. Refs: docs/design/harness-evolution/slice-3-verification.md, docs/reference/qa-infrastructure.md; 2026-10-08 audit coverage. |
| F | `scripts/qa-thread-approvals.sh` | Step `side-ceilings` filtered `--test side_chats side_chat_permission_ceiling`, renamed by 29da9bdd to `fork_permissions_follow_explicit_own_configuration` -> the step ran 0 tests and exited 0. Repointed (step `fork-permissions`). All other targets/filters verified to match tests. |
| R | `scripts/qa-ui-keyboard.mjs` | Native evidence collector for UI keyboard activation, Escape and focus-ring QA (docs/qa/ui-keyboard-activation-cases.md). Every RPC it calls exists in crates/rpc (scanned); 32 failure sites; pass receipt/readback. Refs: docs/qa/ui-audit-2026-10-03.md, docs/qa/ui-keyboard-activation-cases.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-usage.mjs` | Read-only end-to-end check against a running LUNA engine. Every RPC it calls exists in crates/rpc (scanned); 1 failure sites; pass receipt/readback. Refs: docs/qa/backend-audit-2026-09-26.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-workbench-followup.mjs` | Guarded native acceptance for the isolated follow-up QA app. Every RPC it calls exists in crates/rpc (scanned); 49 failure sites; pass receipt/readback. Refs: docs/design/workbench-followup-2026-10-04/verification.md; 2026-10-08 audit coverage. |
| R | `scripts/qa-workbench-performance.mjs` | Real native self-driving fixture with external process CPU/RSS/footprint. Every RPC it calls exists in crates/rpc (scanned); 17 failure sites; pass receipt/readback. Refs: 2026-10-08 audit coverage. |
| R | `scripts/qa-workspace-data.sh` | Aggregate workspace-data runner; 9 engine targets + `git_checkpoints::`/`uploads::`/`git_actions::` verified; result.txt. Refs: docs/qa/workspace-data-audit-2026-09-26.md. |
| R | `scripts/qa-workspace-native.mjs` | Real-app IPC acceptance. Creates only disposable Git roots and QA app records. Every RPC it calls exists in crates/rpc (scanned); 51 failure sites; pass receipt/readback. Refs: docs/qa/workspace-data-audit-2026-09-26.md; 2026-10-08 audit coverage. |
| R | `scripts/run-macos-browser-fixture.sh` | Bundles the fixture with the release Info.plist (ATS policy) for CI ui-tests.yml; the fixture writes `result.txt`, which CI requires. |
| R | `scripts/test-linux-browser.sh` | CI ui-tests.yml `linux-browser` (default + OpenGL): native mouse input on X11/Wayland; captures uploaded. |
| R | `scripts/test-windows-capture-client.ps1` | Helper process for test-windows-rendering.ps1 (PrintWindow isolation); 4 throw sites. |
| R | `scripts/test-windows-lifecycle.ps1` | windows.yml `native-gui`: close real window, clean exit, reopen same profile; result.txt uploaded. |
| R | `scripts/test-windows-rendering.ps1` | windows.yml `native-gui`: pixel regression of windows-render-fixture; capture.png + measurements.json uploaded. |
| R | `scripts/test-windows-startup.ps1` | windows.yml "GUI subsystem and redirected CLI output": no console allocation, redirected help/errors; 4 throw sites. |
| R | `scripts/verify-build-cache.sh` | Cache-integration E2E named by AGENTS.md; missing-mbx and real-crate paths; retains logs/JSON. Refs: AGENTS.md, docs/reference/build-cache.md; 2026-10-08 audit coverage. |
| R | `scripts/verify-github-pr-ui-lifetime.sh` | Runs `luna-ui --lib pr_ui_lifetime_` (2 tests); LUNA_EPOCH_QA_ARTIFACTS consumed; exit preserved. Refs: docs/qa/github-pr-ui-lifetime.md. |
| R | `scripts/verify-github-pr.sh` | Runs `github_draft_pr` then `backup_restore github_pr_attempt_journal` (verified); LUNA_PR_ARTIFACT_DIR consumed. Refs: docs/qa/github-draft-pr.md, docs/qa/github-pr-ui-lifetime.md. |
| R | `scripts/verify-managed-skills.sh` | Runs `skill_library_e2e`; writes result.txt with command/exit/log (docs/reference/managed-skills-qa.md). Refs: docs/reference/managed-skills-qa.md. |
| R | `scripts/verify-project-commands.sh` | Runs `project_command_store` + 3 engine targets (verified); LUNA_COMMAND_QA_ARTIFACTS consumed. Refs: docs/qa/project-commands-2026-09-26.md, docs/qa/project-commands-failure-cases.md +1. |
| R | `scripts/verify-qa-infrastructure.sh` | Subprocess acceptance of the QA identity/build-budget wrappers (docs/reference/qa-infrastructure.md); disposable worktrees; logs retained. Refs: docs/reference/qa-infrastructure.md; 2026-10-08 audit coverage. |
| R | `scripts/verify-updates-e2e.sh` | Signed local release fixture -> rejected updates -> QA install/relaunch (crates/update live); evidence + SHA256SUMS. Refs: docs/qa/update-e2e/2026-09-28.md, docs/qa/update-e2e/README.md. |

## Layer plan

Keeper per contract:

- Cross-language wire codecs: `ChatFramesTests` (chat2 frames; peers
  `crates/sync/src/chat_frames.rs`, `edge/src/chat-frames.test.ts`), `RegistryCoreTests`
  (registry merge vectors; peers `edge/src/registry-core.test.ts`, `crates/doc` registry),
  `NetworkReliabilityTests` wire rows (`pending://`, echo kind, RunRequest shape), and now
  `ChangeRequestTrackingTests::testContractDecodesEveryState` (host JSON, peer
  `crates/proto` change-request status test). Each language keeps its own table.
- Phone-only behavior on real Loro docs / UIKit: `MessageQueueTests`,
  `DeviceRelayClientTests`, `GeneratedImageTests`, `AppshotTests`, `RegistryDocTests`,
  `TranscriptFollowTests`, `TranscriptPresentationTests`, `ComposerEditorTests`.
- Transcript layout: `TranscriptLayoutTests` (CI, real SwiftUI/UIKit) is the keeper;
  `MobilePolishTests` is the app-level acceptance with screenshots (manual run). The overlap
  (warm/late 600-turn tail) is deliberate: different boundary, only one is CI-routed.
- Live iOS stack: `E2ERunner.swift` (manual rig, log artifact).
- Side chats / persistent forks: deterministic keeper `crates/engine/tests/side_chats.rs`
  (other lane); process-boundary Mock keeper `scripts/qa-related-chats.mjs`; real-provider
  keeper `scripts/qa-related-chats-live.mjs`. Retired layer: `qa-sidebar.py`,
  `qa-sidebar-agents.py`, `qa-sidebar-protocol.py` (projectless parent + unload lifecycle,
  both gone) and the End/unload scenarios inside the two related-chats drivers.
- Native subagents: `crates/engine/tests/codex_subagents.rs` (CI) and
  `qa-opencode-scenarios.mjs`; the retired `qa-sidebar-agents.py` had no other unique
  contract.
- Command palette / project picker / Add project flow: GPUI tests in
  `crates/ui/src/shell.rs` (`command_palette_*`), `crates/ui/src/shell/command_palette.rs`,
  `crates/ui/src/shell/spaces.rs` (`devices_locations_folders_and_back_clear_stale_state`
  and 4 siblings), and the headless `sidebar-polish-fixture` (machine tag cases 7-8) and
  `sidebar-redesign-fixture`. Retired layer: the three manual screenshot windows
  `command-palette-fixture`, `project-selector-fixture`, `new-project-fixture` (no checks,
  no artifact).
- Startup landing: `startup-fixture` driven by `scripts/qa-startup.sh` (now all 13 cases).
- Fixture hermeticity (in-memory credentials, root-scoped homes): the per-fixture inline
  `fixture_*auth*` tests (different config builder per fixture, so no consolidation).
- QA aggregate runners (`qa-*-audit.sh`, `qa-thread-approvals.sh`, `qa-steering-status.sh`,
  `qa-mcp-prompts.sh`, `qa-permission-detail.sh`, `qa-project-defaults.sh`,
  `qa-native-inputs.sh`, `verify-*.sh`) own no assertions; they are evidence bundles over the
  real suites. Their only failure mode found was a stale name filter (fixed).

Assertions carried into keepers: none needed; every deleted assertion either could not fail
(tautology, vacuous negatives, literal copies) or is already asserted by the named keeper.

Production seams unlocked (all outside this lane's files; see request 1): the
`project-palette-fixture` Cargo feature, `Shell::fixture_project_responses`
(`crates/ui/src/shell/spaces.rs`) and `Pickers::fixture_model_catalog`
(`crates/ui/src/pickers.rs`). No seam inside this lane's files.

## Cross-lane requests

1. Required for the build (manifest error otherwise: the explicit `[[example]]` points at
   the deleted file), owner of `crates/ui`:
   - `crates/ui/Cargo.toml`: delete line 9 `project-palette-fixture = []` (and the blank
     line after it) and the block at lines 232-234:
     `[[example]]` / `name = "new-project-fixture"` / `required-features = ["project-palette-fixture"]`.
   - `crates/ui/src/shell/spaces.rs` lines 4948-4997: delete the doc comment
     `/// Synthetic responses for the isolated native screenshot fixture only.` and the
     `#[cfg(feature = "project-palette-fixture")] impl Shell { pub fn fixture_project_responses .. }`
     block (otherwise `unexpected_cfgs` fires once the feature is gone).
   - `crates/ui/src/pickers.rs` lines 10870-10894: delete
     `/// Catalog for the isolated native screenshot fixture; never used by the app.` and the
     `#[cfg(feature = "project-palette-fixture")] impl Pickers { pub(crate) fn fixture_model_catalog .. }` block.
     `rg project-palette-fixture|fixture_project_responses|fixture_model_catalog` must then be empty.
2. Test names QA scripts and workflows select by filter. A filter that matches nothing exits 0,
   so any lane renaming or deleting these must update the named script in the same change:
   - `luna-harness --test codex`: `generic_mcp_elicitation_roundtrips_typed_fields_and_actions`,
     `computer_use*` (qa-mcp-prompts.sh); `steer*`,
     `native_terminal_interruption_is_truthful_without_local_stop`,
     `native_terminal_status_wins_the_local_interrupt_race` (qa-steering-status.sh);
     `approval*`, `provider_permission_event` (qa-permission-detail.sh);
     `live_real_app_server_*` (qa-backend-audit.sh --live-codex).
   - `luna-engine --test e2e`: `mcp_form_url_validation_concurrency_and_secret_boundaries_e2e`
     (qa-mcp-prompts.sh); `permission*` (qa-thread-approvals.sh);
     `structured_approval_preserves_details_and_rejects_unadvertised_choice`,
     `typed_live_approval` (qa-permission-detail.sh).
   - `luna-engine`: `remote_auth::mcp_authorization_urls_are_owner_only_even_with_remote_read_scopes`
     (qa-mcp-prompts.sh); `backup_restore::github_pr_attempt_journal` (verify-github-pr.sh);
     `message_queue::steer*` (qa-steering-status.sh);
     `side_chats::fork_permissions_follow_explicit_own_configuration` (qa-thread-approvals.sh).
   - `luna-engine --lib`: `usage::`, `usage::tests::selected_provider_history_readback` and
     `usage::tests::current_litellm_price_readback` (both `--exact`, qa-history-audit.sh);
     `git_checkpoints::`, `uploads::`, `git_actions::` (qa-workspace-data.sh);
     `agent_accounts::` (qa-provider-audit.sh); `permissions::tests` (qa-thread-approvals.sh);
     `approval` (qa-permission-detail.sh). `luna-proto --lib usage::` (qa-history-audit.sh).
   - `luna-ui --lib`: `permission_` (qa-thread-approvals.sh), `pr_ui_lifetime_`
     (verify-github-pr-ui-lifetime.sh), `project_defaults_tests` and
     `project_defaults::engine_epoch_tests` (qa-project-defaults.sh);
     `native_diff_font_geometry`, `notify::tests::` (ui-tests.yml, windows.yml).
   - Whole integration targets named by scripts (deleting or renaming one breaks them):
     harness `codex claude opencode native_inputs permission_modes claude_permissions
     permission_detail provider_lifecycle_audit`; engine `e2e usage_history
     model_cache_lifecycle message_queue drafts task_commands restart_resume task_identity
     session_history backup_control backup_restore local_profiles session_publication
     provider_scope_e2e remote_doc_authority workspace_data_safety workspace_files
     m5_repos_diffs_terminals m5c_accounts_uploads_titles l21_terminals previews
     queued_attachments github_draft_pr skill_library_e2e native_inputs claude_permissions
     claude_child_chats usage_account_selection account_lifecycle_acceptance
     self_continued_quiesce turn_quiesce project_defaults project_commands worktree_on_run`;
     sync `project_command_store`.
   - Evidence env vars read by those tests (keep the readers): `LUNA_NATIVE_INPUTS_EVIDENCE`,
     `LUNA_PERMISSION_MODES_RECEIPT`, `LUNA_PROJECT_DEFAULTS_EVIDENCE`,
     `LUNA_EPOCH_QA_ARTIFACTS`, `LUNA_COMMAND_QA_ARTIFACTS`, `LUNA_PR_ARTIFACT_DIR`,
     `LUNA_STEERING_EVIDENCE_DIR`, `LUNA_MCP_E2E_ARTIFACT_DIR`, `LUNA_PERMISSION_RECEIPT`.
3. Docs (historical records now pointing at deleted tooling; annotate, optional):
   `docs/screenshots/command-palette/README.md`, `docs/screenshots/project-selector/README.md`
   ("Reproduce with ..."), `docs/qa/sidebar-side-chats-2026-09-26.md`,
   `docs/qa/workspace-identity-repair-contract-2026-09-26.md`, and the `sidebar` identity row
   (port 59787) in `docs/reference/qa-infrastructure.md`.
4. `crates/engine/examples/seed_sidebar_qa.rs` (engine examples owner): companion seeder of
   the retired sidebar drivers; nothing else uses the `target/luna-qa-data/sidebar` profile.
   Retire with the drivers or keep deliberately.
5. iOS production file `apps/ios/Luna/Sync/DeviceRelayClient.swift`: `unaryCount` /
   `streamCount` exist only for `DeviceRelayClientTests` (retained); no action unless the
   owner wants a non-accessor assertion.

CI workflow routing changes needed: none. No deleted or moved file is named by a workflow;
every target and filter the workflows name still resolves.

## Possible product defects

None confirmed. Every failing-today candidate found in this lane encoded a superseded design
or a nonexistent API, not a product bug, and was repaired in the tooling:

- `scripts/qa-related-chats.mjs` / `qa-related-chats-live.mjs` asserted the 2026-10-03
  End/unload lifecycle and project-less fork rows; the 2026-10-04 persistent-fork design
  (`docs/design/persistent-fork-threads/README.md`) and `side_chats.rs` say otherwise.
- `scripts/prepare-github-pr-qa.mjs` called `ReadTaskCommandReceipt`, which never existed.
- `scripts/qa-sidebar*.py` create projectless chats, refused by design since 666d8cf0.

Worth a look by the engine owner: `UnloadSideChat` is still exported (`crates/rpc/src/lib.rs`
`UNLOAD_SIDE_CHAT`) but is now a no-op view close that returns `state: "active"`; a client
still expecting End semantics gets no error.

## Follow-ups

- The 21 remaining inline fixture tests run nowhere: CI uses `luna-ui --lib`, and examples
  default to `test = false`. Route them, for example
  `cargo test --locked -p luna-ui --features appshots-fixture --example drafts-fixture --example history-operations-fixture --example task-organization-fixture --example session-history-fixture --example activity-attention-fixture --example child-activity-fixture`,
  or move the hermeticity checks into each fixture's startup.
- About 50 `appshots-fixture` examples are never compiled by CI (only browser, preview and
  windows-render fixtures are). Add `cargo check --locked -p luna-ui --features appshots-fixture --examples`
  to ui-tests.yml, or accept bit-rot risk.
- `LunaUITests` (14 tests) are not in CI (`-only-testing:LunaTests`).
- `E2ERunner.swift` step 4 (fragmented big-doc backfill) depends on a seeder that was never
  committed; add it or drop the step. The rig only logs; nothing reads `e2e.log` in-repo.
- `scripts/e2e-smoke.sh` deletes its logs unless `LUNA_E2E_KEEP_LOGS=1`; keep a retained
  result file per the repo's artifact rule.
- `child-activity-fixture`, `chrome-shortcuts-fixture`, `claude-coordination-fixture`,
  `claude-history-fixture`, `live-usage-fixture`, `qa-project-only.mjs`,
  `qa-project-remove-restore.mjs` and `qa-startup.sh` are documented only in their own
  headers; add them to a QA guide.
- The Swift Codex fallback catalog has no parity check against a host `ListModels` answer.
