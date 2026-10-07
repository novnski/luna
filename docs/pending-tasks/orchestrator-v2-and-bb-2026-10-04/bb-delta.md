# BB: changes since the previous harness audit

Research date: 2026-10-04. Local source: /Users/iulian/workspace/code/personal/bb. Fast-forwarded from 2573ad5e85835dc1c90aa33795e6dba52ef5aa92 to **a2bbfaadaa91084cde68caa06954c8927f0015a4**. The checkout was clean before and after. Installed **BB 0.45.0** was opened and exercised.

The harness baseline was **f1172452bb0ca60c042ce884b3f8b98112fe377d**: **196 commits** to this snapshot. Today's broader UI audit had already examined **76a1ebcaf5fa038d8638daaad7aad3dafe2ae709**: only **four commits** came after it.

**BB has not introduced an equivalent new orchestration rewrite in this range.** Its important additions are provider correctness, immediate persistent forks, richer execution options, automation/prompt controls, platform support, and a substantial set of performance/reliability improvements.

## The most important source findings

### 1. Permission changes during steering are real runtime transitions

[Codex bridge][codex-steer], commit cd12e7a9, remembers the effective permissions used for the running turn. When a steer arrives with different permissions, it verifies the expected turn is still active, interrupts it, waits for settlement, and starts the next turn with the new settings. It does not pretend turn/steer itself changes the running sandbox.

The [Claude bridge][claude-permissions], commit f336aa01, updates permission mode, scope and write roots, clears prior session grants, rebuilds sandbox and bypass settings, and marks the resident runtime for restart before the next turn. If a permission change accompanies a steer, it uses the new-turn path. It also preserves the post-plan permission mode separately from the current plan mode.

Three earlier fixes in this range are valuable regression cases:

- 94d704e7: preserve Full Access after plan approval.
- 96c40f01: respect explicit Claude permissions.ask matches.
- 4502eab4: Bash “allow for session” must not accidentally authorize every command.

These do not prove Luna has those defects. Luna already has permission capabilities, explicit mode transitions and native permission-suggestion handling. Use the scenarios to verify the equivalent paths rather than copy BB's policies. In particular, Luna's Ask/Auto semantics need to remain its own product contract.

### 2. Native Codex questions now have an actual answer path

Commit **45439241** removed BB's forced disabling of the native default-mode question feature and added decoding/rendering/answer translation for native choice questions.

The [decoder][questions] preserves question IDs, creates stable option values, accepts free text and translates the selected option back to the label expected by Codex. The bridge now distinguishes a user-question resolution from an approval resolution; routing every callback through the approval schema would reject the former.

There is a qualification: [server.ts][question-flag] still declares supportsNativeUserQuestion=false. That is relevant to BB's generic replacement-tool/plugin capability behavior. The correct conclusion is **native requests are handled when the installed Codex configuration emits them**, not “every BB Codex session always exposes the native question tool.” No native question was generated during this live probe.

Luna already has native question handling, including an explicit secret-input path that avoids durable logging. Preserve that stronger behavior when harmonizing schemas.

### 3. Forks now exist before the first prompt

[useForkThreadFromMessage][fork-ui] resolves execution defaults, reuses the source environment, sends a spawn request with an empty input array and source checkpoint information, and navigates only after the fork creation result. An in-flight guard prevents duplicate clicks. The [server][fork-server] derives a title from the source when the caller did not provide one.

**Verified live:** a projectless Codex source thread returned a marker. “Fork into new thread” immediately opened another persisted thread named “(1) LUNA BB harness research probe,” with a “Forked from” link and copied visible history. Before any follow-up prompt, the new thread already had a stable ID. A follow-up recovered the original marker.

Independent CLI readback reported:

| Field | Observed value |
| --- | --- |
| Source thread | thr_sin3dht5et |
| Fork thread | thr_pinm6gsbmv |
| Origin kind | fork |
| Source thread ID | thr_sin3dht5et |
| Parent thread ID | null |
| Lifecycle owner thread ID | null |
| Provider | codex |
| Status after reply | idle |

This is a useful modeling distinction: fork ancestry, agent parentage and lifecycle ownership are different relationships. Luna already models those distinctions in TaskLineage and ExecutionBinding. It should preserve them while its persistent-fork UI work is in progress.

### 4. Model-specific service tiers are data, not global switches

Commit **7e139289** added provider-defined tiers constrained by the selected model. [resolveServiceTierOptions][tiers] intersects provider declarations with model support, accepts model-specific labels/descriptions and reconciles an unsupported selection back to the default.

