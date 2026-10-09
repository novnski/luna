# Implementation opportunities for LUNA

Research and design proposal, 2026-10-04. These items are recommendations, not implemented features or passed tests. Read [product findings](product-findings.md), [settings comparison](settings-analysis.md), and [source ledger](sources.md) together.

## Recommended direction

Make LUNA a recognizable native workbench for agent tasks. Keep the transcript calm, but let the surrounding UI answer five questions quickly: **what is this task, where is it running, what is happening, what changed, and what needs me next?**

LUNA already has much of the underlying machinery. The first releases should make it coherent and discoverable, then add a small number of missing controls. Preserve the local-first engine, provider-native permission semantics, and ordinary-checkout default required by this repository's instructions.

## Ranked opportunities

Complexity is relative and based on the identified foundations; it is not a delivery estimate.

| Order | Opportunity | Value | LUNA foundation | Main work | Complexity |
| --- | --- | --- | --- | --- | --- |
| 1 | Searchable, scoped, explainable settings | Everyday control and discoverability | Fourteen settings pages; project defaults; existing save paths | Metadata registry, anchors, scope/source UI, reset | Medium |
| 2 | Task header and useful empty state | More character and immediate orientation | Project/chat/host data; composer; durable startup/checkout context | Context header, project-aware starters, startup receipt | Low–medium |
| 3 | Clear attention views and source navigation | Handle several agents without losing questions | Engine attention, Activity filter, terminal inventory | Refine needs-input/ready/running views, preview/history/read rules | Medium |
| 4 | Visible project commands and run status | Close the code/run/review loop | ProjectAction, command receipts, stop/retry, preview URL | Task toolbar entry, run area, scoped controls and status | Low–medium |
| 5 | Small model loadout and action routes | Fewer repeated choices and sensible task defaults | Provider catalogs, starred model favorites, per-provider new-chat defaults, project defaults, title model, native review | Session/writing/review routes; provenance | Medium |
| 6 | Focus / Build / Review layout presets | Make the UI purposeful and less repetitive | Pane sizing, workbench splits and saved layouts, composer dock, related-chat panels | Named arrangements and per-task restoration; focus treatment | Medium |
| 7 | Next Git/review action with reason | Clearer completion workflow | Existing Git action and PR surfaces | Fresh-state resolver, action reason, alternatives, preparation | Medium–high |
| 8 | Better transcript/composer presentation controls | Readability and personal comfort | Tool groups, lazy blobs, wrapping, composer motion | Expansion/streaming choices and verified draft-safe behavior | Medium |
| 9 | Read-only diagnostics and storage view | Understand failures and disk use | Providers/devices/backups, engine metadata, per-chat Execution card | Process/resource/error readback; owned artifact accounting | Medium |
| 10 | Host capacity and recovery controls | Predictable multi-agent operation | Execution hosts, durable tasks, bounded retry, shared background-work rows with scoped Stop | Admission queue, waiting reasons, retry schedule/cancel | High |
| 11 | Project configuration with personal overrides | Repeatable setup across checkouts and hosts | Device-local project defaults/actions | Schema, precedence, trust, migration, changed-source handling | High |
| 12 | Browser/visual feedback attachments | Shorter path from a visual defect to a fix | Browser, Appshots, image attachments | Selected-element evidence and annotation attachment contract | Medium–high; audit existing browser selection first |
| 13 | One feature spanning related repositories | Useful for app/API/docs work | Multiple projects; task lineage and related chats | Feature bundle ownership, per-repo status and review | High |
| 14 | Durable structured workflow runs | Repeatable implementation/review/QA pipelines | Child chats, startup intents, results, journals, host-owned automations | Stages, output contracts, budgets, resume semantics | Very high |
| 15 | Floating task companion / PiP | Follow a task while using another app | Native windows and existing task state | Small, explicitly scoped view with correct source identity | Medium–high; optional |

Sources of inspiration: 1/8 from T3; 2/4 from Conductor; 5/6/7 from super.engineering; 10/14 from BB. Each combines existing LUNA primitives with a bounded change.

## Existing capabilities to refine, not reimplement

