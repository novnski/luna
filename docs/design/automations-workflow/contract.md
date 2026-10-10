# Project automations workflow

Iulian asked Codex and Claude Desktop to implement the reviewed automation
workflow together: Codex backend, Claude frontend, shared integration and QA.
The reference is `/Users/iulian/Desktop/SCREENSHOTS/Screen Recording 2026-10-09 at 23.31.15.mov`.
Review evidence is `target/qa/automations-review-2026-10-09/`, especially
`findings.json`, `video-observations.json`, and `video/frame-125.jpg` /
`video/frame-155.jpg`. This is implementation, not another review or mockup.

## Ownership and coordination

- Claude owns `crates/ui/**`, including UI examples/fixtures and the UI manifest
  only if its fixture requires it. Follow `test-audit` before writing tests.
- Codex owns proto, engine, sync/doc/RPC, backend tests, scripts, these contract
  docs, integration review, all Cargo commands, app launch/refresh, native QA,
  final commit and push. Neither agent reverts the other's edits.
- Work in the existing main checkout. No new worktrees or subagents. Claude
  does not run Cargo, launch/quit LUNA, commit or push. Format only owned files.
- Claude records progress/questions in `frontend-status.md`. Codex records
  backend readiness/build ownership in `backend-status.md`. A build starts
  only after both writers stop editing; Codex announces the build interval.
- Use High reasoning. Preserve unrelated work and existing app data/caches.

## Product contract

Automations are owned by a project and local host. Source-chat identity is
provenance, not a permanent execution dependency. A source chat can create an
automation targeting itself, another active chat, or fresh chats in a selected
project. Editing must never silently resume a paused automation or change its
saved execution configuration. Removal of a project blocks its scheduled work.
Existing conversations keep their native provider/account: model and reasoning
may vary within that binding. Selecting another provider/account requires fresh
conversations or the chat's explicit provider/context-switch action; scheduling
never silently resets a conversation. Saved folders must still belong to the
project or a registered worktree, and known checkout IDs are rechecked before
dispatch and task admission. This pins a checkout location, not a branch/commit.
An uncertain old occurrence is never replayed; a deliberate Resolve action
acknowledges that old outcome and permits separately scheduled future work.

The main Automations page follows the selected project and supports New.
The editor includes name, prompt, destination project/chat, fresh-chat toggle,
provider/model/reasoning, cadence/local time/timezone, notification policy,
archive-successful-runs (fresh chats only), preview and save. Use real catalogs
and project/chat data, not hardcoded model inventories. Prefer existing picker,
button, input and theme components. Keep the calm native/inset LUNA direction.

Chat-created automations need an inline card (name, cadence, status, Open/Edit)
and triggered user messages need `Sent by scheduled task` attribution. Normal
chat messages remain unchanged. Keep saved automation history accessible after
deletion. Finished one-shots display Finished rather than Active or Paused.

## Shared types

Canonical definitions are in `crates/proto/src/automation.rs`, re-exported by
`luna_proto`. JSON is camelCase. Existing stored definitions remain readable.
The following is the shared frontend/backend contract. The protocol source is
authoritative; refinements made during integration are recorded below.

`AutomationExecution` (all optional fields use serde defaults):

- `harness: HarnessId`
- `provider_account_id: Option<String>`
- `model: Option<String>`
- `reasoning: Option<ReasoningLevel>`
- `model_options: serde_json::Map<String, serde_json::Value>`

`AutomationDefinition` keeps existing fields and adds:

- `execution: Option<AutomationExecution>`; explicit selection is saved and
  pinned. Legacy/omitted selection captures the existing approved config.
- `source_task_id: Option<String>`; creating chat, if any, for the inline link.
- `notification_policy: AutomationNotificationPolicy` with `AllRuns`,
  `ImportantUpdates` (default), `Never`.
- `archive_successful_runs: bool` (default false).

`AutomationTarget`:

- `Existing { task_id: String }`.
- `NewConversation { origin_task_id: Option<String>, project_id: Option<String> }`.
  At least one identifies an initial destination. The project picker supplies
  project_id; creating from a chat may also supply origin_task_id. Legacy string
  originTaskId still decodes as Some. Dispatch uses the saved destination and
  does not require an unarchived/live originating chat.
