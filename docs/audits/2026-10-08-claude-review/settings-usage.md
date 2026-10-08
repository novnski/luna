# Settings, usage and discovery review

Baseline: recent changes since 2026-10-03 through `d3819203`. This lane owns settings pages, usage ingestion/presentation, model discovery/cache and provider update detection. The parent review owns builds, tests, native app operation and final verification. No such operations ran in this lane.

## Finding SU-01: Settings keeps old models after Refresh models

Priority: P2. `HarnessesPage::refresh_model_cache` successfully updates the engine catalogs and emits `HarnessCatalogChanged`, but only composer pickers observed that signal. Provider default panels and the retained Projects page held independent model lists indefinitely; the advanced title-model list also stayed unchanged. A successful refresh therefore did not make newly discovered models selectable in these settings, and removed models stayed visible until an unrelated page reload.

Failure cases recorded before implementation:

- SU-01a: open Providers, load model A, publish model B from the provider, press Advanced > Refresh models, then open the Providers default-model menu; B must appear without refreshing accounts or restarting.
- SU-01b: first open Projects and load its model menu, then refresh from Providers; returning to the same project's model menu must use the new catalog.
- SU-01c: remove A from the new catalog; settings must not continue offering A as a current catalog choice. A saved A remains visible as an unavailable saved selection, without silently overwriting it.
- SU-01d: change model effort/options without changing its ID; settings must show the new choices after refresh.
- SU-01e: refresh while a provider default save is in flight; catalog invalidation must not cancel or overwrite that save. Project selection and unsaved composer content must be preserved.
- SU-01f: refresh while a title-setting save is in flight; reloading catalogs must not cancel its write. Remote-device and engine-generation guards must remain effective.

Fix applied:

- `crates/ui/src/settings/provider_defaults.rs:161` observes the existing catalog signal, closes the old menu and reloads an already-loaded panel. The independent defaults-save task and pending selection remain intact.
- `crates/ui/src/settings/project_defaults.rs:196` retires the cached catalog/task and menu under the existing generation counter. The next model-menu opening reads the new catalog for the same project. It preserves project selection and execution defaults.
- `crates/ui/src/settings/harnesses.rs:2496` reloads title settings/model choices after a successful catalog refresh. It does not replace an ongoing title save; that save already fetches its chosen provider's model list afterward.

Static evidence: the prior settings panels had no `HarnessCatalogChanged` subscription; `Pickers` already had one. `refresh_model_cache` calls `ListModels` with `refresh: true` then emits this signal. `ProjectsPage::action(Models)` reuses a ready list, which made its stale catalog persistent across navigation.

## Coverage

This is a source review. The table records inspected behavior and outstanding QA, not a claim that native interactions or live endpoints were exercised.

