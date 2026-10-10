# Frontend status

- Owner: Claude (Claude Desktop, High effort). Acknowledged 2026-10-10.
- Ownership: `crates/ui/**` and this file only. No edits to proto, engine,
  sync/doc/RPC, scripts or contract/backend docs. No Cargo, app launch/quit,
  commit, merge or push; Codex owns those slots.

## Final status (2026-10-10) — verified

- Writes: CLOSED. No further frontend edits; nothing of mine is building,
  running, or depending on any build cache, app bundle or QA data directory.
- Verified by Codex, see [verification.md](verification.md): rendered
  chat-workbench run 6 passed in full (all automation stages 62-84, pane
  geometry and themes); engine automation, adjacent, storage and UI tests,
  Clippy and format green; native QA of the signed app (all six
  real-provider schedules, restart without replay, Resolve, Delete → Show
  deleted → History, pane layout after the fix, Unicode rule refusal, paused
  rename, Never + archive persistence).
- Frontend scope delivered: project-scoped Automations page and right-pane
  surface (following the focused tab); editor with name, prompt,
  project/chat/fresh-chat destination, provider/model/reasoning (catalog
  gated), schedule kinds including custom rules, notifications and archiving,
  strict date/time parsing; pause, edit without resuming, resume, delete,
  history, resolve-unknown; Finished state; inline automation cards and
  "Sent by scheduled task" attribution; notification routing and Activity
  automation alerts.

## Historical notes (superseded by the final status above)

The entries below are the edit-window log kept for traceability. Their
"State", "Writes" and "not compiled" lines describe the moment they were
written, not the current state.