Codex maps its native priority name to BB's historical fast value and can expose additional tiers such as Ultrafast. The choice travels through CLI, app, SDK, automation/task presets, server execution resolution and host-daemon protocol. A picker-only implementation would be incomplete.

Luna's current Codex adapter already reads serviceTiers and additionalSpeedTiers, normalizes priority, exposes model options, and passes the selected tier to native requests. The right action is to test propagation through child creation, forks and warm-session transitions—not add another fixed fast/ultrafast enum.

### 5. Performance improvements preserve semantic boundaries

The most transferable BB work is not a CSS tweak:

- [Timeline service][timeline] reads bounded segments; defaults to 20, caps at 100, and has a 4 MiB event-data response budget.
- [Conversation outline cache][outline] stores incremental checkpoints with DB data version, rewrite generation, context boundary, ordering boundary and maximum sequence.
- Cache reuse is intentionally conservative around cross-turn state such as nested tools, delegation and background tasks.
- The outline cache has both an entry limit (16) and a character budget (8,000,000); it is not an unbounded map.
- Recent changes retain a few reopened timelines for delta refresh, pause hidden split-pane queries, target history-compaction updates only at viewers, avoid reparsing settled messages, reuse root ordering during child streaming, and reduce repeated daemon liveness writes.
- Workflow progress is driven by signals instead of a one-second refresh loop.

Representative commits: 48aa8f11, dc683dbd, 9de1633d, 6840d63f, c8b9459b, 36aacc04, 1d7e1dc3.

Luna already virtualizes transcript rows and incrementally parses live Markdown. That does not establish that host history reads, outline projections, pane subscriptions and reopen paths are bounded. Profile those paths with large conversations and nested child activity before adding another cache.

### 6. Composer actions have an owner

Commits **68cd6165** and **315107cd** added shared popup slots and routed plugin commands through the same owner-aware composer path as built-in commands.

The [SDK contract][composer-contract] selects the composer holding the caret, otherwise the focused pane's primary composer. It disables the shortcut when focus belongs to a terminal, browser or modal. [PluginComposerCommands][composer-handler] passes the bound composer to a namespaced handler and contains both synchronous and asynchronous failures.

This is directly relevant to Luna's current multi-pane work. Even without plugins, “send,” model picker, slash command, attachment and history actions must target one explicit composer/chat instance. Global focus assumptions can send a correct request to the wrong conversation.

### 7. Prompt history and automation refinements are distinct from saved drafts

Commit **459bc8cc** added a paged cross-thread prompt-history API, CLI and SDK surface. The [service][prompt-history] combines relevant accepted/queued history, filters visibility, and encodes an opaque ordering cursor; [CLI][prompt-cli] exposes cursor and limit. Luna's prompt stash is a deliberate saved-draft collection, not an equivalent automatic history index.

Scheduled-send choices now persist, with short 5/10-minute presets. BB's automation scheduler has explicit timezones and both cron and one-shot schedules. [schedule-helpers][schedule] validates timezone and five-field cron input; [sweep][sweep] claims a due execution using the expected next-run timestamp before dispatch. DST regression coverage was updated in **49fe15af**.

**Do not count the new plugin timeline event architecture as shipped in this source snapshot.** Commit 4d15c1da introduced it, and **75308f40 reverted it** immediately afterward. The earlier compact automation-prompt rows remain a separate change.

No scheduled work was created in the live app.

### 8. Provider availability and account fallback

Provider enable/disable preferences are server-resolved and available through Settings, CLI and SDK (a7562b95). The [registry][provider-registry] separates registration, user-disabled state and host-specific installation/health caching.

The optional Account Pooler gained extra-usage availability/fallback handling for Claude and Codex (41b9f1f6). [Quota interpretation][quota] preserves observed status, source and timestamps, rather than deriving all availability from a single percentage. Missing usage is not equivalent to exhausted or free capacity.

For Luna, show installed, enabled, authenticated, available and selected as separate facts. Automatic paid fallback should be an explicit policy decision, not a side effect of a misleading “available” label. This audit did not enable pooling or purchase extra usage.

## Complete feature-family inventory

The raw [196-commit range][commits] is retained so future implementation can inspect the full delta. Existing features from 0.44 and before are not relabeled as new just because 0.45 mentions them.

