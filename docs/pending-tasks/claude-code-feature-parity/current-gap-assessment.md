# Claude Code parity reassessment, 2026-10-01

LUNA has shipped the shared foundation for most of the earlier Codex parity work. The remaining Claude work is substantially more about connecting the provider to that foundation than building another composer, child-chat system, approval wizard, output viewer or goal card.

However, the original Claude adapter gaps remain. Its transport, result decoder, event normalizer and discovery policy have barely changed. This audit also found a concrete configuration mismatch in Ask/Auto and additional completion/agent-observability risks that should move ahead of cosmetic parity work.

This document supersedes the old completion assumptions and implementation ordering. The original inventory and CC-00–CC-18 package identifiers remain useful scope references. Source findings, a native initialization observation and inherited QA reports are distinguished below; this is not full Claude runtime certification.

## Freshness and evidence

| Repository | Original audit pin | Inspected main after refresh | Commits since original pin | Refresh outcome |
| --- | --- | --- | ---: | --- |
| LUNA | `22a6b236` | `d0f12bcf6f01d719656eac4288483eeb05c2e347` | 116 | SSH fetch and fast-forward check; already current with `origin/main` |
| BB | `f1172452` | `2573ad5e85835dc1c90aa33795e6dba52ef5aa92` | 72 | `git pull --ff-only origin main`; already current |
| T3 | `8792f957` | `5cc99e1c23980d7995a13c47f969b47cb68ed1be` | 32 | `git pull --ff-only origin main`; fast-forwarded 32 commits |

Commit counts include branch history; they are not counts of new features. BB had already advanced locally before this turn. T3's pre-existing untracked `docs/research/` and LUNA's existing pending-task drafts were preserved. All three local heads matched the fetched `origin/main` at the recorded checkpoint.

Installed Claude is **2.1.286**. T3 still resolves Agent SDK **0.3.276**; BB still resolves **0.3.245**. Upgrading to an SDK is not itself a parity solution. Their useful behavior comes from their typed input, controls, native history, event translation and scoped tool bridges.

The [new baseline](reassessment-baseline.json) records commits, inspected file hashes, CLI identity and verification limits. Retained evidence and a second copy of the docs are under:

`/Users/iulian/workspace/code/personal/luna-feature-research/2026-10-01-claude-code-parity-refresh/`

Verification performed here: current source and Git delta analysis, selected current official documentation, CLI version/help, and three isolated initialization handshakes. No model prompt, native tool execution, Rust build, feature E2E rerun or GUI/app operation was performed. The existing app was left running as it was. No source implementation, commit, push or new task was created.

## What has actually changed in LUNA

The major merge is `e583d52a`, followed by the goal-card/child-pane integration at `0991dab8`. The ten features in the earlier handover are now in main. The old paused handover's initial table is therefore stale; its appended 2026-10-01 outcome and final QA rechecks are the relevant delivery record.

| Merged feature | Reusable foundation now in main | Remaining Claude connection |
| --- | --- | --- |
| Native images and typed composer inputs | Durable typed selections, staging, draft/queue ownership and Codex image dispatch | Claude validation/content dispatch; typed selections currently reject non-Codex providers |
| Installed plugin/app/skill picker | Scoped catalog cache, native chips, provenance and accessible retry | UI and RPC discovery explicitly require Codex; add native Claude roots/identities and dispatch |
| Full tool output and activity | Expandable output, host journal retrieval and bounded transcript previews | Claude still discards result bodies; mixed rich content also needs a richer envelope |
| Steering status | Durable queue states and stronger native correlation for Codex | Claude still reports stdin acceptance without confirmed incorporation |
| Native review | Review targets, durable intent, lifecycle and findings presentation | Engine explicitly requires Codex; native Claude review/findings need their own route |
| MCP forms and URL prompts | Shared schemas, input wizard, URL flow and engine-owned pending requests | Claude emits `mcp: None` and has no native elicitation/dialog dispatcher |
| Permission details | Shared approval detail contracts, choices and UI | Claude emits `details: None`, loses option descriptions and lacks newer cancellation/control coverage |
| Same-chat rewind | Durable preview/reconciliation and transcript projection | Native operation explicitly requires Codex; Claude native fork/rewind remains absent |
| Goals and continuation | Goal state, controls, command and redesigned card | Engine and UI require Codex; Claude goal condition/evaluator/lifetime differ |
| Persistent child chats | Durable creation/startup/outbox, lineage, manual creation and a live right pane | Manual shared operation is reusable; agent-callable creation is still gated on `CodexDynamicTools` |