- `origin_task_id()` consequently returns `Option<&str>`.

`AutomationTrigger` keeps Once/Interval/Calendar/PrWatch and adds
`RRule { rule: String, anchor_ms: i64 }`. Use a Custom schedule editor with a
human-readable explanation and server-provided next-occurrence preview. The supported fields and validation limits are documented below.

`AutomationRecord` adds `completed: bool` (default false); display Finished when
true, Deleted when deleted, Active when enabled, otherwise Paused. Metadata-only
edits of finished/paused schedules preserve that state; changing a finished
schedule to a future time re-arms it. A skipped/failed one-shot is paused and
does not claim successful completion. Existing
records' execution choices are available in `authority.config` as fallback.

`AutomationOperation` adds `ResolveUnknown { occurrence_id: String }`.
`AutomationRunState` adds `Resolved`. Never label this as successful delivery:
the old uncertain run was acknowledged, not retried. Offer it only for Unknown
and require the ordinary preview/confirmation flow. Backend still rejects a
live/in-flight run. Saving a definition preserves its paused state.

`AutomationRun` adds `automation_name: String` and
`notification_policy: AutomationNotificationPolicy` with defaults. Other
backend effect bookkeeping may be added with defaults; UI fixtures should use
the final canonical fields, not duplicate the schema.

Landed detail: AutomationRun also has `project_id: String` and
`finalized_at_ms: Option<i64>` (both defaulted), preserving historical execution
scope and a terminal-effect checkpoint. AutomationMutationRequest also has
`permission_ceiling: Option<PermissionMode>`; UI sets None. It is a host-derived
agent cap, absent from the provider-facing tool schema. Cross-project agent
retargets and ResolveUnknown always require a local one-use approval, even in Full mode.

`AutomationReadRequest` adds `project_id: Option<String>`. All supplied filters
intersect, including automationId and includeDeleted. ListAutomationRuns honors
taskId/projectId too. Existing RPC method names and paged replies remain.

Activity retains muted outcomes. `AttentionItem.suppress_notification: bool`
(serde default false) gates desktop banners and sounds, not the Activity row.
ImportantUpdates suppresses success notifications; Never suppresses all run
notifications but leaves blocked/failed work discoverable. Repeated skipped
slots coalesce into one alert per automation. `AttentionTarget::Automation {
automation_id, project_id }` opens the correct manager/history when no result
chat exists; successful runs still link to their actual conversation.

Agent tools keep `automation_update` and `list_automations`, with the new fields
and lifecycle actions. Saved tool results will include `name`, `target`, and
`scheduleDescription` alongside id/revision/enabled/etc so the transcript can
render a semantic automation card. Scheduled user-message IDs already start
with `automation-message:`; use that exact backend-owned identity for initial
trigger attribution, with run lookup when a specific automation link is needed.

## Frontend acceptance

1. Create in the current chat and from an empty project Automations page.
2. Edit name/prompt/model/reasoning/timing independently; preview changes; save
   and reopen to read back persisted values. Switching models validates effort.
3. Retarget to another chat and to a new chat in another project; show exact
   destination and require an ordinary reviewed save. Switching projects never
   keeps the previous project's rows/editor as if they belonged to the new one.
4. Pause, edit while paused, resume, delete, Show deleted and inspect history.
5. Inline card opens the correct saved automation; triggered messages are
   visibly attributed; a resolved unknown run explains that it was not resent.
6. All-runs / important-updates / never notifications and successful-run
   archiving reflect saved backend values.
7. Reject malformed dates without integer wrapping. Calendar and custom rule
   editing have clear errors and authoritative next-run previews.
8. Keyboard/accessibility labels, loading/error/empty states, dark/light themes,
   narrow panes and no clipped editor controls. No raw JSON shown as the UX.

Codex owns final deterministic engine E2E, signed-app/live-provider scheduling
smokes where available, native mouse/keyboard readback, evidence hashes, one-GUI
app discipline, refresh of primary LUNA Dev, signature/port/PID verification,
and commit/push. No claims of real-provider execution from a mock result.