| Area | Inspected paths and result | Required central QA |
| --- | --- | --- |
| Settings kit and scale | `settings/widgets.rs` page/card/row primitives, wrapping, full-width blocks, disabled switches and interface-rem geometry; `appearance.rs` selectors and size/font controls. No additional concrete defect identified. | Gallery at 12/16/20 px, widths 900/1400, dark/light; inspect narrow wrapped rows, visible glyphs, switches, dropdowns and focus rings. |
| General and Notifications | `general.rs` event emission and synchronization; `notifications.rs` master/dependent switches and task policy/keep-awake controls. Parent-disabled controls are inert; master state is preserved separately from child preferences. | Toggle each master, verify dependent controls cannot activate by keyboard or pointer, restore master and confirm choices persisted. Exercise General/Shortcuts send-setting synchronization. |
| Providers and defaults | `accounts.rs` local/remote gating, account refresh triggers, enablement and update controls; `provider_defaults.rs` shared revision, pending state, model/options validation and reload on failed save; `engine/provider_defaults.rs` atomic revision-checked storage and `rpc/provider_defaults.rs` catalog validation. SU-01 fixed. | Execute SU-01a–f. Save two provider cards sequentially, switch account, and confirm the displayed selection and a new chat use current defaults. Existing chats remain unchanged. |
| Default-provider propagation | `save_new_chat_provider` persists the remembered provider and publishes `NewChatProviderChanged`; the existing composer observer adopts it. No composer edits made by this lane. | Change Providers > New chats with an already-open draft, then inspect a fresh chat and a project with an explicit provider. Coordinate with chat lane. |
| Advanced provider options | `harnesses.rs` refresh, title settings, instruction save scope, inventory and runtime-contract reloads; related `native_extensions.rs`, `harnesses/mcp.rs`, `harnesses/claude_sessions.rs` polish changes. The title-model refresh omission is included in SU-01. | Change the selected project/remote device during an instruction or catalog load; confirm the returned data belongs to the active scope. Use disabled/unavailable provider cases. |
| Projects | `project_defaults.rs` scope/engine-generation admission, selected-project identity, catalog requests, compare-and-set save parameters; command-card identity remains keyed to project/device/engine. SU-01 fixed for retained models. | A→B project switch and engine reconnect while requests are delayed; no stale reply should replace B. Check project commands card and explicit provider/model precedence. |
| Shortcuts and Appshots | `shortcuts.rs` recording interceptor/blur cleanup, conflict checks and reset path; `appshots.rs` capability controls, destination selector and permission actions. No additional concrete defect identified. | Record a conflicting chord, cancel/blur, switch Shortcuts↔Appshots, and verify scroll reset; capture to each destination in an isolated QA profile without sending it. |
| Files | `files.rs` autosave toggle, preset/custom delay segment and file-display events. Non-preset saved values remain visibly represented. | Use a custom delay, select a preset, reopen; toggle wrapping and ignored-file visibility and read back the actual editor/browser behavior. |
| Backups | `backup.rs` busy/refresh gating, export destination derived from status, inspect/stage flow and retry/error presentation. A new page is constructed on each Backups visit. No additional concrete defect identified in the polish changes. | Use disposable backup/profile data. During a delayed action, verify no duplicate action; stage only against the isolated QA profile and verify the status/error readback. |
| Devices and Archived | `devices.rs` device identity, rename/copy actions and metadata; `archived.rs` grouped rows and pending-request guard for unarchive. No additional concrete defect identified. | Rename/copy one synthetic device; double-activate one archived row and verify one mutation; cover offline/failure/retry state. |
| Updates and Automations | `shell.rs::render_updates_page` check/error/download status and controls; `automations.rs` scope/generation fencing, exact preview equality/expiry and mutation receipt recovery. No additional concrete defect identified in the polish changes. | Seed update error/available/downloading states without downloading. Automation preview→edit must invalidate Apply; expired preview needs Review again; lost response must use receipt recovery. |
| Local usage vs accounts | `shell/usage.rs:1598` selects only the local device snapshot for the compact card; provider tabs use local limits/sources. `maybe_open_low_usage_card` filters alerts to local working tasks. The full Usage page retains independent environment filtering. Account cards and history are distinct sources. | Seed local Claude/OpenCode and remote Codex usage, then verify only local providers/data in the compact card, remote data on full Usage, and no compact alert caused solely by a remote task. |
| OpenCode Go limits | `usage_opencode.rs` selects the saved `opencode-go` credential before environment/console/legacy sources, rejects unusable saved rows and fences in-flight credential changes by signature. The shared SQLite opener applies a 100 ms busy timeout and immutable mode for a closed store. No additional concrete defect identified. | Use isolated saved-key and key-switch fixtures; confirm 401/403/malformed responses remain unavailable and never reuse a different account's quota. Do not expose keys in artifacts. |
| OpenCode history/compaction | `usage_opencode_history.rs` schema introspection, usage-only SQL, v1/v2/legacy dedupe, fork exclusion, completed compaction rows, root isolation, bounded walks and completed-read caching. Compaction rows intentionally explain differences from OpenCode 2.0.22 CLI stats. | Run the existing reconciliation script over a copy of the QA DB and saved LUNA/CLI outputs. Include compaction, fork, malformed row, busy DB and repeated cached-read cases. This lane did not open a live provider DB. |
| Reset semantics | `proto/usage.rs::reset_state`, `shell/usage.rs:864` and `pooled_reset_label` preserve stale absolute resets and distinguish fresh future, passed, omitted and zero-used windows. No additional concrete defect identified. | Seed stale/past/future/missing resets for every provider; stale data must never become a live countdown or display a reset as newly confirmed. |
| Claude live discovery | `claude/mod.rs::models_live` delegates to `live_models`/initialize; the engine cache uses execution-aware model discovery. Empty/malformed discovery fails; normal non-refresh reads can use bundled fallback. | Fake CLI returns an added model after baseline, then fails; refresh should report failure without replacing the saved good catalog. Confirm an explicit refresh causes a new initialize call. |
| New-model badges | `model_cache.rs:53` normalizes dated/1m identities and preserves first-seen stamps; the first catalog is an unbadged baseline. `pickers.rs::model_is_new` restricts the seven-day badge to the latest label-family version. Providers/Projects reuse this helper. No badge code changed. | Baseline old catalog→new model; verify badge across three menus, restart retention, seven-day expiry, same-version spelling and an older back-catalog addition. |
| Provider updates | `provider_updates.rs` uses the v2 resolver, `@opencode/cli`, recognized install method, Cellar formula/receipt tap and same-major update availability. Compatibility detail is only attached for a version actually checked in this process. No additional concrete defect identified. | Synthetic install paths for curl/npm/bun/pnpm/Homebrew v2 and beta; v1 latest must not be offered to a v2 binary. Do not perform an actual provider upgrade during review. |