Evidence: [typed-input admission][L-inputs], [native picker UI][L-picker-ui], [catalog RPC][L-picker-rpc], [tool-result contract][L-tool-contract], [Claude approvals][L-approvals], [review admission][L-review], [rewind admission][L-rewind], [goal admission][L-goals], [child authority][L-child-authority].

The prior coordinator's [QA report](/Users/iulian/workspace/code/personal/luna-feature-checkouts/2026-09-30-codex-parity/reports/qa-parity.md) includes real Codex runs and final corrections for review settlement, post-rewind continuation and child-creation error reporting. These are inherited evidence for the merged Codex/shared implementation. They do not prove the corresponding Claude paths. The child-chat acceptance script uses a synthetic provider for its durable engine scenarios, as its [QA guide](/Users/iulian/workspace/code/personal/luna/docs/qa/child-chats.md) explicitly states.

The Claude production-source delta is especially small: `claude/mod.rs` only clears the new typed inputs for title runs, adds `user_message_id: None` to the existing warm boundary, and supplies `mcp: None`/`details: None` for the expanded shared structures. `claude/wire.rs`, `claude/normalize.rs` and `claude/catalog.rs` are unchanged from the original audit pin. None of the merged Codex adapter functionality should be assumed to have transferred automatically.

## Corrections to the original assessment

**Follow-up images are not universally lost.** Claude's warm mailbox is text-only, but the engine now declines that route when a non-Codex request carries attachments. A normal follow-up can therefore launch a fresh process with native resume and inline images. Mid-turn image steering is explicitly rejected. The actual gap is consistent validation and delivery across cold/resumed/warm/steer paths, including the silent skip in the image loader; it is not “Claude cannot receive an image after message one.” [Warm routing][L-warm], [steer rejection][L-steer], [image loading][L-images].

**Claude already has some integration and usage infrastructure.** Shared integration settings scan Claude global/project skill folders. Shared Usage can parse native Claude transcript cache/cost fields and map live quota reset windows. These are existing assets, including code predating this merge. The narrower missing work is effective selected-session inventory, typed native invocation, live per-turn/task accounting, warnings and controls. Do not recreate the settings or Usage pages. [Claude skill inventory][L-skill-inventory], [transcript usage][L-usage], [quota windows][L-limits].

**Manual app children and native subagents are different.** The shared manual child operation has no blanket Claude-provider exclusion and can preserve a same-provider account, subject to project/global defaults matching the parent. Its engine startup/outbox/pane can be reused. This audit did not run a real Claude manual-child scenario. Claude calling `create_thread` itself remains unavailable; native Claude Agent/Task children are a third mechanism. [Manual child entry][L-child-manual], [provider selection][L-child-selection], [callable gate][L-child-authority].

**Shared delivery has progressed; certification has not transferred.** All ten shared/Codex features are in main, so earlier “unmerged/WIP” assumptions should be retired. No CC package can be marked fully Claude-certified from that merge or from this source audit. Several packages can now be narrowed to an adapter integration plus provider-specific acceptance.

