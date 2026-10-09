# Evidence and source ledger

Research date: 2026-10-04 (Europe/Bucharest). This is a fresh UI/source/documentation audit with a limited read-only interaction check. It is not an E2E validation of provider execution, a performance benchmark, or a release certification.

## Baselines

| Product | Installed app inspected | Source audited | Boundary |
| --- | --- | --- | --- |
| LUNA | Existing running LUNA Dev | `455caac00993e1a9b9f48785604c349130bbbbb2` | Source and visible settings; bundle/source equivalence not newly built or certified |
| BB | `0.45.0` | [`76a1ebcaf5fa038d8638daaad7aad3dafe2ae709`](https://github.com/get-bb/bb/commit/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709) | Fresh upstream main snapshot; installed version may differ |
| T3 Code (Alpha) | `0.0.44` | [`a21702d0e58ad20263dc965d84472bb6d7eb38c8`](https://github.com/pingdotgg/t3code/commit/a21702d0e58ad20263dc965d84472bb6d7eb38c8) | Fresh upstream main snapshot; app displayed an available 0.0.45 update, not installed here |
| Conductor | `0.90.1` | No full application source identified in this audit | App screens and official docs; `conductor-oss/conductor` is unrelated |
| super.engineering | `c623b42b` | No full application source identified in this audit | App screens and official docs; custom renderer exposed little content through accessibility |

Installed versions were read from the app bundles; T3's version and super.engineering's build identifier were also visible in Settings. Latest upstream snapshots were cloned read-only using SSH without updating or changing the user's existing BB/T3 checkouts.

The existing local checkouts were initially inspected at BB `2573ad5e85835dc1c90aa33795e6dba52ef5aa92` and T3 `5cc99e1c23980d7995a13c47f969b47cb68ed1be`. The source conclusions and pinned links below use the newer snapshots above.

LUNA already had unrelated edits to `docs/pending-tasks/README.md` and an untracked `claude-code-feature-parity/` folder. They were preserved. Research files were added only under `agent-workspace-app-research/`; presentation sketches are stored in the current chat's visualization directory.

## Labels

- **Observed:** visible screen or read-only interaction inspected in this run.
- **Source:** code inspected at the specified immutable revision; not necessarily exercised in the installed app.
- **Documented:** official vendor description; not independently exercised.
- **Proposed:** an original LUNA recommendation, design sketch, or acceptance scenario.

A visible toggle or command establishes UI presence, not that the underlying operation succeeds. A provider model shown by an installed app does not establish entitlement or capability in LUNA.

## BB source map

All BB links below pin the audited source revision. The repository's root LICENSE identifies MIT; copying substantial code would still require preserving its notices. This report copies no implementation.

| ID | Evidence | Source |
| --- | --- | --- |
| BB-01 | Product surfaces and extension-first positioning | [README](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/README.md#L15) |
| BB-02 | Enabled plugins contribute settings navigation | [plugin-settings-entries.ts](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/apps/app/src/components/settings/plugin-settings-entries.ts#L26) |
| BB-03 | Split layout, pane focus, and stored layout updates | [SplitThreadArea.tsx](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/apps/app/src/views/thread-detail/SplitThreadArea.tsx#L273) |
| BB-04 | Inactive-pane dimming is a separate preference | [SplitDimmingSetting.tsx](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/apps/app/src/components/settings/SplitDimmingSetting.tsx#L8) |
| BB-05 | Side chat and return-to-main product contract | [side-chat overview](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/plugins/side-chat/PLUGIN_OVERVIEW.md#L1) |
| BB-06 | Return-to-main queues the reply and records sender thread | [useSendSideChatMessageToMain.ts](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/apps/app/src/views/thread-detail/useSendSideChatMessageToMain.ts#L19) |
| BB-07 | Optional workflow cards, phases, workers, and limits | [workflows overview](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/plugins/workflows/PLUGIN_OVERVIEW.md#L1) |
| BB-08 | Workflow runtime entry and constrained evaluation | [runtime.ts](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/plugins/workflows/src/runtime.ts#L575) |
| BB-09 | Semantic call-cache key and successful-result reuse | [cache.ts](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/plugins/workflows/src/cache.ts#L170) |
| BB-10 | Workflow limits and settings registration | [settings.ts](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/plugins/workflows/src/settings.ts#L74) |
| BB-11 | Host/global admission and waiting-turn semantics | [concurrency overview](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/plugins/concurrency-limit/PLUGIN_OVERVIEW.md#L1) |
| BB-12 | Retry classification, scheduling, limits, and cancellation | [retry overview](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/plugins/provider-retry/PLUGIN_OVERVIEW.md#L1) |
| BB-13 | Core settings destination list | [settings-sections.ts](https://github.com/get-bb/bb/blob/76a1ebcaf5fa038d8638daaad7aad3dafe2ae709/apps/app/src/components/settings/settings-sections.ts#L5) |

## T3 source map

The repository's root LICENSE identifies MIT. Source structure changed since the older local checkout: current settlement logic is under `orchestration-v2`, so older path references were not reused.

| ID | Evidence | Source |
| --- | --- | --- |
| T3-01 | Search metadata, aliases, scope and availability | [settingsSearch.ts](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/web/src/components/settings/settingsSearch.ts#L29) |
| T3-02 | Setting inheritance layers | [SettingInheritance.tsx](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/web/src/components/settings/SettingInheritance.tsx#L87) |
| T3-03 | Scoped writes, mixed state, partial failures | [useScopedSettings.ts](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/web/src/components/settings/useScopedSettings.ts#L38) |
| T3-04 | Resolver behind UI provenance | [projectSettings.ts](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/packages/shared/src/projectSettings.ts#L18) |
| T3-05 | Proactive PR/diff decision and manual-choice observation | [ChatView.logic.ts](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/web/src/components/ChatView.logic.ts#L117) |
| T3-06 | Wiring proactive decisions into the task view | [ChatView.tsx](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/web/src/components/ChatView.tsx#L5256) |
| T3-07 | Proactive panel control and other behavior settings | [SettingsPanels.tsx](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/web/src/components/settings/SettingsPanels.tsx#L2680) |
| T3-08 | Auto-settle protects blocked, running and queued work | [ThreadSettlementService.ts](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/server/src/orchestration-v2/ThreadSettlementService.ts#L133) |
| T3-09 | Workspace checkpoints distinct from provider rollback | [CheckpointStore.ts](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/server/src/checkpointing/CheckpointStore.ts#L1) |
| T3-10 | Editable/reorderable queued run UI | [QueuedRunsControl.tsx](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/web/src/components/chat/QueuedRunsControl.tsx#L48) |
| T3-11 | Plan collapse and copy/download/save affordances | [ProposedPlanCard.tsx](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/web/src/components/chat/ProposedPlanCard.tsx#L37) |
| T3-12 | Composer focus versus scroll collapse state | [useComposerFocusState.ts](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/web/src/components/chat/useComposerFocusState.ts#L3) |
| T3-13 | Realistic miniature of app roles | [ThemeWireframe.tsx](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/web/src/components/settings/ThemeWireframe.tsx#L4) |
| T3-14 | Retention/cleanup preferences and capability checks | [StorageSettings.tsx](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/web/src/components/settings/StorageSettings.tsx#L133) |
| T3-15 | Repository conventions and separate writing model | [SourceControlWritingSettings.tsx](https://github.com/pingdotgg/t3code/blob/a21702d0e58ad20263dc965d84472bb6d7eb38c8/apps/web/src/components/settings/SourceControlWritingSettings.tsx#L134) |

## LUNA source map

Links are pinned to the inspected baseline `455caac00993e1a9b9f48785604c349130bbbbb2`; line numbers in the current tree have moved since.

| ID | Existing foundation | Source |
| --- | --- | --- |
| LU-01 | Settings section inventory and fixture-only routing | [shell.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/shell.rs#L439) |
| LU-02 | Settings navigation implementation | [shell.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/shell.rs#L8143) |
| LU-03 | Device-local behavior, theme, panes and file preferences | [settings.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/settings.rs#L725) |
| LU-04 | Queue/steer, retries, sleep and confirmations | [general.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/settings/general.rs#L149) |
| LU-05 | Appearance/theme/font/glass/width controls | [appearance.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/settings/appearance.rs#L3348) |
| LU-06 | One resolver for project new-task defaults | [project_defaults.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/settings/project_defaults.rs#L24) |
| LU-07 | Named commands, setup flags, preview and durable run UI | [project_commands.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/project_commands.rs#L217) |
| LU-08 | Actions and run receipts contract | [project_commands.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/proto/src/project_commands.rs#L1) |
| LU-09 | Project command execution service | [project_commands.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/engine/src/project_commands.rs#L52) |
| LU-10 | Activity presentation and notification delivery | [activity_center.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/shell/activity_center.rs#L1) |
| LU-11 | Engine-owned attention reconciliation | [attention.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/engine/src/attention.rs#L151) |
| LU-12 | Related chat ownership and presentation | [related_chats.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/shell/related_chats.rs#L1) |
| LU-13 | Sidebar settlement behavior and contextual preferences | [spaces.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/shell/spaces.rs#L1135) |
| LU-14 | Git actions and file-checkpoint restore UI | [git_actions.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/changes/git_actions.rs#L852) |
| LU-15 | Provider-native review surface | [native_review.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/shell/native_review.rs#L1) |
| LU-16 | Provider skills/MCP catalog controls | [harnesses.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/settings/harnesses.rs#L960) |
| LU-17 | Sounds, desktop policy and task-specific choices | [notifications.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/settings/notifications.rs#L233) |
| LU-18 | Autosave and file visibility preferences | [files.rs](https://github.com/novnski/luna/blob/455caac00993e1a9b9f48785604c349130bbbbb2/crates/ui/src/settings/files.rs#L133) |

## Official documentation

These sources were read in this run. Links in the findings identify the particular supporting page. Documentation claims remain untested unless separately labeled observed/source.

### Conductor

- [Introduction](https://www.conductor.build/docs)
- [Workflow](https://www.conductor.build/docs/concepts/workflow)
- [Configure your project](https://www.conductor.build/docs/configure-your-project)
- [Diff viewer](https://www.conductor.build/docs/reference/diff-viewer)
- [Checks](https://www.conductor.build/docs/reference/checks)

### super.engineering

- [Introduction](https://super.engineering/docs/)
- [Layout, sidebars and PiP](https://super.engineering/docs/layout-sidebars-and-pip/)
- [Themes and appearance](https://super.engineering/docs/themes-and-appearance/)
- [Shared context](https://super.engineering/docs/shared-context/)
- [Settings reference](https://super.engineering/docs/settings-privacy-local-data/)
- [Custom commands and routing](https://super.engineering/docs/custom-commands-and-routing/)
- [Git actions and pull requests](https://super.engineering/docs/pull-requests-and-review/)
- [Notifications and approvals](https://super.engineering/docs/notifications-and-approvals/)
- [Navigation and browser workflows](https://super.engineering/docs/navigation-and-file-workflows/)

## Saved screen evidence

PNG files are fresh captures from the installed apps. Text files are accessibility readbacks where available. For super.engineering, the renderer supplied little AX content, so screenshots establish the observed state. UI evidence is stored under [evidence/](evidence/).

| Group | Files | Established observation |
| --- | --- | --- |
| BB | `bb-home`, `bb-settings`, `bb-appearance`, `bb-files` | Composer context; core/plugin categories; interface customization; per-extension openers |
| T3 | `t3-home`, `t3-general`, `t3-appearance`, `t3-source-control`, `t3-storage` | Thread navigation; behavior, appearance, writing and retention controls |
| T3 search | `t3-settings-search`, `t3-settings-search-destination` | Query `glass` returned the row; selection opened Appearance and focused Glass opacity |
| T3 provenance | `t3-inheritance` | Current model default showed an environment/built-in inheritance chain |
| Conductor | `conductor-settings`, `conductor-appearance`, `conductor-models`, `conductor-git`, `conductor-environment`, `conductor-advanced` | Settings groups, model loadout, Git policies, env configuration, executable and scrollback controls |
| super.engineering | `super-settings`, `super-settings-tab`, `super-routing`, `super-notifications` | Visual settings previews, global/workspace/project routes, delivery and queue retention controls |
| LUNA | `luna-settings`, `luna-appearance`, `luna-projects` | Current visible controls and existing project command entry |

The first `conductor-settings.png` was captured after opening Settings; fresh navigation readback confirmed its General page. The `super-settings-tab.png` capture is the General page. Source line/link and evidence-hash verification are recorded in `evidence/verification.json`.

## What was not done

No agent prompts, shell/run scripts, Git actions, provider calls, settings writes, account changes, permission grants, updates, or paid/cloud workflows were triggered in the inspected products. Navigation, search, and opening a provenance popover are the only exercised interactions. No LUNA runtime code was changed, no app was rebuilt, and no commit/push was performed. Proposed E2E scenarios are future acceptance criteria.

Prior parity notes were used to find established research conventions. They were not treated as confirmation of current runtime behavior. All LUNA capability mappings above were refreshed against current source or visible controls.