## Verification and evidence requirements

Completed in this lane:

- Read repository instructions, workspace and crate manifests, relevant README and UI CI configuration.
- Inspected recent commits and the current call paths above.
- `rustfmt --edition 2024 --check crates/ui/src/settings/provider_defaults.rs crates/ui/src/settings/project_defaults.rs crates/ui/src/settings/harnesses.rs` passed.
- `git diff --check` passed after the change.
- No unit tests added. No Cargo invocation, native app action, live quota request, provider update, commit or push performed.

Central run should compile the final merged UI changes and run the existing Settings gallery. The fixture's own instructions specify:

```sh
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example settings-gallery-fixture
target/debug/examples/settings-gallery-fixture ABSOLUTE_EVIDENCE_DIR --sizes 12,16,20 --widths 900,1400 --themes dark,light
```

Run it only after the parent has coordinated the single-GUI-app slot. Keep `manifest.json`, `index.html`, `settings-gallery.pass` and the PNGs. The gallery is static; it does **not** prove the dynamic SU-01 fix on its own.

For SU-01, retain a dynamic provider fixture or controlled CLI whose first initialize/list response has A and whose second has A+B with changed effort/options. Open and retain Projects, Providers and the title-model list before changing the provider response. Activate the actual `refresh-model-cache` control, wait until catalog requests finish, and capture/read back B in Providers, Projects and Session titles. Repeat with A omitted and with delayed saves. Record exact returned model IDs, the selected defaults before/after, request counts and screenshots. This is the acceptance gate still pending central QA.

For OpenCode historical reconciliation, `scripts/qa-opencode-usage-reconcile.py --db COPIED_QA_DB --luna SAVED_LUNA_READBACK --stats SAVED_OPENCODE_STATS --out RESULT_JSON` compares saved evidence without a new model call. Use the parent QA runner's prepared Python environment. Preserve the input hashes and result JSON; do not label any mismatch fixed without inspecting its compaction/window-skew breakdown.

## Handoff

Changed source paths: `crates/ui/src/settings/provider_defaults.rs`, `crates/ui/src/settings/project_defaults.rs`, `crates/ui/src/settings/harnesses.rs`. This report is the fourth owned path. All other working-tree changes belong to the parent/other review lanes and were preserved. Source is frozen for parent verification.

## Startup follow-up SU-02: Shell opened after readiness needs its initial splash reconciled

The initial background capture of signed `LUNA QA Review` (PID 29323, IPC 59841, isolated profile `target/luna-qa-data/claude-review-20261008`) appeared to retain the setup splash while IPC returned a ready local engine and empty workspace rows. A later capture after scenario project creation showed the app, but that sequence alone did not establish that project creation caused recovery. Preserve `target/qa/claude-review-20261008/startup-empty-profile.json` as empty-engine evidence, not proof of a blocked UI bootstrap.