**The original permission-flag migration note needs correction.** Current installed help documents `--permission-prompts` targets as `host` or `none`, matching the current official reference. The older audit's proposed `--permission-prompts stdio` spelling should not be used as a migration instruction. LUNA's existing `--permission-prompt-tool stdio` successfully completed all three initialize handshakes here; that proves accepted initialization syntax, not full approval behavior. Keep transport selection and host-versus-deny policy separate. The old baseline JSON remains historical rather than an executable compatibility recipe. [CLI reference](https://code.claude.com/docs/en/cli-reference).

## Remaining gaps, ranked

### P0: resolve before claiming dependable everyday Claude parity

**G1. Ask/Auto configuration and tool policy differ from discovery.** LUNA launches Ask with `--restricted --permission-mode acceptEdits`, and Auto with `--restricted --permission-mode auto`. Current native documentation says restricted mode omits user/project settings and removes command-running built-ins and WebFetch unless explicitly selected. LUNA does not add a normal-run `--tools` allowlist. This is a compatibility contract, not simply an approval-label issue. [Launch policy][L-policy], [CLI reference](https://code.claude.com/docs/en/cli-reference).

The installed CLI initialization probe used an isolated project with `outputStyle: Explanatory` and a harmless local slash command. Both LUNA mode combinations returned `output_style: default`, omitted that command, and advertised 49 commands. The same isolated initialization without `--restricted` returned `Explanatory`, included the command, and advertised 50. All three exited 0; no user message or model/tool execution occurred. See `initialize-probe-results.json` and the repeatable `probe-initialize.py` in the retained evidence directory.

LUNA's `commands()` discovery uses a separate unrestricted initialize process with no requested task cwd, and caches success in a harness-wide `OnceCell`. Therefore the discovery and actual-run configurations can disagree; a project/account switch can also reuse the wrong catalog. The probe confirms settings/command omission, while the command-tool restriction is supported by current documentation and source, not an executed Bash test. [Discovery implementation][L-discovery], [cache][L-discovery-cache].

Remaining work: define effective Ask/Auto semantics for supported CLI versions; align discovery with actual cwd, provider home, settings and policy; expose omissions truthfully. Preserve the permission boundary when choosing explicit settings/tool grants or a different verified native policy. Silently removing the flag would be an insufficient fix.

**G2. Claude result bodies still disappear before the journal.** `ContentBlock` has no nested result `content`; both root and child `tool_result` branches emit `output: None` and `diff: None`. Shell output, read content, edit results, web evidence, notebook changes and MCP results therefore cannot feed the new expandable-output UI. Existing upstream execution does not repair the display/persistence loss. [Wire][L-content], [root results][L-results], [child results][L-child-results].

Remaining work: preserve complete received result content and metadata at decode/normalization; feed the existing journal/lazy-fetch path; distinguish an empty result from unavailable output. Handle envelope `tool_use_result` and block results without duplicating them. Capture results once rather than rerunning tools. Text/diff support can reuse the merged foundation; richer assets require G10 below.

**G3. Root completion does not distinguish another native turn's result.** LUNA's outbound user frames do not assign native message UUIDs, `ResultFrame` retains neither `origin` nor `user_message_uuid(s)`, and every root result is normalized into root usage and Done. A resumed native background task, peer wakeup or native command can produce its own turn before a queued human prompt runs. The adapter lacks the evidence needed to tell those results apart. This is a strong source-supported risk, not a reproduced LUNA failure in this audit. [Input frame][L-user-frame], [result frame][L-result-frame], [completion][L-completion].

T3 just fixed this in `9da066dbe`: it compares echoed UUIDs and origin with the active user turn before settlement. BB also checks origin before a response has begun, though its inspected check is narrower than T3's UUID matching. Remaining work: carry native identities/origin through admission, normalization and queue settlement; version-gate receipt support and preserve uncertainty for older runtimes. A successful stdin write must remain distinct from incorporation. [T3 correlation][T-correlation], [BB origin guard][B-origin].

**G4. No callable LUNA tool transport for Claude.** Claude still advertises `ClaudeInstructionsOnly` and rejects any runtime contract with tools. The catalog and child-authority checks require `CodexDynamicTools`. Consequently Claude cannot call LUNA's read/wait/send/create-thread operations through this app-owned interface, even though shared infrastructure and persistent child UI now exist. External native MCP connections are a separate mechanism. [Adapter gate][L-runtime], [catalog gate][L-tool-gate], [child authority][L-child-authority].

Remaining work: add a verified session-scoped callable bridge and generalize the authority/capability checks deliberately. Reuse the existing engine fences, receipts, idempotency and outbox. T3's authenticated per-session HTTP MCP and BB's SDK MCP tool proxy are useful references. Reuse existing Executor connections for external integrations; registering another global gateway is not required by this gap. [T3 bridge][T-bridge], [BB proxy][B-proxy].

**G5. Rich approvals, dialogs and cancellation are not wired.** Only `can_use_tool` has a dispatcher; unknown control requests are logged and abandoned. Other frame families, including cancellation, become `Other`. Claude's shared approval payload has `details: None`, its questions have `mcp: None`, and option objects are reduced to labels. The new form/URL wizard cannot receive the native data it needs. [Dispatcher][L-controls], [approval payload][L-approvals], [question decoder][L-questions], [unknown frames][L-unknown].

Remaining work: correlated one-terminal-response dispatch, native cancellation/abort support, full command/path/reason/suggestion context, descriptions and Claude form/URL/dialog mapping. Test deny/cancel/timeout/late response and unsupported requests through the engine-owned input path. T3 exposes both `canUseTool` and `onUserDialog`; BB's interactive contracts are useful mappings. Preserve the product decision excluding Plan/Build UI.

**G6. Image and native-selection input still has provider holes.** The cold/resumed loader silently skips unreadable, over-cap or unsupported images. Claude does not use the new Codex-specific admission validator. Direct warm serialization is text-only; engine steering rejects image/typed-input messages for Claude. Typed plugin/app/skill selections reject Claude even on cold dispatch. A formatter or native chip alone cannot make these operations work. [Image loader][L-images], [warm routing][L-warm], [steering][L-steer], [typed admission][L-inputs].

Remaining work: provider-aware validation and ordered input content across every admitted route; explicit recoverable rejection; selected-host file ownership; Claude-native skill expansion and integration identity. Do not import Codex's macOS TIFF policy into Claude's inline PNG/JPEG/GIF/WebP contract. T3's all-turn content builder and last-text-block slash dispatch are the strongest references. BB still turns input images into text/path markers, so its SDK integration is not evidence for vision parity. [T3 input][T-input], [T3 skill dispatch][T-skills], [BB input][B-input].

### P1: connect native observability and shared advanced features

**G7. Native agent/task/hook visibility is narrower than the new UI.** Basic tagged child content and background Agent/Task completion remain supported. But LUNA does not request `--forward-subagent-text` or `--include-hook-events` on normal launch. Foreground/nested transcript visibility therefore depends on external configuration. Current native docs document these opt-ins. Adding flags alone would be incomplete: the current wire/system decoder still lacks progress, workflow membership, output-file, hook and model/usage detail. [Launch][L-launch], [system fields][L-system], [CLI reference](https://code.claude.com/docs/en/cli-reference).

There is also a specific nested ownership risk: LUNA's tagged assistant branch returns before the root branch records Agent/Task spawn IDs. A spawn seen only in that tagged snapshot is not registered there. A qualifying `task_started` with `subagent_type` does register it, so the existing fallback must be preserved. Without that frame, later untagged completion is ignored; the wire also lacks the launching tool's owning-agent relation and task model/progress details. The missing ancestry/snapshot bookkeeping is confirmed in source, while an actual missed nested completion remains unverified. [Tagged branch][L-tagged], [notification and task-start handling][L-task-notification].

T3's new `7ab800a43` tracks snapshot-only tool parents, inherits the owning subagent's actual model and publishes corrections immediately. Reuse LUNA's existing child transcript sinks, but add native parent/task/generation mapping. Keep native subagents, workflows, teams, shell jobs and persistent app chats distinct. Hook execution may occur natively even while LUNA cannot display it; G1 also changes which configured hooks can load. [T3 ownership][T-nested], [T3 model correction][T-model].

**G8. Native history, reset and context lifecycle are still missing.** Claude's native-fork capability remains None. Same-chat rewind explicitly requires Codex. Message UUIDs/checkpoints, native list/read/fork/truncation and file rewind need Claude implementations; a context-copy chat or Git restore is a different operation. The shared explicit compaction path falls through to the harness default unsupported operation, while Claude's normalizer ignores native compact/reset subtypes. Native textual commands can work without their effects being correctly projected into LUNA. [Fork gate][L-history], [rewind][L-rewind], [default compaction][L-harness-defaults], [system filtering][L-task-notification].

Native CLI fork support means this is not proof that an SDK rewrite is mandatory. Establish exact idle history operations and atomic binding changes before advertising support. File checkpoint rewind must stay separate: native documentation excludes Bash edits and most subagent edits from checkpoint restoration. T3 and BB native fork implementations are reusable references, not proof that either implements all rewind semantics. [Session contract](https://code.claude.com/docs/en/agent-sdk/sessions), [checkpoint limitations](https://code.claude.com/docs/en/agent-sdk/file-checkpointing), [T3 history][T-history], [BB fork][B-fork].

**G9. Claude goals, scheduling and findings are disconnected from shared controls.** The redesigned goal card and `/goal` host command are Codex-only; the review admission route is also Codex-only. Claude's native `/goal` works in non-interactive mode and has condition/evaluator/resume semantics, so a plain slash-command path may execute while LUNA has no native goal readback. It is not equivalent to a Codex token-budget goal. Generic `ReportFindings`, Cron/Monitor/Workflow calls also lack usable results and task lifecycle fidelity through G2/G7. [Goal gate][L-goals], [goal UI][L-goal-ui], [review][L-review], [native goal contract](https://code.claude.com/docs/en/goal).

Remaining work: adapt controls and observation to Claude's actual operations, represent supported controls separately, preserve schedule lifetime/cancellation and expose findings with immutable revision/provenance. Ordinary chat review already remains useful. Cloud ultrareview needs separate account/service/job verification. Do not send Codex RPC method names to Claude or reintroduce cancelled Plan/Build modes.

**G10. Mixed results/resources/widgets need more than the new text-output viewer.** The shared `AgentEvent::ToolResult` contract still has `Option<String>` output plus an optional diff. It cannot by itself describe ordered images/documents/resources, structured content, MIME, citations or widget metadata. Simply stringifying everything would retain some bytes while losing product semantics. Native integration observations remain internal, and the new picker/RPC does not cover Claude's selected-session MCP state. [Shared output contract][L-tool-contract], [init observation][L-system], [picker gate][L-picker-rpc].

Remaining work: a host-owned typed result/asset envelope, lazy retrieval with session ownership, effective MCP health/auth/reload/catalog readback and a safe rich-content fallback. Add widget support only after capability validation; T3 and BB's unchanged pinned SDK versions do not establish newer MCP Apps administrative API support. Current native docs also distinguish `ui://` app resources from ordinary model-readable resource suggestions. Preserve the existing decision to omit computer-use screenshot bytes from retained tool output until the user changes it. [MCP resources](https://code.claude.com/docs/en/mcp).

**G11. Effective settings, extensions and live accounting remain partial.** Shared Claude skill-folder inventory and native Usage infrastructure exist, but effective plugins/agents/LSP/output styles, hooks, dynamic reload and per-session attachment are not connected. Initialize reports useful style/capability fields that LUNA's model/command projections ignore. `commands_changed` is not normalized. Model/effort/fast/context changes mostly reconstruct a session rather than use validated live controls. Stream usage keeps input/output totals and ignores allowed warnings; shared historical cost/quota parsing does not repair native turn/task omissions. [Discovery][L-discovery], [rate limit handling][L-results], [historical usage][L-usage], [quota mapping][L-limits].

Remaining work: scope discovery by actual cwd/account/binary/policy, invalidate on native/config changes, show configured versus effective state, add native runtime mutation where supported, and reconcile main/background/cache/cost usage without double-counting. BB's resident settings controls and T3's task-event translation are references. This is extension of existing infrastructure, not a new settings implementation.

**G12. Advanced and externally hosted features still need their own coverage.** The original workflows/teams/channels/peer-message and hosted/browser/computer/remote/cloud/artifact/routine ledger remains open or gated. The latest shared merge does not add these Claude interfaces. Generic tool-name rendering does not establish their lifecycle/control/result support. Certify individually rather than treating them as prerequisites for the first everyday-parity milestone.

One inventory omission deserves an explicit row: provider-supervised local background sessions (`claude agents --json`, attach/logs/stop/respawn) are separate from Claude subagents and LUNA-created child chats. Current native docs expose a supported session-state interface. If full parity includes external native sessions, add import/link/ownership and prevent two writers to the same native conversation. This audit did not start or inspect real external background sessions. [Agent-view contract](https://code.claude.com/docs/en/agent-view).

## CC-00–CC-18: refreshed package ledger

“Shared ready” below means relevant code is merged, not that the entire package is done. No package is fully Claude-certified by this audit.

| Package | Current assessment | Remaining scope after the merges |
| --- | --- | --- |
| CC-00 baseline/conformance | Fresh source baseline and isolated initialization evidence | Full version/account/policy/OS runtime matrix; prioritize G1 and G3 |
| CC-01 transport/control | Open; current CLI driver is still the transport | Versioned controls/receipts/config semantics; evaluate CLI sufficiency before choosing an SDK sidecar |
| CC-02 events/storage | Shared text-output foundation ready | Complete Claude result, origin/UUID, task/hook/reset/compaction decoding and safe unknown retention |
| CC-03 permissions/dialogs | Shared details/forms/URL foundation ready | Claude detail projection, native dialogs/elicitation/cancel and exact effective mode contract |
| CC-04 images/attachments | Shared staging/queue ownership ready; Claude cold/resumed images already existed | Recoverable validation, image-only acceptance evidence, all admitted turn types and native skill input |
| CC-05 tools/results/schema | Shared viewer/activity ready | Native output/diffs, lossless tool arguments, structured result/schema and richer typed assets |
| CC-06 models/settings/usage/delivery | Existing Claude discovery and shared Usage ready in part | Matching effective discovery, receipts/origin, runtime controls and live detailed accounting |
| CC-07 callable app/Executor bridge | Open | Actual session bridge and capability-based engine authority; external connection reuse |
| CC-08 native agents/tasks | Existing basic child routing; shared transcript infrastructure reusable | Forwarding opt-ins, nested bookkeeping, progress/model/usage/workflow/task cancellation |
| CC-09 persistent children/peers | Shared creation/startup/outbox/right pane ready | Real Claude manual-child acceptance, callable creation via CC-07, native peer trust/delivery |
| CC-10 history/checkpoints | Shared history presentation/reconciliation ready | Claude native history UUIDs/fork/truncation/reset/file checkpoint operations |
| CC-11 MCP inventory/resources/apps | Shared inventory/picker presentation ready; Claude skill folder inventory already exists | Effective Claude MCP state, rich resources, native admin and capability-gated widgets |
| CC-12 extensions/hooks/styles | Partial native inheritance and file-based settings infrastructure | G1 policy correction, effective scope/precedence, typed dispatch, hook observation and reload |
| CC-13 workflows/teams/channels | Open | Native graphs, origin, membership and lifecycle; add external background-session sub-ledger |
| CC-14 native goals/scheduling | Codex goal UI ready | Claude condition/evaluator/status/control mapping; Cron/Monitor/wakeup lifecycle |
| CC-15 review/findings | Codex review/shared findings foundation ready | Claude ordinary/native structured findings, immutable provenance and separate ultrareview service lane |
| CC-16 hosted/device integrations | Individually unverified or externally gated | Per-service supported interface, identity, permissions, results/assets and revoke/stop evidence |
| CC-17 compatibility/operations | Existing launch/recovery; BB offers newer maintenance references | Current native policy, cancel/stop/drain, limits/auth variants, unknown controls and compatibility ledger |
| CC-18 certification/delivery | Open for Claude parity | Production-path native/engine/UI evidence for every claimed capability, then normal release gates |

## Changes in the newly refreshed references

### T3

The original audit's statement that newer upstream commits had no provider changes is now stale. The latest 32-commit range includes two directly relevant Claude changes:

- `9da066dbe`: prevents an autonomous/background result from ending a different active user turn; important for `/compact`, resumed tasks and peer arrivals. [Correlation implementation][T-correlation].
- `7ab800a43`: preserves actual child model and nested tool/task ownership when assistant snapshots race task registration. [Nested ownership][T-nested], [model correction][T-model].

Its existing typed image/skill builder, scoped HTTP MCP registration, tool/dialog callbacks, task/hook/compaction/commands events and native session history remain stronger integration references than a bare SDK import. No T3 app or SDK runtime was executed in this audit. T3's Plan/Build handling is not a design to copy under the current LUNA product decision.

### BB

The Claude-related source delta since `f1172452` includes:

- `0162e53`: stop/discard now interrupts before ending the SDK input iterable and draining completion. [Stop/drain][B-stop].
- `d5d3e78`: cheap installation checks can skip update/doctor/global-package work, reducing provider startup overhead. [Maintenance][B-maintenance].
- `ebfe4f0`: accepts the npm 12 dist-tags result shape. [Maintenance][B-maintenance].
- `a7562b9`: provider availability settings/CLI work affects the broader discovery/operations surface.
- `3f28c07`: removes unused compatibility paths and tests, including accepted-but-unused `disallowedTools` plumbing; a large deletion count is not new capability coverage.

The resident session, app MCP tool proxy, native fork, interactive and task/workflow translation remain useful. Input images are still represented as text/path markers in the inspected bridge. BB has no inspected `rewindFiles`/`enableFileCheckpointing` path in that bridge. Do not describe it as the reference implementation for vision, full checkpoint rewind or every new MCP Apps API. Its stream origin guard is useful but should not be mistaken for T3's stronger UUID correlation.

## Residual shared issues to keep separate from Claude adapter work

The final coordinator handover retained several follow-ups. Source/history checks here show they should stay on the shared backlog; this audit did not reproduce their UI behavior:

- Approval choices use a radio role while acting immediately. The current [wizard code][L-radio] still uses the radio role. Review the permission action semantics across providers.
- A fresh dispatch writes its normal user row before native resume reconciliation. A rejected send can therefore leave a delivered-looking row. The current source retains that ordering at [write/prepare-resume][L-rejected-row].
- The failed child-creation collapsed chip remains generic; expanded output has the reason. This is inherited final QA evidence, not a new Claude failure.
- The earlier full-suite ledger retained three `task_identity` failures, one `turn_quiesce` failure, a parallel native UI test SIGABRT and manual checks. These are prior evidence at the integration revision, not fresh test results for this audit's HEAD.

The goal/child-pane follow-up is now merged, but its existence does not prove Claude provider support or settle every remaining shared manual check. Avoid reopening the already-fixed steering `turnId`, native-review settlement or cold-resume timeout work as if it were still missing.

## Recommended implementation order now

1. **Certify the installed runtime contract.** Resolve G1 discovery/run mismatch and G3 native-turn identity first. Add current fixtures and failure expectations before changing the adapter. Keep the CLI-versus-sidecar decision evidence-driven.
2. **Preserve data and interactive outcomes.** Complete Claude text/result decoding, unknown-frame retention, approvals/dialogs/cancellation and recoverable image input. Reuse shared journal, input wizard, queue and draft ownership.
3. **Attach callable app tools.** Deliver CC-07 with scoped authority, then connect persistent child creation and cross-chat operations. Prove one harmless read plus authorized creation/send separately.
4. **Connect native agent/history/settings surfaces.** Add native forwarding, task ownership, hooks, scoped picker/skill dispatch, UUID history/reset/compaction and detailed live accounting. Reuse existing panes and inventories.
5. **Adapt goals/review and certify advanced lanes.** Connect Claude-specific semantics to shared controls; certify workflows/teams/channels and hosted/device features individually. Keep availability/unsupported states visible.

This replaces the old implication that every shared feature needs to be built again. It also avoids closing Claude packages merely because a Codex feature was merged.

## Additional acceptance scenarios

Retain E01–E22 in the original [validation plan](validation-plan.md), with their native/engine/persistence/UI artifact requirements. Add these concrete cases before implementation:

| ID | Failure expectations and required evidence |
| --- | --- |
| E23 effective configuration | Initialize and launch under the same binary/home/cwd/policy for Ask/Auto/Full; custom project command, style and tool availability must agree. Cover project/account switches, explicit settings precedence and cache invalidation. The current three initialization observations are a baseline, not the full pass. |
| E24 autonomous-result correlation | Queue a human prompt behind a resumed background/peer/native-command turn; foreign result must not complete its receipt or transcript. Cover native UUID echo, origin-only frames, older runtimes, duplicates and reconnect. Preserve native and queue IDs. |
| E25 foreground/nested native ownership | Parent invokes a foreground child, that child invokes a child/tool/background job; preserve forwarded text, spawn IDs, model and task completion. Unknown/late notification must not attach to the root. Observe configured hook progress separately. |
| E26 Claude input/discovery | All admitted image turn types and image-only input, invalid/oversized/changed files, selected-host ownership, typed skill expansion with mixed images/prose, and project/account catalog changes. Rejection must preserve the actual draft envelope. |
| E27 provider-aware shared controls | Real Claude manual child plus restart/outbox/pane; callable child only after bridge delivery. Goals/review/compact/history use verified Claude operations or truthful unavailable controls, never Codex method names. |
| E28 rich results | One controlled tool/MCP call returns text, structured values, image and resource metadata; root/child journal ownership, lazy fetch, restart and safe unknown fallback preserve ordering without duplication. Keep widget certification separate. |

Implementation requires the normal locked build/format checks, meaningful production-path E2E, retained repeatable artifacts and primary app handoff. This reassessment changes documentation only, so it does not require a Rust rebuild or app refresh.

## Pinned source references

All LUNA links below point to the inspected `d0f12bcf` tree; T3 links to `5cc99e1c2`; BB links to `2573ad5`. Use the new baseline to detect drift before implementing.

[L-launch]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/mod.rs#L148
[L-policy]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/mod.rs#L353
[L-discovery]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/mod.rs#L263
[L-discovery-cache]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/mod.rs#L448
[L-runtime]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/mod.rs#L502
[L-controls]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/mod.rs#L1000
[L-approvals]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/mod.rs#L1021
[L-questions]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/mod.rs#L1097
[L-images]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/mod.rs#L665
[L-content]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/wire.rs#L198
[L-user-frame]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/wire.rs#L302
[L-result-frame]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/wire.rs#L235
[L-system]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/wire.rs#L28
[L-unknown]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/wire.rs#L294
[L-results]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/normalize.rs#L570
[L-child-results]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/normalize.rs#L533
[L-completion]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/normalize.rs#L597
[L-tagged]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/normalize.rs#L413
[L-task-notification]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/claude/normalize.rs#L276
[L-tool-contract]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/proto/src/agent.rs#L850
[L-tool-gate]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/cross_task_tools.rs#L641
[L-inputs]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/sessions.rs#L3161
[L-warm]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/sessions.rs#L677
[L-steer]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/sessions.rs#L3985
[L-review]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/sessions.rs#L3178
[L-rejected-row]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/sessions.rs#L3609
[L-picker-ui]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/ui/src/composer/integrations.rs#L396
[L-picker-rpc]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/rpc.rs#L9685
[L-rewind]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/rpc.rs#L5420
[L-goals]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/rpc.rs#L4326
[L-goal-ui]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/ui/src/composer/goals.rs#L195
[L-child-authority]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/child_threads.rs#L90
[L-child-manual]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/child_threads.rs#L149
[L-child-selection]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/child_threads.rs#L280
[L-history]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/history_operations.rs#L21
[L-harness-defaults]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/harness/src/lib.rs#L263
[L-skill-inventory]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/integration_settings.rs#L202
[L-usage]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/usage.rs#L260
[L-limits]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/engine/src/usage.rs#L105
[L-radio]: https://github.com/novnski/luna/blob/d0f12bcf6f01d719656eac4288483eeb05c2e347/crates/ui/src/composer.rs#L11108
[T-correlation]: https://github.com/pingdotgg/t3code/blob/5cc99e1c23980d7995a13c47f969b47cb68ed1be/apps/server/src/provider/Layers/ClaudeAdapter.ts#L1712
[T-nested]: https://github.com/pingdotgg/t3code/blob/5cc99e1c23980d7995a13c47f969b47cb68ed1be/apps/server/src/provider/Layers/ClaudeAdapter.ts#L3771
[T-model]: https://github.com/pingdotgg/t3code/blob/5cc99e1c23980d7995a13c47f969b47cb68ed1be/apps/server/src/provider/Layers/ClaudeAdapter.ts#L3374
[T-input]: https://github.com/pingdotgg/t3code/blob/5cc99e1c23980d7995a13c47f969b47cb68ed1be/apps/server/src/provider/Layers/ClaudeAdapter.ts#L1620
[T-skills]: https://github.com/pingdotgg/t3code/blob/5cc99e1c23980d7995a13c47f969b47cb68ed1be/apps/server/src/provider/Drivers/ClaudeSkillDispatch.ts#L3
[T-bridge]: https://github.com/pingdotgg/t3code/blob/5cc99e1c23980d7995a13c47f969b47cb68ed1be/apps/server/src/provider/Layers/ClaudeAdapter.ts#L5026
[T-history]: https://github.com/pingdotgg/t3code/blob/5cc99e1c23980d7995a13c47f969b47cb68ed1be/apps/server/src/provider/Layers/ClaudeAdapter.ts#L5538
[B-proxy]: https://github.com/get-bb/bb/blob/2573ad5e85835dc1c90aa33795e6dba52ef5aa92/plugins/provider-claude-code/src/bridge/tool-proxy-mcp.ts#L28
[B-input]: https://github.com/get-bb/bb/blob/2573ad5e85835dc1c90aa33795e6dba52ef5aa92/plugins/provider-claude-code/src/bridge/bridge.ts#L2456
[B-fork]: https://github.com/get-bb/bb/blob/2573ad5e85835dc1c90aa33795e6dba52ef5aa92/plugins/provider-claude-code/src/bridge/bridge.ts#L2252
[B-origin]: https://github.com/get-bb/bb/blob/2573ad5e85835dc1c90aa33795e6dba52ef5aa92/plugins/provider-claude-code/src/delta-translation.ts#L1002
[B-stop]: https://github.com/get-bb/bb/blob/2573ad5e85835dc1c90aa33795e6dba52ef5aa92/plugins/provider-claude-code/src/bridge/sdk-session.ts#L348
[B-maintenance]: https://github.com/get-bb/bb/blob/2573ad5e85835dc1c90aa33795e6dba52ef5aa92/plugins/provider-claude-code/src/bridge/provider-maintenance.ts#L122