| Capability | Current evidence | Follow-up |
| --- | --- | --- |
| Persistent side chats and related child chats | Recent commits and `shell/related_chats.rs` | Improve naming, ownership, selection-based entry, and return-to-main clarity |
| Queue and steer | General settings and provider paths | Keep behavior explicit; test delivery correlation when changing UI |
| Attention/Activity | `shell/activity_center.rs` and engine attention | Add missing presentation controls around the existing authoritative model |
| Project commands, setup, stop/retry, preview | `project_commands.rs`, proto and engine | Improve access and receipts rather than add a second runner |
| Git commit/push and checkpoints | `changes/git_actions.rs` | Add contextual recommendations; keep file restore distinct from conversation rewind |
| Native review support | `shell/native_review.rs`, provider integrations | Audit visible controls/artifact handling before adding generic parallel state; Plan/Build stays cancelled |
| Theme import, fonts, glass, backgrounds | Appearance UI and source | Use the existing theme system for new identity/layout previews |
| Auto-settle and sidebar organization | `shell/spaces.rs`, UiSettings | Index contextual preferences in settings search |
| Bounded automatic retries | General and engine | Make schedule and cancellation visible; preserve delivery safety |

## A UI with identity

### Design analysis

**User/context:** a developer and operator working on multiple local or remote agent tasks, frequently switching to terminals, browser previews, code review, and support tools.

**Primary verbs:** orient, read, steer, run, inspect, review, and return to the right task.

**Feel:** quiet enough for long reading, with clearly visible operational state. Personality should come from composition, context, and feedback, not constant decorative motion.

**Domain vocabulary:** workspace, branch, run, checkpoint, handoff, attention, review, and artifact.

**Color world:** graphite canvas, warm paper, brushed aluminum, subdued copper, restrained ink blue, and terminal phosphor for semantic success. Use one contextual accent at a time; reserve error/success colors for actual state.

**Signature:** a narrow task trajectory near the header—context, current activity, changes, and review readiness—driven by real engine/forge state. It should be the thing that makes LUNA recognizable, and appear consistently in the header, run area, related-chat summary, settings preview, and compact task view.

**Defaults to reject:** endless anonymous monochrome rows, a large empty composer with no project context, and giving every panel the same visual weight. These are design choices to evaluate, not claims that all current LUNA screens exhibit them.

### Focus layout

- A generous reading column, quieter sidebar, and a compact context header.
- Activity and changes are small labeled controls that expand when asked.
- Composer rests at the bottom and preserves its current draft while the user reads.
- One clear attention indicator; no unrelated counters or looping glow.
- Useful empty state: project context, recent task, saved project actions, and two or three task starters derived from available capabilities.

This is the default for asking questions and reading substantial responses.

### Build layout

- Sidebar shows a short task label with a secondary status line where density permits.
- Header names project, checkout/branch, execution host, and permission state.
- Conversation occupies the center; run/preview/changes occupy a deliberate right workbench.
- Running command entries show exact command, checkout, lifecycle state, elapsed time where available, and stop/open-output actions.
- Panel motion is brief and optional; focus can softly dim other panes.

This borrows Conductor's run continuity and super.engineering's layout purpose without requiring a general arbitrary split tree.

### Review layout

- Changes or PR context becomes the main surface; a narrower chat stays available beside it.
- The header shows the comparison base, scope, freshness, and meaningful check/review status.
- Inline feedback becomes a typed draft attachment with file/line/context, not an unstructured sentence.
- A next-action control states both action and reason. Secondary choices remain visible.
- File restore explains that it affects files; conversation rewind explains provider-history effects separately.

### Settings treatment

- Search-first navigation, grouped pages, an explicit scope sentence, and source badges.
- Layout/theme choices use small realistic previews.
- The controls that make LUNA distinct—routes, task behavior, host capacity, and review—receive their own understandable grouping.
- Preserve native keyboard focus and accessible control labels.

The accompanying layout sketches use illustrative content and local-only interactions. They do not imply that proposed features are shipping.

## Settings acceptance scenarios

**Proposed E2E coverage; none executed for this research.** Write failure cases before implementing an isolated subsystem. Use the project's native E2E tooling and retain a repeatable artifact at the end.