The parent repeated the native check with a rebuilt signed app and genuinely empty profile `target/luna-qa-data/claude-review-20261008-fresh` (PID 251). A background capture again showed a spinner, but **raising the existing window immediately showed the normal welcome controls, with no project/chat mutation**. That establishes a stale/occluded capture limitation; it does not establish a production bootstrap hang. Keep the raised-window screenshot beside explicit EngineReady and empty WatchSpaces/WatchChats readbacks. IPC readiness alone must never be equated with `AppState::connection == Ready`.

The narrower source finding remains valid: `Shell::new` always initialized `splash` at `Visible`, and only a later `on_state_changed` advanced it. A Shell constructed around an already-ready state could therefore wait for another notification. `gate_phase` itself has no requirement for a project/chat, and this change does not add or bypass any readiness/authentication gate. The `ready-empty` fixture directly covers this ready-before-Shell ordering; the parent reports the 26 startup fixture executions pass. This is separate from the disproven interpretation of the background native capture.

Failure cases recorded before the follow-up implementation:

- SU-02a: a ready local/development engine with zero projects and zero chats must reveal the welcome canvas without creating a project or receiving another workspace change.
- SU-02b: a Shell created after connection readiness must not stay at `Visible` waiting for an already-past notification.
- SU-02c: a genuinely connecting engine must keep its setup overlay; an engine failure must immediately reveal its error gate.
- SU-02d: readiness transitions start one fade and retire it; ordinary subsequent frames must not restart the fade or resurrect the splash.
- SU-02e: existing startup selection, manual-new, archived/settled-only and child-thread behavior remains unchanged.

The existing startup fixture's `empty` case always created `startup-project`, so it did not cover a true first-install workspace. Extend its rendered acceptance with truly projectless and ready-before-Shell cases; save JSON, PNG and pass receipts. Parent retains ownership of compilation, running the fixture and app refresh.

Follow-up implementation completed:

- `Shell::reconcile_splash` now runs both at the existing state-observation boundary and at the start of rendering. It reads the existing connection state, starts at most one fade, keeps Connecting covered and exposes Failed immediately. It does not alter `gate_phase`, workspace scope or authentication rules.
- `startup-fixture` adds `no-project` (real engine watches, zero stored projects/chats) and `ready-empty` (an existing ready AppState before Shell construction, without later workspace watches). Its first-draw readback catches a still-Visible splash for the latter; the final readback requires `gone` and verifies no project/chat was manufactured. Existing cases now also assert that the splash is gone.
- `rustfmt --edition 2024 --check crates/ui/src/shell.rs crates/ui/examples/startup-fixture.rs` and `git diff --check` passed. No build, test run or app operation was performed by this lane.

The parent executed startup validation; consult its retained artifacts for final run results. A temporary opt-in startup trace patch was prepared concurrently with the raised-window clarification, then fully reverted before any build or run. `crates/ui/src/state.rs` has no remaining diff, and no startup tracing is retained.

Run each new case twice using its own evidence directory:

```sh
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example startup-fixture
target/debug/examples/startup-fixture ABSOLUTE_NO_PROJECT_DIR no-project first
target/debug/examples/startup-fixture ABSOLUTE_NO_PROJECT_DIR no-project restart
target/debug/examples/startup-fixture ABSOLUTE_READY_EMPTY_DIR ready-empty first
target/debug/examples/startup-fixture ABSOLUTE_READY_EMPTY_DIR ready-empty restart
```

Also rerun `empty`, `manual-new` and an active/archived startup case using separate persisted case directories. Retain and hash the `.json`, `.png` and `.pass` artifacts. Finally launch the rebuilt signed QA bundle against another genuinely empty isolated profile, confirm normal welcome controls without seeding anything, and save the empty `WatchSpaces`/`WatchChats` readback alongside its screenshot.

Follow-up source ownership is limited to the splash/readback hunks in `crates/ui/src/shell.rs` and `crates/ui/examples/startup-fixture.rs`. These paths are now frozen for the parent.
