# What makes these apps special

Research date: 2026-10-04. Evidence labels and exact revisions are in [sources.md](sources.md). Settings have their own [detailed comparison](settings-analysis.md).

## The useful distinction

These products put different objects at the center of the experience:

| App | Center of gravity | What to learn from it |
| --- | --- | --- |
| BB | A programmable agent workspace | Extensibility, visible orchestration, composable panels, operator controls |
| T3 Code | A thread inbox connected to execution environments | Attention management, coherent composer behavior, scoped settings, polished transitions |
| Conductor | A workspace that progresses toward review and merge | Onboarding, setup/run/review continuity, small model loadouts, specific feedback |
| super.engineering | A persistent engineering workbench | Flexible layouts, workspace identity, contextual Git actions, reusable action routing |
| LUNA today | A native, local-first provider control surface | Strong existing primitives; the opportunity is to connect and present them clearly |

This is a qualitative judgment from the inspected screens and source, not a speed, reliability, or usability benchmark.

## BB: the application itself is an extension surface

**Observed:** the empty screen has a prominent composer near the top, then an explicit context row for project, environment, branch, and permission mode. Navigation gives Plugins, Skills, and Automations first-class destinations. Appearance can select the sidebar implementation and navigation owner, reorder footer destinations, and fade inactive splits. File preferences choose viewers by extension. These controls make the application feel like an adaptable workbench rather than a fixed chat shell.

**Source:** BB's README describes desktop, web, CLI, and HTTP API surfaces. The source separates client components, server/host execution, provider adapters, and plugins. A concrete example is `buildPluginSettingsEntries`: enabled plugins with settings or a custom settings section become sorted navigation entries. Another is `SplitThreadArea`, which reads a stored layout, resolves panes, tracks focus, and updates the split tree. See [BB source map](sources.md#bb-source-map).

### Interesting functionality

1. **Side conversations have a return path.** A selected message or selection can lead to a persistent side chat, and a reply can be sent back into the main thread. The source sends a queued message to the source thread and records the side thread as sender. That preserves provenance rather than producing an anonymous pasted answer. **Source; not exercised here.** LUNA has recently implemented persistent side chats and related-chat panels, so this belongs in a refinement audit, not a new-feature backlog.
2. **Durable scripted workflows.** The optional workflows plugin exposes phases, worker calls, structured results, a live run card, and stop controls. Its runtime uses a constrained JavaScript environment and its cache key includes prompt, previous result key, provider/model/reasoning/permissions, schema, and execution protocol versions. Reuse accepts successful non-null results only. **Source; plugin is off by default and was not enabled or run.** This is the most substantial BB-inspired architectural opportunity, but it should come after simpler workspace improvements.
3. **Concurrency is a product control.** The concurrency plugin presents host capacity and effective limits, queues new turns when a host/global cap is reached, and explains why the turn waits. It distinguishes a new turn from a message to an already-running thread. **Source; live settings entry observed, capacity behavior untested.** This is especially relevant to LUNA's build-budget and multi-host workflows.
4. **Retry is visible and cancellable.** The provider-retry plugin offers scheduled recovery and a queued card that can be sent now or cancelled. Credit/spend failures are distinct from transient overload or subscription reset. **Source.** LUNA already has bounded retry support; borrow the explicit schedule, reason, and controls where needed.
5. **Commands and extensions can enter the same UI vocabulary.** Plugin settings, file viewers, sidebar providers, and commands participate in host surfaces. The lesson is stable extension contracts and discoverable controls, not “let arbitrary plugins replace everything.”

### UI ideas worth adapting

- Put the current execution context close to the composer. A branch name alone is insufficient when several hosts or checkouts exist.
- Use restrained dimming to identify the active pane in a multi-pane layout. Keep inactive content readable and provide an off switch.
- Show orchestration as phases and named workers, with result links and ownership, instead of a flat stream of tool chips.
- Give queued work a reason, a scheduled time, and an obvious cancellation action.
- Offer a small set of selectable panel arrangements before exposing a completely general split editor.

### What to be cautious about

BB's modularity can produce a crowded settings sidebar; the observed installation lists many plugin pages alongside core settings. LUNA should use extension groups and search rather than copy that navigation literally. An older BB README screenshot mentions a dispatch panel and task board, but that screen was not reproduced and a generic “board” search did not establish a current core task-board implementation. Do not promise a BB-style Kanban board on that evidence.

## T3 Code: the inbox and control details are the product

**Observed:** thread navigation includes search, a project filter, snooze, settle, and a separate settled list. The draft screen asks what to build in the selected project. Model, reasoning, permissions, attachments, external editor, project actions, terminal drawer, and right panel are discoverable from the task surface.