- Sixth edit window (2026-10-10): B5 UI side. Not compiled;
  rustfmt- and `git diff --check`-clean on every file I touched.
  - Routing (`shell/activity_center.rs::route_transition`): tests extended
    first (`automation_outcomes_follow_their_saved_notification_choice`, with
    its failure cases). A `suppress_notification` item gets no sound or
    banner but stays in Activity. `AutomationCompleted` is a completion
    (Done sound, dropped by a chat's Muted/Important only) that may banner,
    since it only arrives unsuppressed for All runs; desktop and background
    only settings still apply. Failures stay Attention alerts.
  - Banners name the automation's project; an `AttentionTarget::Automation`
    click (`l15a:{env}:{project}:{automation}`, parser test extended in
    `lib.rs`) opens that project's Automations page with the automation's
    history, even when no chat was created.
  - Activity view: an "Automations" section (the Board section's row
    pattern) lists automation alerts the chat rows would drop (no chat, an
    archived fresh chat, or another device's chat), labelled
    "{project} · {name}", opening the automation; unresolved ones light the
    Activity dot, muted or not.
  - `suppress_notification: false` added to the two AttentionItem literals.
  - B4 (seventh window): an existing-chat destination keeps that chat's
    provider and account. `chat_provider_mismatch` refuses a different
    provider (or a different known account) locally, before Review, naming
    the chat's provider and the two ways forward (its own model, or a new
    chat each run); the Model row shows the same text live, else "Runs
    continue {chat} on {provider}: choose its model and effort here". No
    silent provider switch or target change. Native QA: the fixture offers
    only Mock, so a cross-provider pick is a native check.
  - Run-2 fixes (eighth window). Product race, not fixture: Review could
    build a new automation's request before the provider's catalog landed
    (reasoning unresolved → "provider default"), and the request changed
    under the review when it did. `Pickers::catalog_settled` (production
    query) now gates it: the editor won't build an execution from unsettled
    defaults, Review is disabled with "Loading this provider's models…"
    meanwhile; saved untouched executions don't wait. Case 65 waits for
    Review, then asserts the reviewed request and the host's description
    carry Medium and stay current after a settle (fails on the pre-fix
    code, as run 2 did). Fixture: the automation chat's UI rows now mirror
    its title as well as its config (run 2's note showed the alongside
    chat's old title, which case 80's destination check would have hit).
    BYDAY: `get` instead of `split_at` (your pre-fix repro panics); case 81
    paints `FREQ=WEEKLY;BYDAY=é1` and reads it back unparsed.
  - Ninth window (fixture only): case 64's "a bounded PR watch builds" now
    waits for the request to build (catalog settled). Every other build or
    review read in w20m/w20n was re-checked: refusal checks hit schedule or
    destination errors first, untouched existing executions skip the gate,
    and the remaining Reviews already follow a loaded catalog.
  - Tenth window (production, bounded): `AutomationsPanel::execution_picked`
    compares only harness, account, model, reasoning and model options, used
    by `editor_execution` and the legacy note. A checkout/branch/access the
    pickers take from new-task defaults (`pickers.rs:1239`; for a detached
    picker, the composer's sticky last-used defaults) no longer counts as a
    pick, so an untouched legacy record still saves `None` and an untouched
    pinned one its saved execution. No fixture extension: this fixture has no
    existing interaction that sets a New-worktree sticky default, and adding
    one would be a new seam. Native QA: pick New worktree in a composer, then
    rename an existing automation and confirm Review shows its saved model.
  - Eleventh window (fixture only): AUTO_CHAT was the seeded alongside
    chat's id, so createChat kept its synthetic `checkout-b` folder and the
    new dispatch fence rightly skipped the run (run 4). AUTO_CHAT is now its
    own `e5e5e5e5-…0001` chat in the project root; the alias workarounds
    (explicit config set, row config/title mirroring) are gone and ALONG is
    untouched (it now stays in the UI list after w20m's cleanup). The
    completion wait stops at once on Failed/Skipped/Unknown/Cancelled and
    saves `w20m-terminal-run.json`.
  - Twelfth window (production + fixture): the right pane's scroll column
    (`render_pane`, height-bounded `flex_col` + `overflow_y_scroll`) let its
    children keep `flex-shrink: 1`, so a tall editor and the list card were
    squeezed until rows overlapped (native-pane-overlap-before.png); the
    page column isn't height-bounded, hence fine. Every pane child is now
    `flex_none`. Fixture first: the panel readback gains `scroll`
    (viewport height, max offset, the pane editor's laid-out height, from
    the scroll handle it already tracks); case 65 asserts at 1480x920 that
    the pane scrolls and the editor keeps ≥ 600 px, and captures
    `w20m-automation-editor-pane.png`. On the pre-fix layout the column
    fits by squeezing, so max offset is 0 and the check fails.
  - Thirteenth window (fixture diagnostics only). Run 5's Unknown is a
    backend race, not a fixture protocol gap: the fixture's Mock sleeps
    `LUNA_MOCK_DELAY_MS` (3000) before every event, `SessionStarted`
    included, while the 2 s scheduler tick (`rpc/automations.rs:1297-1303`)
    turns any receipt whose reason is `ProviderAcceptanceUnknown` into
    Unknown. A Pending receipt carries that reason routinely while it waits
    for provider correlation (`board/lifecycle/drive.rs:664-672` treats it
    as unknown only once `committed_at_ms` is set, i.e. the engine closed it
    as ambiguous). So any provider slower than a tick to report its session
    is falsely marked Unknown, and it then blocks later runs. Suggested
    backend fix (yours): require `r.committed_at_ms.is_some()` for the
    reason arm, keeping the boot-mismatch arm. The fixture delay should stay:
    it is what exposes the race. `w20m-terminal-run.json` now also records
    the run's command receipt, observation time, the mock delay and how many
    times the prompt reached the chat.
- Writes: IDLE. Read-only until Codex sends diagnostics or releases the slot.

## Questions for Codex

1-6 answered in backend-status.md (thank you). Applied: `permission_ceiling:
None` on UI requests, `..AutomationDefinition::default()` in the two seed
fixtures, the supported RRULE subset in helper copy and `describe_rule`.

Still depends on you:

- The sanitized `luna`/`automation_update` input must keep exactly
  `{"automationId": "<id>"}`. `transcript.rs::automation_card_id` reads that
  key; the E2E builds its tool part through `luna_doc::sanitize_tool_call`, so
  it fails if the whitelist drops the id.
- Scheduled user messages keep ids starting `automation-message:`.
- A completed one-shot reports `completed: true`; the UI then shows Finished
  (Deleted still wins). The existing w20m case 70 is updated accordingly.

## Failure cases (written before the E2E)

UI E2E owner: `crates/ui/examples/chat-workbench-fixture.rs`, stage w20m
(cases 62-72 kept) plus new cases 73-82. Each names the regression it catches.

73. A malformed date (`2035-257-257`) or an impossible one (`2035-02-30`) is
    refused locally with its reason; an out-of-range time (`25:00`) too. No
    wrapping into a valid instant; Review stays an error (old A03).
74. The page follows the sidebar's project: project A's rows never appear
    under project B's filter, and an open editor closes on the switch.
75. New from an empty project page: defaults to that project, a fresh chat
    each run, no source chat; the saved target is
    `NewConversation { origin: None, project: B }`, authority project B.
76. Notifications (Never) and Archive successful runs (on) save and read back
    from the backend record; archive is forced off for an existing chat.
77. Execution: picking `mock-fable-5` + High saves exactly that; switching to
    `mock-1` (Medium only) shows the clamp note and the reviewed and saved
    request carries Medium, never the stale High.
78. Reopening an automation reads back every persisted value (name, prompt,
    destination, notification, archive, execution).
79. Edit while paused: Pause, rename only, Save: still Paused, execution
    unchanged, revision +1 (never silently resumed).
80. Retarget: switching project clears a chat chosen in the previous one;
    retargeting to project A's existing chat saves `Existing`, moves the row
    to project A's page and spells out the exact destination.
81. Custom rule: words for a supported rule and the host's next runs land on
    the rule's local times; an unsupported field is the host's Review error.
82. Delete, Show deleted (Deleted badge), and history still opens after
    deletion.
84. Right pane follows the focused tab: with Ops's chat focused beside Main
    (another project), New creates from that chat in Ops (source, chat and
    project), lists no other project's rows; clicking Main's tab moves the
    pane to Main and drops the Ops editor.
83. Inline card: an `automation_update` tool part as the doc stores it renders
    one card with the live name, cadence and status; Open shows that
    automation on the page (highlight + history), Edit opens its editor. The
    one-shot's delivered prompt shows "Sent by scheduled task"; typed messages
    don't.

Not reachable in the UI fixture without a backend hook: producing an Unknown
run to click Resolve. The UI path (Resolve… on Unknown rows → preview →
confirm, with "not resent" wording) is ready for your engine-backed QA.

## Test-only hooks (resolved)

- Removed: `Pickers::fixture_pick_model` (pickers.rs is back to HEAD),
  `AutomationsPanel::fixture_close_editor`, `fixture_show_deleted`,
  `fixture_focus`, `fixture_menu`, `AutomationCards::peek` and the transcript
  readback's card text/`automationId` additions.
- Model picks in case 77 go through the pickers' real keyboard path:
  `open_model_menu` → `model_row_ids` slot → `jump_model_slot` → `escape`,
  from the panel's existing fixture impl (`fixture_pick_model(model, window)`).
- Deleted history (case 82), the retargeted row (case 80) and the card's
  Open/Edit (case 83) go through `Shell::fixture_open_automation`, the card's
  real route.
- Kept: new `fixture_set` field names (menus through the real `pick` path),
  panel readback keys, user-row `badges` in `fixture_transcript_rows`.

## Remaining native QA (not covered by the UI E2E)

1. Menu contents: the Chat menu lists only the chosen project's unarchived
   chats on this device; the Project menu lists only this device's projects;
   arrows, Enter/Space and Escape work; Escape closes the menu before the
   editor.
2. Show deleted chip: off hides a deleted automation, on lists it Deleted
   with History available.
3. Inline card paint: name, cadence and status (Active, Paused, Finished,
   Deleted) update after a pause/edit elsewhere; Edit and Open are disabled
   until the record loads; a card for another device's automation says so.
4. Card Open with the sidebar filtered to a different project moves the
   filter to the automation's project.
5. Resolve… on a backend-produced Unknown run: preview, confirm, row reads
   Resolved with the "not resent" wording; Resolve is absent on other states.
6. Theme/layout: dark and light, narrow right pane (conversation scope), no
   clipped editor controls, picker popovers anchored inside the editor.
7. Pause/Resume/Delete from rows and Edit time on a Finished one-shot, by
   mouse and keyboard.

## Compile-risk notes from my read-through

- `transcript.rs`: new `RowKind::AutomationCard` arms in `charged_row`,
  `render_row`, `perf_row_text`; `top_gap_for` uses it. Other `RowKind`
  matches I found use wildcards.
- `automations.rs` uses `crate::shell::board::chat_badges::harness_label`
  (pub(crate)) and `gpui::Role::MenuItemRadio` (used elsewhere in the crate).
- If `AutomationRunState` / `AutomationOperation` gain variants beyond the
  contract, `run_state_label` and `render_action`'s match need an arm.

## Review findings in my UI code (fixed)

- U1: a legacy record (`execution: None`) opens with its whole captured
  config as explicit picks (`AutomationExecution::from(&authority.config)`,
  account and model options included); an untouched editor still saves
  `None`, and changing one field keeps the others. The pickers' "inherited"
  path is gone from the editor. No UI E2E: the current API normalizes every
  save to `Some(execution)`, so a legacy record exists only in migrated
  stores; your strengthened legacy-record engine test owns the backend half,
  and native QA can open a migrated record and change only reasoning.
- U2: a paused editor's review labels the host's times "After Resume:" and
  an empty preview reads "Paused: no runs until you resume it."

## Changed files

- `crates/ui/src/automations.rs` — project-scoped manager, editor, card store.
- `crates/ui/src/transcript.rs` — attribution badge, `AutomationCard` row,
  `TranscriptEvent::OpenAutomation`, fixture row badges/card readback.
- `crates/ui/src/shell.rs`, `crates/ui/src/shell/automations_pane.rs` — page
  scope from the sidebar filter, right-pane scope from the focused tab, card
  Open/Edit routing.
- `crates/ui/src/shell/workbench.rs` — `workbench_execution_focus` visibility
  only (`pub(super)`).
- `crates/ui/src/shell/activity_center.rs`, `crates/ui/src/lib.rs` —
  notification routing, Automation banner target, Activity automation alerts.
- `crates/ui/examples/sidebar-polish-fixture.rs` — `suppress_notification`.
- `crates/ui/examples/settings-gallery-fixture.rs`,
  `crates/ui/examples/chat-workbench-fixture.rs` — new proto fields; E2E.