| Family | Recent changes | Recommendation for Luna |
| --- | --- | --- |
| Provider control | Enable/disable, model-tier constraints, native questions, warm permission updates | Highest-value contract and E2E work |
| Claude correctness | Ask rules, session grants, plan exit permissions, optional sandbox, 1M context telemetry | Reuse failure cases; keep Luna semantics |
| Other providers | Fresh Pi catalog, host-model validation, replay-safe catalog probes, OpenCode v2 credentials/questions, Hermes steer recovery | Per-provider compatibility probes |
| Persistent forks | Eager creation, inherited execution options, source-derived title | Compare with current persistent-fork work; do not duplicate it |
| Performance | Bounded history, cached outlines, delta reopen, selective subscriptions, fewer liveness writes | Profile at scale before implementing |
| Composer/extensions | Popup host, owner-aware commands, consumers off the typing path | Adopt ownership contract without requiring a plugin marketplace |
| Prompt workflow | Global paged history, remembered delayed sends, one-tap dictation Send | History is useful; drafts already exist in Luna |
| Cloud AI | BB account-based titles, commit messages and voice; automatic routing | Borrow separate service routing; no reason to make Luna depend on BB cloud |
| Account pooling | Extra-usage states and fallback, family-level quota observations | Later optional feature; explicit cost/account policy |
| Environments | Machine storage detail, explicit unused-environment cleanup | Useful lifecycle UI; preserve live/stopping workspace guard |
| Thread/pane UX | Restore closed panels in order, per-tab sidebar layout, fullscreen command, task-draft retention | Matches Luna workbench direction |
| Tool/message display | Stable touch/desktop actions, timestamps in menus, literal user HTML, contextual link handling | Small quality improvements |
| Plugin system | Bundled-default inheritance, marketplace distinction, removal of old loader, explicit composer APIs | Learn lifecycle boundaries; defer full ecosystem |
| Automation | DST fixes, remembered schedules, compact prompt rows | Build app scheduler only after durable execution policy |
| Connect/remote | Revocable sessions, seven-day session lifetime, jitter, resumed-socket probes, response ownership, cookie isolation | Relevant to Luna remote work; not required for local harness improvements |
| Platform support | Native Windows daemon/CLI/app/terminal/hooks and Windows CI; Android improvements | Separate platform roadmap |
| Browser/files | Downloads, direct streamed downloads and sampled previews, profile reuse | Useful when expanding Luna browser/file workspace |
| Notifications/mobile | Push opt-in, context-specific navigation, long-question scrolling | UI follow-on after core lifecycle tests |
| Packaging/maintenance | Update errors, npm version handling, CI/cache and installer fixes | Port only bugs reproduced in Luna's own platform paths |

## What is new after today's UI snapshot?

Only these four commits are additional:

1. **c201f649** — pass the full task to the title service.
2. **4d15c1da** — introduce automation runs as plugin timeline events.
3. **75308f40** — revert that introduction.
4. **a2bbfaad** — show message timestamps as menu annotations.

The large BB backlog above is the delta from the earlier harness review. Most of it was already present in the source used by today's settings/UI research. The live fork and CLI checks here are fresh evidence rather than a second claim of new features.

## Adoption decision

Borrow the active-permission transition logic as a test model, owner-aware composer routing, careful fork lineage, capability-driven model options, incremental history invalidation and visible recovery states.

Do not copy a React/TanStack cache into GPUI, adopt BB's cloud as a required dependency, import its whole plugin ABI, or assume its newly added native question path eliminates every capability mismatch. Luna can implement the useful contracts inside its existing Rust engine and document/UI architecture.

[codex-steer]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/plugins/provider-codex/src/bridge/bridge.ts#L1916
[claude-permissions]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/plugins/provider-claude-code/src/bridge/bridge.ts#L488
[questions]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/plugins/provider-codex/src/interactive-requests.ts#L99
[question-flag]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/plugins/provider-codex/server.ts#L43
[fork-ui]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/apps/app/src/hooks/useForkThreadFromMessage.ts#L23
[fork-server]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/apps/server/src/services/threads/thread-create.ts#L693
[tiers]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/packages/domain/src/service-tier.ts#L28
[timeline]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/apps/server/src/services/threads/timeline.ts#L163
[outline]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/apps/server/src/services/threads/conversation-outline-cache.ts#L218
[composer-contract]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/packages/plugin-sdk/src/app-contract.ts#L1844
[composer-handler]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/apps/app/src/components/plugin/PluginComposerCommands.tsx#L16
[prompt-history]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/apps/server/src/services/prompt-history.ts#L179
[prompt-cli]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/apps/cli/src/commands/prompt-history.ts#L48
[schedule]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/plugins/automations/src/schedule-helpers.ts#L1
[sweep]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/plugins/automations/src/sweep.ts#L92
[provider-registry]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/apps/server/src/services/providers/provider-registry.ts#L59
[quota]: https://github.com/get-bb/bb/blob/a2bbfaadaa91084cde68caa06954c8927f0015a4/plugins/account-pool/src/quota.ts#L26
[commits]: evidence/bb-since-parity-20261004.txt
