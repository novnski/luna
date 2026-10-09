# Settings: gaps, discoverability, and a better control model

Research date: 2026-10-04. **Recommendation:** improve settings search, ownership/scope, and effective-value explanations first; add action routing and operator controls next. LUNA's settings contain substantial functionality already. The feeling that they are sparse partly comes from presentation and from preferences living outside Settings.

All statuses below are bounded to the inspected UI and source. **Existing** means a control or implementation was found, not that every runtime path was E2E-tested. **Partial** means a narrower control exists. **Gap** means no corresponding user-facing control was found in the targeted audit. **Audit** means more runtime/source work is necessary before claiming absence.

## What was inspected directly

| Product | Settings evidence | Most useful pattern |
| --- | --- | --- |
| BB 0.45.0 | General, Appearance, Files; core and plugin navigation | Host-owned settings with plugin-owned sections; customizable surfaces |
| T3 Code 0.0.44 | General, Appearance, Source Control, Storage; search-to-row and provenance popover | Search anchors, scope sentence, value sources, row reset controls |
| Conductor 0.90.1 | General, Appearance, Default models, Git, Environment, Advanced | Model loadout, concrete behavior copy, operational controls, file-backed settings entry |
| super.engineering c623b42b | Appearance, General, Routing, Notifications; settings category search visible | Visual previews, action routes, global/workspace/project scope, notification lifecycle |
| LUNA Dev | General, Appearance, Projects | Many existing primitives, but a flat navigation list and limited cross-cutting explanation |

Fresh upstream source snapshots were inspected for BB and T3. They are ahead of the installed versions; source-only findings are identified in the [source ledger](sources.md).

## What LUNA already has

The Settings navigation has fourteen destinations: General, Projects, Providers, Appearance, Notifications, Shortcuts, Files, Appshots, Automations, Usage, Updates, Devices, Backups, and Archived sessions. Automations (host-owned automations and PR watches) was added on 2026-10-05. This is not an empty foundation.

- **General:** send shortcut, queue/steer behavior, context usage, in-app links, automatic retries, prevent sleep, archive/settle/delete confirmation.
- **Appearance:** independent light/dark theme choice, theme import/linking, accent presets, theme/frosted/opaque glass, new-thread background image/effect, interface/terminal/code fonts and sizes, conversation width, separate wrapping options, diff layout, accessible diff color pairing, and reset.
- **Projects:** provider/model, permissions, current-checkout versus new-worktree defaults, inheritance, project commands, setup-on-creation and wait-for-setup behavior.
- **Notifications:** event sounds, desktop delivery, background-only behavior, per-task policy and per-task keep-awake choices.
- **Files:** autosave and delay, word wrap, hidden/ignored file visibility.
- **Providers:** provider enablement/setup, account/usage surfaces, per-provider defaults for new chats, title generation, skills and MCP catalogs, native provider extensions. Exact actions depend on provider support.
- **Other foundations:** configurable shortcuts, backups, archive recovery, updates, and device management.