1. **Search-to-control:** search an exact label and aliases such as “transparency,” “sleep,” and “queue.” Verify the right page, visible row, keyboard focus, and no preference write. Artifact: screenshots plus event/route trace.
2. **Unavailable setting:** connect a fixture host without a capability. Search must not land on an absent row; show why it is unavailable where appropriate. Artifact: capability snapshot, search results, and UI screenshot.
3. **Scope and provenance:** one fixture project inherits, another overrides. Verify effective provider/model/permission/checkout, displayed source, and reset-to-inherit. Artifact: before/after persisted config and rendered readback.
4. **Remote partial failure:** change an eligible preference for two fixture hosts and disconnect one. Show which host saved and which failed; do not report universal success. Artifact: operation receipts and independent owner readback.
5. **Effect timing:** change a new-chat default with an existing chat open. Verify the existing chat retains its runtime choices and the new chat uses the new default. Artifact: chat config snapshots and provider launch context.
6. **Reset and migration:** change one preference, reset it, restart, then load an older settings version. Preserve unknown/unrelated values and imported themes as specified. Artifact: saved config, migration log, and post-restart screenshot.
7. **Portable export:** export allowed preferences from a fixture. Verify credentials, local paths, permission grants, machine IDs, and runtime tokens are excluded. Import into another fixture and verify only intended keys changed. Artifact: reviewed export and config diff.
8. **Settings accessibility:** keyboard navigation, screen-reader labels, 24-hour/12-hour values, light/dark appearance, narrow window, and OS reduced motion. Artifact: accessibility tree, screenshots, and interaction replay.

## Workspace acceptance scenarios

**Proposed; not test results.**

1. **Correct execution context:** open two fixture projects on different hosts, start a permitted command in one, switch tabs, and stop it from its run area. Verify command/checkout/host ownership remains exact. Preserve command receipt, process readback, and UI recording.
2. **Attention lifecycle:** produce completion, error, approval, and user-input events. Visit, peek, mark unread, dismiss, and restart. Verify the attention service remains authoritative and a notification dismissal cannot approve/deny a tool action. Preserve event journal and readback.
3. **Layout restoration:** select Build, then Review, change pane widths, reopen the task and restart. Restore layout without changing task context or losing a draft. Preserve settings/session layout snapshots and screenshots.
4. **Proactive panels:** a completed turn crosses the proposed diff threshold; later the user chooses another panel. Verify the relevant panel opens at most when appropriate and does not override the later choice. Preserve turn event and panel-choice trace.
5. **Git action freshness:** use fixture states for dirty tree, ahead branch, missing PR, pending/failed checks, comments, conflicts, draft, merged PR, and unavailable forge. Verify the recommendation and its reason, then reread before any mutation. Preserve fixture refs and action-resolution receipts.
6. **Routing fidelity:** choose a writing route and a review route on different supported providers. Inspect exact provider/model/reasoning/permission context and output origin; a routing edit must not broaden execution permission. Preserve provider launch manifests and result correlation.
7. **Capacity and retry:** hold an admission limit, verify a waiting reason, free one slot, and test a known retry reset versus unknown delivery. Cancellation survives restart and uncertain delivery is not silently resent. Preserve journal, schedule, and terminal provider outcomes.
8. **Review attachment:** draft feedback on a precise changed line, switch comparison base, and verify feedback is not silently sent against stale coordinates. Preserve source revision, attachment payload, and screenshot.

## Before implementing the larger ideas

**File-backed project settings:** define ownership, precedence, schema validation, local-vs-shared intent, and changed-command review. Do not execute a newly discovered script simply because a file exists.

**Durable workflows:** define worker ownership, call budgets, permission inheritance, correlation, cancellation, and completion delivery. A successful cached answer does not prove that external side effects can safely be replayed. Start with a read-only research/review workflow before an implementation pipeline.

**Cross-repository feature bundles:** define how ordinary checkouts, branches, review bases, shared instructions, and per-repo results relate. Never silently create worktrees or mutate another repository because it was included in a feature label.

**Storage cleanup:** begin with read-only accounting. Actual cleanup needs candidate ownership, active-use checks, recovery boundaries, and an exact reviewable preview. Preserve current primary app, data, evidence, and the build cache policy.

## Suggested delivery sequence

1. **Make current capabilities discoverable:** settings registry/search, scope/source for existing defaults, task header, useful startup receipt, visible project commands.
2. **Give LUNA a consistent workbench:** Focus/Build/Review presets, better task identity, attention presentation, review readiness, realistic settings previews.
3. **Add deliberate control:** model routes, conversation preferences, diagnostics/storage readback, capacity/recovery UI.
4. **Evaluate bigger bets with real use:** portable project config, visual feedback, cross-repo bundles, durable structured workflows, optional PiP.

For future code changes, follow the repository's locked Cargo wrapper, focused E2E checks, formatting/build requirements, and LUNA Dev refresh/readback procedure. This research changes documentation and evidence only, so no Rust build or app refresh is required or claimed.