The settings experience is particularly useful: it states which projects and environments a change applies to, and small provenance controls explain the effective value. I verified that searching for `glass` returns Glass opacity, and selecting it navigates to Appearance and focuses the relevant row. I also opened a value's provenance popover; it showed the environment inheriting a built-in default. These are actual read-only interaction results, not inferred from source.

### Interesting functionality

1. **Attention has a lifecycle.** Snooze and settle support different intentions: defer until later versus put finished/quiet work away. The current settlement service excludes pending runtime requests, active runs, background tasks, queued turn starts, pinned threads, and explicit settlement overrides from automatic settlement. **Source.** The important behavior is preventing an approval request from disappearing into a quiet shelf.
2. **Settings search targets the actual control.** The index stores a title, destination, row anchor, search aliases, scope, and platform/capability availability. This is much more helpful than filtering only category names. **Observed and source.**
3. **Settings scope is executable, not decorative.** The source resolves project overrides, environment settings, eligible `t3.json` values, and built-in defaults. Write planning can detect mixed values and partial save failures across environments. The UI tells the user when a change saved on some environments but failed on others. **Source; multi-environment writes were not exercised.**
4. **Proactive panels respect manual choice.** At turn completion, a ready diff with at least three files or fifty changed lines can open Changes; linked PRs take precedence. The implementation tracks user-choice revisions, so a later panel choice is meaningful. **Source; the option's UI was observed, automatic opening was not exercised.** The lesson is context-sensitive help that does not continually steal the user's layout.
5. **Queued follow-ups are editable work.** `QueuedRunsControl` presents queue state and supports grip-initiated reorder, edits, and run actions. **Source.** The composer clearly separates queue versus steer defaults and supplies a one-message alternate shortcut. LUNA already supports both behaviors.
6. **Plans can become artifacts.** `ProposedPlanCard` can collapse a long plan, copy/download it, or save it in the workspace. **Source.** LUNA has provider-native review. Its Plan/Build feature is cancelled, so plans stay ordinary chat output or files.
7. **File checkpoints and conversation history are different.** The checkpoint store explicitly handles workspace/staging snapshots and diffs, not provider conversation rollback. **Source.** This is a good clarity rule for LUNA's existing restore-files and rewind features.

### UI ideas worth adapting

- Scope expressed as a sentence: “Applying settings for this project across this environment.” It reads more clearly than unlabeled dropdowns.
- Tiny source badges that expand into an inheritance chain. A value should be explainable without reading a config file.
- Miniatures of real surfaces for theme/font previews: composer mentions, a code diff, terminal output, and a small app wireframe.
- A composer that takes less space while reading history, without losing the draft or focus behavior.
- Separate typography controls only when useful; keep advanced controls folded.
- Search results include the containing settings category, so names like “model” are understandable.

### What to be cautious about

The General page is already very long. LUNA should adopt the settings metadata and search model, then divide settings into meaningful groups. Current upstream source is ahead of the installed app; “source-supported” does not mean a feature was present in the inspected 0.0.44 installation. Several capability-dependent controls can be disabled until the connected environment supports them.

## Conductor: one stream of work with an obvious path to completion

**Observed:** the new workspace explains the source branch, its copied checkout, and optional setup. It provides prompt starters and a right panel combining files, changes, checks, setup, run, and terminal. The screenshot supplied by the user and the fresh accessibility inspection both establish this structure.

The visual language is restrained: a warm dark canvas, muted separators, branch/workspace breadcrumbs, a warm selection underline, and a bounded composer. The distinction comes mostly from coherent hierarchy and useful state, not ornament.

### Interesting functionality