Code pointers, pinned to the research baseline, are in the [LUNA source map](sources.md#luna-source-map).

Preferences such as sidebar sorting/grouping and auto-settle live in **Sidebar view options** (`shell/spaces.rs:1135`) rather than the Settings landing page. They should remain available in context and also be discoverable through settings search. Duplicating their state would create inconsistent controls.

## Gap matrix

Priority is a proposed implementation order, not a measured score. P1: high value for daily work. P2: useful after the foundation. P3: optional or a larger investment.

### Navigation and ownership

| Setting or UX capability | Strong reference | LUNA status | Recommendation |
| --- | --- | --- | --- |
| Search setting names, descriptions, aliases | T3 search verified; Conductor/super search visible | Gap | **P1:** one registry drives search, section labels, and row anchors |
| Navigate to and focus the exact setting row | T3 interaction verified | Gap | **P1:** search and command-palette results should share destinations |
| Explain the setting's effective source | T3 popover verified; super route inheritance observed | Partial: project “inherit global” options | **P1:** show source, owner, scope, and reset-to-inherit |
| Clear device versus host versus project ownership | T3 project/environment sentence | Partial: local UI plus project/engine settings | **P1:** label ownership; do not make every key globally syncable |
| Multi-environment mixed values and partial-save reporting | T3 source | Gap in inspected generic settings surface | **P2:** add when bulk editing hosts/projects is supported |
| Per-row reset | T3 observed | Partial: appearance/global project reset; shortcut reset | **P1:** reset one preference without resetting a whole page |
| Changed-only view and override count | Proposed extension of provenance UX | Gap | **P2:** show deliberate overrides and their owners |
| Share/export only portable preferences | Proposed | Partial: backups are broader recovery artifacts | **P2:** exclude credentials, local paths, machine IDs, and execution grants |
| Searchable advanced sections | T3 folded typography; super category subnavigation | Partial | **P1:** progressive disclosure must remain searchable |
| Settings destination usable outside fixtures | T3 URL/anchor routing | Partial: LUNA fixture route mapper exists | **P2:** add an app-supported settings destination contract; do not mistake fixture routing for production deep links |

### Providers and conversation behavior

| Setting or UX capability | Strong reference | LUNA status | Recommendation |
| --- | --- | --- | --- |
| Provider executable/setup and health controls | All four; Conductor observed overrides | Existing/partial | Improve status and capability explanations; retain current owners |
| Small model favorites/loadout | Conductor observed | Existing: starred favorites in the model picker, and per-provider new-chat defaults in Settings → Providers | Make both discoverable through settings search |
| Model/reasoning per action class | super Routing observed; T3 writer model observed | Partial: title generation, project defaults and per-provider new-chat defaults | **P1:** session, lightweight writing, review/repair routes with explicit inheritance |
| Editable built-in action instructions | super documented | No general route/template editor found | **P2:** named, reviewable templates; distinguish these from skills and shell scripts |
| Queue versus steer | BB/T3/Conductor observed | Existing, observed | Retain; improve alternate-shortcut hint consistency |
| Response streaming presentation | T3 observed paragraph streaming | No user-selectable mode found | **P2:** compare token/paragraph/finished-response options for readability |
| Rich Markdown composer | BB/T3 observed controls | Audit | Verify current composer behavior and accessibility before adding a mode |
| Collapse composer while reading history | T3 control observed and focus-state source | Audit: LUNA has composer docking/motion | Check scroll behavior and draft/focus preservation; refine existing machinery |
| Default tool-call expansion | Conductor observed | No settings preference found | **P2:** compact/expanded preference with per-group override |
| Pasted-text attachment threshold | Conductor observed | Audit | Verify paste pipeline first; expose threshold only if it solves real composer clutter |
| Show per-session MCP connection status | Conductor observed; LUNA catalog controls present | Partial | **P1:** health/status link near the composer, scoped to the current provider session |
| Model visibility/capability controls | T3 and super provider settings source/docs | Partial: the focused chat's Execution card shows adapter capabilities and the admitted runtime | Show supported plan/review/image/steer capabilities rather than universal toggles |

### Workflow and machine operation

| Setting or UX capability | Strong reference | LUNA status | Recommendation |
| --- | --- | --- | --- |
| Saved named project run commands | Conductor/super; LUNA observed/source | Existing | Surface existing commands in the task toolbar; keep durable receipts |
| Setup-on-new-checkout with wait policy | LUNA source; competitor lifecycle controls | Existing for new worktrees | Improve visibility; follow LUNA's authorized checkout policy |
| Shared repository configuration plus personal override | Conductor documented; T3 source | Partial: device-local project actions | **P2:** explicit, schema-validated portable project settings with personal overrides |
| Port allocation and named service discovery | Conductor documented | Partial: LUNA actions have preview URL | **P2:** actual runtime port/URL readback rather than a guessed link |
| Git writing style/templates/model | T3 Source Control observed | Partial: LUNA message generation and Git actions | **P2:** repository conventions and template-aware generated text |
| Merge/check policy | Conductor observed | Audit: LUNA has PR controls | Expose applicable forge policy and local preferences; do not weaken repository requirements |
| Auto-settle inactivity | T3 observed | Existing in Sidebar view options | Add a search route to the existing preference |
| Snooze and wake semantics | T3 observed/source | No snooze preference/control found in targeted lifecycle audit | **P2:** defer work while preserving urgent runtime requests |
| Concurrency/admission controls | BB source; installed plugin entry | No user-facing capacity settings found | **P2:** host/global caps with a visible waiting reason |
| Retry maximum wait and scheduled recovery control | BB source | Partial: LUNA on/off, four attempts/six-hour bound | **P2:** show/reset/cancel the current recovery policy before making budgets configurable |
| Resume after update/crash/restart | T3 control observed | Audit: LUNA durable startup/context operations already exist | Trace acceptance and restart state before adding an automatic-resume switch |
| Battery/idle/locked-host background profile | T3 observed/source | Partial: LUNA keep-awake control | **P2:** bounded background probes with clear effect; never silently suspend active authorized work |
| Terminal scrollback budget | Conductor observed | No settings budget found | **P2:** expose an understandable retention limit with scope and effect |
| Storage usage/retention/cleanup preview | T3 Storage observed; super General observed/docs | Partial: backups and archive, no general storage page found | **P1:** inspect usage first; cleanup candidates require exact ownership and live-use checks |
| Process/resource/log diagnostics | T3 diagnostics entry and source | No unified diagnostics page; per-chat execution diagnostics exist | **P1:** read-only process, connection, version, resource, and recent-error views |

### Appearance, layout, and notifications

| Setting or UX capability | Strong reference | LUNA status | Recommendation |
| --- | --- | --- | --- |
| Light/dark themes and import | T3/BB; LUNA observed | Existing | Improve theme preview and discovery; avoid another parallel theme format |
| Per-project/workspace visual identity | super documented | Partial: app-wide accent and project identifiers | **P2:** small contextual accent/icon treatment separate from semantic status |
| Glass intensity/contrast controls | T3 observed; super documented | Partial: theme/frosted/opaque choices | **P2:** expose readability controls only with consistent contrast guarantees |
| Typography previews and separate fonts | T3/Conductor; LUNA observed | Existing | Add realistic preview contexts and searchable advanced controls |
| Density/sidebar style/layout presets | super observed/docs | Partial: resizing, workbench splits and saved layouts | **P1:** Focus, Build, Review presets; **P2:** compact/detailed density |
| Inactive-pane dimming | BB observed/source | No preference found | **P2:** useful when simultaneous panes ship; preserve contrast |
| Motion duration/reduced motion | T3 observed control | Existing: follows the macOS Reduce motion setting; no in-app preference | **P2:** optionally expose a quieter in-app mode |
| Proactive opening of relevant Changes/PR panel | T3 observed control/source | No matching preference found | **P2:** default restrained, respects later user choices |
| Persistent notification history and clear-on-visit policy | super observed/docs | Partial: engine attention/activity exists | **P1:** audit/refine existing attention service rather than create a second inbox |
| Per-event delivery and sounds | super observed; LUNA sounds present | Existing/partial | Add event-specific delivery where useful; show OS limitations clearly |
| Searchable context-menu preferences | Proposed from LUNA discoverability finding | Partial | **P1:** same registry points to contextual settings without duplicating storage |

## Proposed settings structure

Group the navigation by user intent, not by the code module that owns the control:

| Group | Destinations | Why |
| --- | --- | --- |
| Personal | General, Appearance & Layout, Keyboard, Notifications | Device-local interaction and comfort |
| Agents & execution | Providers & Accounts, Models & Routing, Conversation, Environment & Capacity | What runs, where it runs, and how it behaves |
| Project | Selected project, Defaults, Commands & Setup, Git & Review | Project-aware execution and review |
| System | Devices & Connections, Storage & Backups, Diagnostics, Updates | Machine health and durable data |
| Recovery | Archived work | Restoring prior work without mixing it with destructive cleanup |

A grouped sidebar should not expose twenty separate pages immediately. Keep a small set of pages and expand subsections when needed. Search can index every row even when a section is collapsed.

### The row contract

A setting row should be able to answer:

1. What behavior does this change?
2. Who owns this value: this viewing device, an execution host, a project, or this chat?
3. What is currently effective, and where did it come from?
4. Does changing it affect current work, new chats, or the next restart?
5. Has the change been saved, or is the owner unavailable?
6. How can I restore the inherited/default value?

Examples: “Theme: this Mac; applies immediately.” “Default review route: project; applies to the next review.” “Provider executable: execution host; active sessions keep their current process.” These explanations should describe actual behavior, not speculative promises.

### Scope should follow the key

LUNA already has device-local `UiSettings` and engine-backed project defaults/actions. Preserve that boundary. A viewing phone should not set a Mac's UI font, and a project default should not retroactively change permissions on an existing chat.

For the proposed registry, record allowed scopes, source/provenance, default, validation, effect timing, availability, aliases, and destination. Use the existing resolver for project execution defaults. Add scope only when the underlying owner supports it; never show a global/project selector that cannot actually change the value in that scope.

Repository configuration is a separate future step. Its precedence needs a written specification and conflict tests before implementation. T3's resolution chain is useful evidence, but LUNA should not adopt its exact precedence blindly.

## UI treatment

- Search always visible at the top, with category breadcrumbs in results.
- A small owner/scope sentence under the page heading when scope applies.
- A calm source badge: “Device default,” “From project,” “CLI default,” or “Override.” Expand it for details.
- Contextual reset beside the affected row, and a page reset with an explicit preview.
- Appearance previews show the actual sidebar, transcript, composer, and diff roles. Tiny color swatches alone do not explain contrast or density.
- Settings that cannot apply show a reason and a relevant status/action link.
- Advanced controls collapse, but searches reveal them and focus the match.
- Operational settings show last read/saved state. Avoid optimistic “saved” claims after a remote write failed.

## First settings release

Implement **search + metadata + provenance for existing project defaults + row reset** first. This makes current functionality discoverable and creates a foundation for later additions. Then add **Models & Routing**, **Diagnostics/Storage readback**, and a few **Layout/Conversation preferences**. Expanding all categories at once would create a larger but equally confusing settings area.

The proposed acceptance scenarios are in [luna-opportunities.md](luna-opportunities.md#settings-acceptance-scenarios). No preference was changed to produce this research.