1. **The empty state explains the environment.** “New copy,” origin branch, created directory, and setup status answer the questions a user usually has before asking an agent to edit code. LUNA can show a similarly concrete startup receipt using its existing durable startup and checkout information.
2. **A model loadout reduces choice.** The observed settings screen has a small ordered set of model slots, one default, a provider catalog, a separate review model, and a new-chat plan default. LUNA already has a favorites rail in its model picker and per-provider new-chat defaults; per-action defaults, such as a review model, would complete it. Do not hard-code the inspected model names; provider catalogs change.
3. **Setup, run, and feedback are connected.** Repository configuration documents named run scripts and workspace-specific port ranges. Personal local settings are distinct from reviewed team settings. **Documented; no scripts were configured or run.** [Project configuration](https://www.conductor.build/docs/configure-your-project).
4. **Diff comments become precise agent context.** The documented workflow turns inline comments into composer attachments, and review can be filtered by commit. **Documented; no comments were submitted.** [Diff viewer](https://www.conductor.build/docs/reference/diff-viewer).
5. **Checks is a readiness surface.** The documented page combines Git state, PR metadata, CI, deployments, review threads, and todos, with availability depending on integrations. **Documented; live checks were not exercised.** [Checks](https://www.conductor.build/docs/reference/checks).

### Settings details that are unusually useful

Observed controls include automatic long-paste attachment conversion, context-usage visibility, tool-call expansion, MCP status in chat, a keep-awake policy with a low-battery cutoff, provider executable overrides, terminal scrollback limits, accessible color options, and code/terminal previews. Git settings expose optional-check blocking and archive-on-merge choices. The environment page has variable and env-file configuration with a documented-variable list.

One observed control strips a stereotyped agreement phrase from assistant messages. It is distinctive but a poor first priority for LUNA. If presentation transforms are ever added, retain the raw transcript and make the transform explicit rather than silently changing stored provider output.

### What to be cautious about

Do not turn competitor defaults into LUNA policy. Automatic branch deletion, Git configuration, config synchronization, env copying, and remote local-command permissions each have their own scope. Conductor Cloud and multiplayer are separate product surfaces and were not tested. The similarly named `conductor-oss/conductor` repository is a different workflow-engine project, not source for this desktop app.

## super.engineering: the most ambitious workbench and routing model

**Observed:** the user's screenshot shows a project/worktree sidebar, provider tabs, a large inset conversation canvas, a serif empty-state headline, custom-action affordance, files/changes/review/checks, run-script configuration, and a prominent Git action at the bottom. Fresh inspection also showed a light variant, detailed Settings navigation, preview cards for sidebar styles, action Routing, and Notifications.

### Interesting functionality

1. **The Git action reflects the current condition.** Documentation describes changing actions for local edits, missing PRs, failed checks, unresolved feedback, conflicts, review readiness, and merged work. Agent actions and direct Git/forge operations are distinguished. **Documented; commit/merge actions were not invoked.** [Git actions](https://super.engineering/docs/pull-requests-and-review/).
2. **Actions have reusable routes.** The observed Routing screen has Session default, Fast, Thorough, individual Git/action routes, provider profiles, and explicit launch behavior. Documentation separates instruction templates from model routing. **Observed and documented.** [Custom commands and routing](https://super.engineering/docs/custom-commands-and-routing/).
3. **Layouts belong to the task.** Documentation describes restored tabs, split trees, sidebars, and floating companions, with compact/detailed sidebar options. **Documented; restore behavior was not tested.** [Layout and PiP](https://super.engineering/docs/layout-sidebars-and-pip/).
4. **A notification inbox offers Running, Queue, and Timeline.** It includes peeking, navigation to the source task, and read/dismiss semantics. Its events come from provider lifecycle signals rather than arbitrary terminal text. **Documented; notification delivery was not tested.** [Notifications and approvals](https://super.engineering/docs/notifications-and-approvals/).
5. **One feature can span repositories.** Experimental shared context groups related repositories and materializes a checkout only when needed. **Documented; not enabled or tested.** [Shared context](https://super.engineering/docs/shared-context/).
6. **The browser can supply structured visual feedback.** Documentation describes selecting elements, attaching comments to a chat, and keeping annotations distinct from browser-control permission. **Documented; not exercised.** [Navigation and browser workflows](https://super.engineering/docs/navigation-and-file-workflows/).

### UI ideas worth adapting

- A task-level action area that answers “what is the next useful step?” with a visible reason and secondary choices.
- A reading canvas distinct from the surrounding operational chrome.
- Warm or otherwise recognizable workspace accents, used consistently in the selected project, active tab, and context header. Keep semantic success/failure colors separate.
- Preview cards for layout settings rather than text-only switches.
- Clear configuration of click-to-prepare versus click-to-run. Use labeled choices; a hidden modifier shortcut should be an accelerator, not the sole explanation.
- An empty state with personality, but with practical project actions and context directly available.

### What to be cautious about

A large empty canvas can be attractive and still unhelpful. LUNA should use the space for project-aware starters and recent work. A changing Git button needs fresh state and permission handling; it must not quietly expand a user's authorization. General split-layout machinery and cross-repository branch groups are expensive features. No full public application source was identified, so their architecture remains a documented design rather than a verified implementation pattern.

## The combinations that suit LUNA

**Best functional combination:** T3's attention and settings clarity, Conductor's workspace startup/run/review continuity, super.engineering's action routing and layout identity, and BB's explicit ownership and operator controls.

**Best visual combination:** keep LUNA's native glass sidebar and efficient transcript, add a more informative task header and restrained project identity, make the right panel feel like a deliberate workbench, and give settings searchable, scoped, previewable controls. Preserve a quiet Focus layout for long reading sessions.

**Avoid indiscriminate parity:** LUNA already has side chats, activity, project commands, Git actions, checkpoints, theme import, and many provider controls. The backlog should connect these and address verified gaps. A plugin marketplace, arbitrary layout tree, or a full cloud collaboration product should not precede those improvements.
