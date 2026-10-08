# Claude-coauthored feature audit — verified coverage snapshot

**The source review and bounded local acceptance run are complete, with the limits below.** Frozen at **2026-10-08T01:32:33.601894+00:00**, source HEAD `dd80eb3d`. Completed live-r4 and warm-restart evidence is now included. The coordinator completed preservation, scoped QA cleanup and the primary-app handoff; see the final delivery record below.

Scope is unchanged: **185 commits / 29 feature families**, comprising 169 Claude/Anthropic coauthor trailers and 16 shared foundation commits in `bd603de1^..d3819203`, plus dependency `010bbadf`. [coverage.json](coverage.json) retains the exact commit/path map, original failure matrix, planned commands, 110 hashed evidence records, per-family results/gaps and 20 fix entries.

## Verified results at this cutoff

- **59 distinct Rust suite targets** have latest complete target summaries of PASS. The UI library passed **1,188 tests** after the multiline/AX fixes. `fmt-r4` and `clippy-r3` passed; earlier failures remain preserved.
- **21 signed-app T3 scenario passes:** Claude S1/S2/S3/S5/S6/S7/S8/S9U/S9Q and restart S4; Codex S9Q; OpenCode S1/S2/S2X/S3/S5/S6/S7/S8/S9U and MiMo S9Q. **Four signed-app T2 passes:** OpenCode restart S4, pause/kill S4X, 165-second stall S4S and automatic/manual compaction S8X.
- **Eight additional real-provider app workflow runs passed:** Codex and Claude Full→Ask transitions; native fork findings merge and Codex→Claude→Codex recall; both-provider scheduled one-shots with paused twins; fresh opposite-provider child creation/defaults; the eight-check Codex goal flow with finite 60,000-token budget; resumed agent-workspace; and live context recall after restart. Four additional restart readbacks verify agent-workspace, automations, execution observations and completed-goal persistence. Native workspace-data acceptance separately passed **4/4** local-data checks.
- **19 paid OpenCode Go probes passed** their actual acceptance predicates on installed **OpenCode 2.0.24**, model `opencode-go/claude-haiku-5-5`. Whole-batch secret scan found **zero findings in 96 files**; a **99-entry checksum manifest** is retained. These are adapter/engine probes, not graphical app certification.
- **Settings 168/168 captures; startup 26/26 runs; performance 44/44 checks at both 1,200 and 10,000 entries across all 11 phases; sidebar/settings AX 8/8 trees plus four PNGs.** The AX frame numbers advance and expected Button nodes survive.
- **Actual CUA acceptance** verifies bounded three-line and scrolling sixteen-line answers, narrow-pane footer, draft navigation/deferred reopen, correct-chat submission and one amendment/two-answer history. Native model-cache refresh adds the controlled small model; native automation preview/confirmation has independent readback showing all six audit definitions paused. Native empty Welcome still has image/AX plus ready/zero-chat/zero-space proof.

S4’s held queue rows survived PID **3565→17694** with execution count **0**. A later graceful restart **17694→52172** passed completed context/automation/execution/workspace/goal checks. This proves preservation without replay; it does **not** prove the held messages ran. Historical failed attempts are never relabelled PASS: each has a separate superseding run and reason.

The coverage updater only read existing artifacts and saved images and edited these two ledger files. It ran no build, test, GUI operation, provider call or Git mutation. Tests span several audit revisions and dirty source; artifact hashes and available binary/source identities prevent the current HEAD from being mistaken for every earlier tested binary.

## Proof types and identity

| Type | Established evidence | Limit |
| --- | --- | --- |
| Rust deterministic integration/component checks | Actual production contract assertions, persistence and fault handling | Not current model eligibility or native hit-target proof |
| Native fixture with synthetic state/engine | Actual Shell/pane rendering, keyboard ownership, AX trees and checked geometry | Not real-provider behavior or every production gesture |
| Signed-app T3 RPC scenario/workflow | Actual model/provider behavior through app fences and preserved receipts | Native gesture/pixel acceptance needs separate CUA evidence |
| Signed-app T2 scenario | Real OpenCode binary with controlled mock provider | Model wording/consumption informational rows stay informational |
| Paid Go adapter/engine probe | Installed OpenCode/model through the named compiled production layer | Not a signed-app GUI or complete scenario suite |
| CUA image + AX + independent data readback | Recorded real native control, draft, layout or pause state | Only the actual tested scale, identity and sequence |

`qa-identity-r3.json` records **LUNA QA Review**, bundle `com.novnski.luna.qa.review`, port **59841**, PID **17694**, exact executable, QA data and MCP isolation. The rebuild log records **43 seconds** (approximately 44 seconds for the refresh reported by the coordinator). `qa-identity-restarted.json` subsequently verifies the same bundle/profile at PID **52172**. These are QA identities, not final primary-app handoff. The fresh Welcome check used the separate `claude-review-20261008-fresh` profile. Paid Go probes reused an existing profile; binaries were built in this checkout. No new worktree is attributed to this audit.

## Completed signed-app scenarios

| Scenario / provider | Tier | Checked outcome | Important retained limit |
| --- | --- | --- | --- |
| S1 / claude-code | T3 | **PASS**, 33 decisive checks | Native blocking prompts corrected; stale-answer receipt also retained. |
| S2 / claude-code | T3 | **PASS**, 19 decisive checks | Shared shell Task Work row passed; separate NativeTask shell observation informational/no. |
| S3 / claude-code | T3 | **PASS**, 18 decisive checks | Manual compact refused truthfully; later tool call passes, not completed compaction. |
| S5 / claude-code | T3 | **PASS**, 39 decisive checks | Fork/edit/rewind/side-chat assertions pass; not every file-restore permutation. |
| S6 / claude-code | T3 | **PASS**, 12 decisive checks | Usage-store explicitly skipped. |
| S7 / claude-code | T3 | **PASS**, 27 decisive checks | Named image/skill/title acceptance passes; transcript image projections include informational/no, Claude manual compact unsupported. |
| S8 / claude-code | T3 | **PASS**, 9 decisive checks | Review receipts/settlement pass; content quality and goal-get rows informational. |
| S9U / claude-code | T3 | **PASS**, 25 decisive checks | Recommended auto-answer, Keep waiting and no-recommendation waits; not all exact-expiry races. |
| S9Q / claude-code | T3 | **PASS**, 25 decisive checks | Model wording/busy-time observations informational; 25 shared decisive checks. |
| S9Q / codex | T3 | **PASS**, 25 decisive checks | Explicit-model T3-only runner admission; 25 shared decisive checks. |
| S9Q / opencode | T3 | **PASS**, 26 decisive checks | MiMo free model; 25 shared checks plus wire capture. Earlier quota failure retained. |
| S4 / claude-code | T3 | **PASS**, 15 decisive checks | Held queued row preserved, execution count 0; live/stale answer readback retained. |
| S4 / opencode | T2 | **PASS**, 14 decisive checks | Held queued row preserved, execution count 0; real binary plus mock. |
| S4X / opencode | T2 | **PASS**, 26 decisive checks | Approval/form pause+kill and stale answer pass; model output and some server rows informational/no. |
| S4S / opencode | T2 | **PASS**, 10 decisive checks | 165-second turn; notice after 131,758 ms, still working, then cleared. |
| S8X / opencode | T2 | **PASS**, 12 decisive checks | mock-small automatic/manual compaction; session-during-compaction informational/no. |
| S1 / opencode (live-r4) | T3 | **PASS**, 34 decisive checks | Completed real-model acceptance; not every native gesture or edge permutation. |
| S2 / opencode (live-r4) | T3 | **PASS**, 20 decisive checks | Completed real-model acceptance; not every native gesture or edge permutation. |
| S2X / opencode (live-r4) | T3 | **PASS**, 28 decisive checks | Late asks/wake/Stop pass; later replacement child has distinct call IDs. Final blocking/unknown 0. |
| S3 / opencode (live-r4) | T3 | **PASS**, 19 decisive checks | Fresh/resume/fork/compact app-tool delivery and scoped refusal. |
| S5 / opencode (live-r4) | T3 | **PASS**, 40 decisive checks | Completed real-model acceptance; not every native gesture or edge permutation. |
| S6 / opencode (live-r4) | T3 | **PASS**, 10 decisive checks | Usage-store enabled; this model offers no variants, so variant rows informational/no. |
| S7 / opencode (live-r4) | T3 | **PASS**, 29 decisive checks | Staged images match actual wire files; manual compact and next LUNA call pass. |
| S8 / opencode (live-r4) | T3 | **PASS**, 10 decisive checks | Completed real-model acceptance; not every native gesture or edge permutation. |
| S9U / opencode (live-r4) | T3 | **PASS**, 26 decisive checks | Completed real-model acceptance; not every native gesture or edge permutation. |

Paths and exact criteria live in JSON under these evidence IDs. Source directories are `live/`, `live-r2/`, `live-r3/`, `live-r4/`, `restart-r2/`, `restart-r3/` and `mock-r2/` beneath `target/qa/claude-review-20261008/`. Scenario manifests, result Markdown, secret scans and checksum manifests were inspected. **All nine completed `live-r4` scenarios are now counted**; their completed manifests and decisive criteria were inspected after batch completion.

## Other completed app and CUA workflows

| Evidence ID | Verified behavior | Boundary |
| --- | --- | --- |
| `native-context-transfer` | Native fork findings merged while parent was active; Codex to Claude to Codex recalled transferred markers and all three transfers were consumed. | Three transfers consumed; selected preview/editor edge permutations not inferred. Restart proof has its own row below. |
| `native-execution-codex` | Full to Ask while running holds native Steer; explicit later send appears once using Ask. | Full→Ask tested; inverse/all setting combinations remain separate. |
| `native-execution-claude-code` | Full to Ask while running holds native Steer; explicit later send appears once using Ask. | Running old policy, blocked Steer, held row, then exactly one send under Ask. |
| `native-automations-r2` | Codex and Claude each scheduled once with one user/assistant marker; separately paused definitions produced zero occurrences. | One completed occurrence and user/assistant marker per provider; paused twins zero. Restart proof below; no live PR-watch claim. |
| `native-agent-workspace-fresh` | Both provider parents created the opposite-provider fresh child using saved defaults, exactly supplied initial prompt, no approval and completed child response. | Fresh flow: opposite providers, saved defaults, supplied prompt, no approval; resumed/restart proof has separate rows below. |
| `native-workspace-data` | Four production-app data checks: file read/write and stale save; worker diff sources; immutable upload retry/collision; checkpoint readback and shared-restore refusal. | Four local file/diff/upload/checkpoint assertions, not a user-checkout destructive restore. |
| `live-codex-goals-r2` | All eight original native goal checks passed with a finite 60000-token budget: ordinary-no-goal, pause/update/remove-budget, two idle continuations, checkpoints, usage and explicit completion. | All eight original checks; finite 60,000 cap. Original 6,000 cap failure retained. |
| `native-question-flow` | Three-line Unicode draft bounded; 16-line draft scrolls within six-line cap in wide/narrow panes; navigation and deferred reopen preserve draft; correct-chat submission followed by exactly one amendment yields two answers. | Three/16 lines, scroll/narrow/navigation/defer/amend pass. Native 12/20 sizes and unsent-draft restart not claimed. |
| `native-settings-model-refresh` | Native Refresh model cache action completed; reopened Providers list retained selected model and refreshed catalog readback contains luna-mock/mock-small. | Controlled add-model/Providers refresh; removal, Projects/title menus and concurrent saves not fully checked. |
| `native-automations-ui` | Native Automations page/paused preview-confirm flow captured; independent readback proves all six audit definitions disabled. | Actual preview/confirm capture plus six disabled definitions in independent readback. |
| `fixture-sidebar-ax-native` | Eight rendered AX trees keep all expected task/project/settings Buttons across fresh increasing frames; four PNGs and pass receipt retained. | Eight actual rendered trees retain expected Buttons over fresh frames; four images. |
| `native-empty-welcome` | Raised signed QA window visibly shows What will you build? and Add project; AX exposes Add project; EngineReady=true with zero chats and spaces. | Raised window shows Welcome without creating chats/projects; prior background spinner was an occluded capture. |
| `live-claude-writer-refusal` | Claude 2.1.293 listed a real external interactive writer; LUNA rejected cold resume before acceptance, launched no turn and cleaned holder by stdin EOF. | External real writer refused before acceptance, no turn launch; no atomic lock or fail-closed listing guarantee. |
| `native-agent-workspace-resume` | Both providers completed the resumed tool workflow with default models/permission checks, exactly one fresh helper and one persistent snapshot fork; archived helper/fork and Main-only layout. | Defaults/permissions/helper/fork/exact-once/archive checked; no broad all-tools claim. |
| `native-agent-workspace-restart` | After PID17694 to52172 both providers retain completed fresh/resume workflows, one archived helper, one archived fork and Main-only layout. | Archived helper/fork and Main-only layout survive restart for both providers. |
| `native-context-restart` | After PID17694 to52172 the same native provider session recalled both markers; three consumed transfers stayed identical with no reinjection. | Same providerTaskId before/after marker recall; 3 consumed transfers unchanged and no reinjection. |
| `native-automations-restart` | Both providers retain one completed occurrence/user message after restart; paused definitions still have zero occurrences. | One completed occurrence/message per provider; paused twins still 0. |
| `native-execution-restart` | Both execution-transition chats have idle/no-live-running observations after restart. | Both tasks idle with no stale live-running observation. |
| `native-goal-restart` | The completed native Codex goal remains complete after app restart with the finite60000 budget and retained usage. | Goal remains complete; 60,000 budget; 12,879 tokens retained. |
| `native-opencode-work-popover` | Native Background work popover labels work owned by this chat and truthful stop availability; final S2X readback has zero blocking/unknown with four completed/stopped child rows. | Owned-work live popover and final four terminal child rows;blocking/unknown 0. |

Main artifacts: `native-expanded/context-transfer/run-zHGEvS/`, `native-expanded/execution-summary/run-{codex-DpGpRw,claude-code-xASZry}/`, `native-expanded/automations/run-aZiEpY/`, `native-expanded/agent-workspace/snapshot-verify-1791422183026.json`, `native-expanded/workspace-data/result.json`, `codex-goals-live-r2/result.json`, `sidebar-ax-native/`, `question-native-{submit-<correct-chat>,amendment}.json`, `models-after-small.json`, and `automations-native-paused-readback.json`. Exact paths and hashes are in JSON.

## Paid OpenCode Go: completed predicates

All rows below have exit 0 **and** the stated acceptance marker/structured assertions; zero exit alone was not used as acceptance. Evidence is under `target/qa/claude-review-20261008/live-go/<name>/`.

| Probe | Layer | Checked result |
| --- | --- | --- |
| `plain` | adapter | PASS — explicit acceptance PASS marker |
| `s1-allow` | adapter | PASS — status/text/count/resume predicates true |
| `s1-deny` | adapter | PASS — status/text/count/resume predicates true |
| `s1-cancel` | adapter | PASS — status/text/count/resume predicates true |
| `s1-question` | adapter | PASS — status/text/count/resume predicates true; 7-second answer delay |
| `s2-subagents` | adapter | PASS — both named foreground/background children completed; wake/output predicate true |
| `s2-late-ask` | engine | PASS — 9 structured checks, none failed |
| `s2-child-stop` | engine | PASS — 10 structured checks, none failed |
| `s3-luna-tools` | engine | PASS — 23 structured checks, none failed |
| `s3-child-containment` | adapter | PASS — fresh/resume tool delivery and child containment PASS |
| `steer` | adapter | PASS — explicit acceptance PASS marker |
| `stop-steer` | adapter | PASS — explicit acceptance PASS marker |
| `s4-drop` | adapter | PASS — explicit acceptance PASS marker |
| `s4-drop-late` | adapter | PASS — explicit acceptance PASS marker |
| `s4x-server-kill` | adapter | PASS — 6 structured checks, none failed |
| `s5-history` | adapter | PASS — 16 structured checks, none failed |
| `s6-s7-inputs-models` | adapter | PASS — all input/model/compaction steps true; text-only/SVG input rejected recoverably |
| `s7-discovery` | adapter | PASS — command/skill discovery and refreshed entries PASS |
| `tools-output` | engine | PASS — Completed; GO-HEAD/GO-TAIL markers verified; one full tool result |

The history probe checks native fork/rewind prefix and file-preservation conditions. The tool-output probe retains journal/document/readback artifacts. The child-stop probe checks idempotent Stop receipt, interrupted child, continued root and absence of lingering work; the late-ask probe checks root parked, child attribution, answered resolver and separate wake segment. SSE/server faults affect only the owned native provider server, not a full application restart.


## Rust and fixture evidence

| Coverage | Completed result | Primary artifacts under the evidence root |
| --- | --- | --- |
| Harness/OpenCode | 137 library +13 OpenCode adapter passed | `logs/opencode-live-fixes{,-r2}.log` |
| Claude adapter baseline | 113 passed across 16 targets; 5 ignored | `logs/claude-provider-regressions.log` |
| Input/queue/restart | Named target results pass; original task-command failure superseded | `logs/engine-input-queue.log`, `logs/command-stack-fixed.log` |
| Workspace/context/automations | Automation 13(+1 ignored), workbench 9, child 4, tools 1, context 8; cross-task 5 rerun | `logs/engine-workspace-orchestration.log`, `logs/engine-final-extended.log` |
| Backup/history/E2E | Backup 27, Claude history 1, E2E 33(+2 ignored) | `logs/engine-fixes-final.log` |
| Goal default stack | Codex goals 5 + task commands 39 | `logs/command-stack-fixed.log` |
| UI library | 1,188 passed after UI fixes | `logs/ui-lib-r3.log` |
| Settings fixture | 168 settled captures, no warnings/bottom flags | `settings-gallery/manifest.json`, pass receipt |
| Startup fixture | 26 readbacks/pass receipts, splash gone | `startup/*/{first,restart}.json` |
| Performance fixtures | Each size 44/44 checks, 11 phases; signed/runtime/binary identity stable | `performance-{1200,10000}-r2/` |
| Rendered AX fixture | 8 fresh trees, 4 PNGs, expected Button labels retained | `sidebar-ax-native/readback.json`, `sidebar-polish.pass` |
| Final recorded checks | fmt-r4 and clippy-r3 pass; QA identity-r3 refreshed | `logs/fmt-r4.log`, `logs/clippy-r3.log`, `qa-identity-r3.json` |

Some aggregate logs retain nonzero results because one target failed at that attempt. Latest named target summaries and their full matching reruns establish acceptance. Earlier failures are retained, not ignored or erased. The native long-history fixtures are candidate acceptance/measurement; no before/after speedup claim is made.

## Retained failures and their superseding evidence

| Original failure | Attribution | Superseding result |
| --- | --- | --- |
| Initial Claude S1 question rows | Runner asked ambiguously despite nonblocking runtime guidance | Fresh blocking-tool S1 `live-r2` PASS |
| Initial Claude S7 turn/skill | Runner sent empty skill path from ListCommands; product rejected before acceptance | Absolute existing SKILL.md fallback/assertion, `live-r3` S7 PASS |
| Free Big Pickle OpenCode S9Q | Upstream quota prevented ask_user; duplicate terminal error was a separate fixed LUNA defect | Fresh available MiMo S9Q PASS; original quota run still FAIL |
| First native Codex goal | Normal budgetLimited at 6,000 cap, observed 13,208 tokens | Configurable finite 60,000 run passes all eight original checks |
| First live automations run `f9FbuN` | Transient document readback before assistant marker; independent later read found it | Bounded-reader runner correction and fresh `aZiEpY` PASS |
| Claude restart-r2 pre-phase | Pending input not reached within 180s | Fresh restart-r3 pre/post PASS; no restart claim for failed r2 |
| Goal Rust stack overflow | Large nested future on ordinary default stack | Production future boxing; full default-stack goal 5/commands 39 PASS |
| Initial 10k native performance | Fixture incorrectly required cache retention above default count ceiling | Corrected byte/count/positive-probe assumptions; 1.2k/10k reruns 44/44 |
| Missing AX rows/card/subtrees and multiline overflow | Confirmed native UI defects | UI `dd80eb3d`, eight-tree fixture + real native question/AX acceptance |

Failed artifact hashes remain in JSON. Fixing a runner does not establish a product defect, and succeeding with another provider/model does not retroactively pass the original run.

## Per-family results and remaining gaps

Every family retains its original detailed case matrix in JSON. “Partially verified” means the listed scoped receipts pass; it never means every possible case has been executed.

### F01 — Durable dispatch, queue receipts and RPC lifetimes

Completed S4 pending-input/held-row restart passes, plus resumed-workspace, context, schedule and execution readbacks now pass after a second graceful restart.

**Remaining:** Held queued S4 rows still executed zero times. Arbitrary crash-point/unknown-acceptance fault permutations are not all live-exhausted.

Evidence: `log-engine-input-queue`, `log-command-stack-fixed`, `log-engine-fixes-final`, `app-restart-r3-s4-claude-code`, `app-restart-r2-s4-opencode`, `native-context-restart`, `native-execution-restart`, `native-agent-workspace-restart`.

### F02 — Permissions, child ceilings and requested versus running settings

Both Codex and Claude live execution snapshots prove Full-to-Ask leaves Running=Full, Requested=Ask, blocks Steer, holds the message, then applies it exactly once under Ask. Permission wire/child ceiling suites and Claude S1 pass.

**Remaining:** Inverse Ask-to-Full, same-model account switch and every changed-setting permutation remain narrower than this live transition proof.

Evidence: `app-r2-s1-claude-code`, `log-claude-provider-regressions`, `log-engine-final-extended`, `native-execution-codex`, `native-execution-claude-code`, `native-execution-restart`.

### F03 — Persistent children, side chats and sidebar placement

Fresh and resumed opposite-provider workflows now pass, including exactly one helper/fork, explicit archival and Main-only layout; post-restart readback preserves them.

**Remaining:** Fold/drag/shelf and every concurrent layout/revision permutation remain non-exhaustive.

Evidence: `fixture-startup`, `app-r2-s5-claude-code`, `log-engine-workspace-orchestration`, `native-agent-workspace-fresh`, `native-agent-workspace-resume`, `native-agent-workspace-restart`.

### F04 — Persistent workbench, fork tabs and split placement

Native resumed workflow validates split/fork use and closes archived helper/fork views to Main-only; layout survives restart.

**Remaining:** All tab keyboard, simultaneous revision and source-close race combinations are not inferred.

Evidence: `app-r2-s5-claude-code`, `fixture-performance-1200-r2`, `fixture-performance-10000-r2`, `native-context-transfer`, `native-question-flow`, `native-agent-workspace-resume`, `native-agent-workspace-restart`.

### F05 — Workspace app tools, delegation UX and shared defaults

Fresh, resumed and post-restart workspace tools/defaults workflows pass on Codex and Claude; completed OpenCode S3 also covers fresh/resume/fork/compact and scoped refusal.

**Remaining:** Not every workspace tool payload or cross-host permission permutation is exhausted.

Evidence: `app-r2-s3-claude-code`, `go-s3-luna-tools`, `go-s3-child-containment`, `native-agent-workspace-fresh`, `app-r3-s9q-opencode`, `native-agent-workspace-resume`, `native-agent-workspace-restart`, `app-r4-s3-opencode`.

### F06 — Portable context and fork findings transfer

Real context merge/handoff roundtrip and post-restart same-provider-ID marker recall pass with exactly three consumed transfers and no reinjection.

**Remaining:** Arbitrary in-flight handoff crash points and all manual context-editor gestures remain separate.

Evidence: `log-engine-workspace-orchestration`, `native-context-transfer`, `native-context-restart`.

### F07 — Owned background work, native children and scoped Stop

Completed OpenCode S2/S2X and native owned Background work popover are verified; final four child rows are terminal with blocking=unknown=0.

**Remaining:** Replacement child after Stop has distinct call IDs and is not evidence of a failed original Stop. Every archive/ownership race is not exhausted.

Evidence: `app-r2-s2-claude-code`, `go-s2-child-stop`, `go-s2-late-ask`, `log-engine-fixes-remaining`, `native-agent-workspace-fresh`, `app-r4-s2-opencode`, `app-r4-s2x-opencode`, `native-opencode-work-popover`.

### F08 — Durable host automations and bounded PR watches

Real one-shot/pause acceptance and post-restart readback pass for both providers: still one occurrence/message and zero paused occurrences. Native readback leaves all six definitions disabled.

**Remaining:** Live PR-watch polling and every crash-between-claim-and-dispatch permutation remain covered by fixtures rather than current real-provider acceptance.

Evidence: `log-engine-workspace-orchestration`, `log-engine-fixes-final`, `native-automations-r2`, `native-automations-initial`, `native-automations-ui`, `native-automations-restart`.

### F09 — Long-history refresh and responsive panes

Both 1200/10000-entry native fixture reruns passed 44 checks across 11 phases. New native AX fixture additionally proves sidebar/settings trees survive later frames.

**Remaining:** No speedup claim or blanket final-source stress certification; the full long-history workload was not repeated solely for the later accessibility-active uncached sidebar path.

Evidence: `fixture-performance-1200-r2`, `fixture-performance-10000-r2`, `fixture-sidebar-ax-native`.

### F10 — Timeline chronology, tool details and shared visual controls

Output integration/live Go journal retrieval, pane rewrite fixtures, UI 1,188 and actual native sidebar/question fixes pass. Eight rendered AX trees retain expected Buttons; bounded three/16-line answer fields and native navigation/amendment work.

**Remaining:** Native output-detail vocabulary, drag/menu/hover combinations and every theme/size are still not exhaustive.

Evidence: `go-tools-output`, `fixture-performance-10000-r2`, `log-ui-lib-r3`, `native-sidebar-missing-task-ax`, `native-async-card-missing-ax`, `fixture-sidebar-ax-native`, `native-question-flow`.

### F11 — Composer command and skill sigils, goal mode and focus

Command/skill suites, paid Go discovery, corrected Claude S7 and all eight native Codex goal checks pass with explicit 60,000 budget.

**Remaining:** Native sigil/caret/menu/goal-chip negative permutations remain partial. Original 6,000-budget goal run remains failed as a fixture limit, superseded by fresh unchanged-check rerun.

Evidence: `go-s7-discovery`, `log-command-stack-fixed`, `live-codex-goal-budget`, `app-r3-s7-claude-code`, `live-codex-goals-r2`.

### F12 — Settings kit across all fourteen pages

Settings 168 capture matrix, current UI 1,188, native rendered Settings AX and actual model-cache Refresh acceptance pass. Native automation controls have independent paused readback.

**Remaining:** Full retained Projects/title-menu invalidation, removed models, concurrent saves and every loading/disabled/secret state are not inferred from the add-model case.

Evidence: `fixture-settings-gallery`, `log-ui-lib-r3`, `fixture-sidebar-ax-native`, `native-settings-model-refresh`, `native-automations-ui`.

### F13 — Appearance propagation and provider defaults for open composers

Provider defaults/appearance fixtures pass; fresh children use discovered saved cross-provider defaults. Native Refresh yields the added mock-small catalog while retaining the selected Provider model.

**Remaining:** Removed-model handling, Projects/title menus, delayed saves and complete outside-Settings appearance persistence permutations remain separate.

Evidence: `fixture-settings-gallery`, `log-engine-fixes-batch1-r3`, `native-provider-catalog-before`, `native-provider-catalog-after`, `native-agent-workspace-fresh`, `native-settings-model-refresh`.

### F14 — Claude model discovery, live switches and local usage

Claude catalog/CC06 suites, live S6 model/variant and native Settings refresh checks pass against actual discovered models; corrected Claude S7 passes named image/skill/title checks.

**Remaining:** S6 usage-store was explicitly skipped; account eligibility/removal/error-refresh and local-only usage presentation remain partial.

Evidence: `app-r2-s6-claude-code`, `log-claude-provider-regressions`, `log-engine-final-extended`, `native-settings-model-refresh`, `app-r3-s7-claude-code`.

### F15 — OpenCode v2 boot, contract gate, steering and SSE recovery

Completed OpenCode real-model S1/S2/S3/S5/S6/S7/S8/S9U/S2X joins paid probes and T2 recovery/stall/restart evidence.

**Remaining:** Version compatibility still reports Untested; all unsupported-version/remote-host or arbitrary transport race permutations are not certified.

Evidence: `log-opencode-live-fixes`, `log-opencode-live-fixes-r2`, `go-plain`, `go-steer`, `go-stop-steer`, `go-s4-drop`, `go-s4-drop-late`, `go-s4x-server-kill`, `app-mock-r2-s4x-opencode`, `app-mock-r2-s4s-opencode`, `app-restart-r2-s4-opencode`, `app-r4-s1-opencode`, `app-r4-s2-opencode`, `app-r4-s3-opencode`, `app-r4-s2x-opencode`.

### F16 — OpenCode native inputs, images, models and compaction

OpenCode real S7 now verifies image wire files against staged inputs and manual compaction; T2 automatic/manual compaction remains independently verified.

**Remaining:** Some transcript image projection observations remain informational/no; Claude compact stays unsupported. Every attachment MIME/size/race combination is not live-exhausted.

Evidence: `go-s6-s7-inputs-models`, `log-engine-input-queue`, `app-r2-s7-claude-code`, `app-r3-s7-claude-code`, `app-mock-r2-s8x-opencode`, `app-r4-s7-opencode`.

### F17 — OpenCode permissions, forms and reliable answer delivery

Completed OpenCode real-model S1 passes 34 decisive checks in addition to delayed Go input and T2 approval/form fault checks.

**Remaining:** All session-allowance/retry-exhaustion permutations are not inferred solely from general S1 success.

Evidence: `go-s1-allow`, `go-s1-deny`, `go-s1-cancel`, `go-s1-question`, `log-opencode-live-fixes-r2`, `app-mock-r2-s4x-opencode`, `app-r4-s1-opencode`.

### F18 — OpenCode native subagents and late background asks

Completed real-model S2 and S2X, late asks, wake segments, per-child/root Stop and final terminal work readback pass.

**Remaining:** Transient unknown shell resolved. Distinct replacement Foreground child after Stop is retained as model behavior; not every ownership race is tested.

Evidence: `go-s2-subagents`, `go-s2-child-stop`, `go-s2-late-ask`, `app-r4-s2-opencode`, `app-r4-s2x-opencode`, `native-opencode-work-popover`.

### F19 — OpenCode shared tool vocabulary and complete output

Paid Go full output markers/envelope/journal/document retrieval and tool-output integration pass; actual native question/transcript fixes also pass their own readbacks.

**Remaining:** Full native output detail/media/tool-vocabulary matrix remains partial; do not substitute question UI proof for output-detail acceptance.

Evidence: `go-tools-output`, `log-engine-final-extended`.

### F20 — OpenCode history forks, edit, rewind and review

OpenCode signed-app S5/S8 now add full scenario fork/edit/rewind/side-chat and review receipt/settlement acceptance to native-history probes and Claude cases.

**Remaining:** Review semantic quality and arbitrary filesystem/concurrent history races remain bounded by actual assertions.

Evidence: `go-s5-history`, `app-r2-s5-claude-code`, `log-command-stack-fixed`, `app-r3-s8-claude-code`, `app-r4-s5-opencode`, `app-r4-s8-opencode`.

### F21 — OpenCode LUNA tool delivery, commands, skills, titles

Completed OpenCode S3/S7 prove app-tool delivery fresh/resume/fork/post-compaction, scope refusal, command/skill and title checks; all-provider S9Q passes.

**Remaining:** Individual informational title/projection observations stay informational; every tool/extension is not exhausted.

Evidence: `go-s3-luna-tools`, `go-s3-child-containment`, `go-s7-discovery`, `app-r3-s9q-opencode`, `app-r3-s7-claude-code`, `app-r4-s3-opencode`, `app-r4-s7-opencode`.

### F22 — Usage history, limits and provider generation updates

OpenCode S6 now includes usage-store readback, joining usage/session/model-cache suites and primary complete-window reads.

**Remaining:** No offered variants in this model, so switch-variant remains informational/no. Same-profile stats/compaction reconciliation, Go-limit reset UI and updater generation are not fully rerun.

Evidence: `log-engine-final-extended`, `app-r2-s6-claude-code`, `read-primary-usage`, `app-r4-s6-opencode`.

### F23 — Unified cancellation, stall notice and blocking auto-answer

Both Claude and OpenCode real S9U pass recommended auto-answer, Keep waiting and unanswered taste paths; T2 stall/fault proof remains independent.

**Remaining:** Codex-specific live countdown/stall and every exact-expiry/failed-Hold race remain non-exhaustive.

Evidence: `log-engine-fixes-final`, `app-r2-s1-claude-code`, `go-s1-cancel`, `app-r3-s9u-claude-code`, `app-mock-r2-s4s-opencode`, `app-mock-r2-s4x-opencode`, `app-r4-s9u-opencode`.

### F24 — Nonblocking ask_user cards and answer amendments

S9Q passes on all three real providers: Claude/Codex 25 shared decisive checks and OpenCode 26 including wire capture. Native three/16-line layout, deferred reopen/navigation and correct-chat initial+amended answers pass.

**Remaining:** Earlier quota failure is retained/superseded by fresh MiMo run. Separate size 12/20 native question flows, unsent-draft restart and simultaneous-card/timer permutations remain unclaimed.

Evidence: `app-s9q-claude-code`, `app-s9q-codex`, `app-s9q-opencode`, `log-engine-fixes-final`, `log-ui-lib-r3`, `native-question-multiline-layout`, `app-r3-s9q-opencode`, `native-question-flow`, `fixture-sidebar-ax-native`.

### F25 — Claude baseline event identity, tools, inputs, MCP and extensions

Claude baseline suites and completed live S1/S2/S3/S5/S6/S7/S8/S9U/S9Q cover broad real-provider acceptance; context roundtrip and opposite-provider children also pass.

**Remaining:** Not every hook/MCP form/extension/image negative boundary is live-certified; fixture proof and installed CLI entitlement remain distinct.

Evidence: `log-claude-provider-regressions`, `log-engine-final-extended`, `app-r2-s1-claude-code`, `app-r2-s2-claude-code`, `app-r2-s3-claude-code`, `app-r3-s7-claude-code`, `app-r3-s8-claude-code`, `app-r3-s9u-claude-code`, `native-context-transfer`.

### F26 — Claude history, goals/review and two-writer resume

Native Codex goal is complete with 60,000 budget and remains complete after restart; context/execution/workspace warm readbacks pass alongside live Claude writer refusal and history/review.

**Remaining:** Same-draft resume after an external writer exits and live hook-isolation are not added by these app restart checks; native workflow lanes remain cancelled.

Evidence: `live-claude-writer-refusal`, `app-r2-s5-claude-code`, `log-command-stack-fixed`, `live-codex-goal-budget`, `live-codex-goals-r2`, `app-r3-s8-claude-code`, `native-goal-restart`.

### F27 — QA isolation, signed app lifecycle and deep-link ownership

Second verified graceful restart uses same signed QA identity PID 52172; context, automations, execution, agent-workspace and completed-goal readbacks all pass.

**Remaining:** Only final artifact preservation/scoped QA cleanup and primary LUNA Dev refresh/signature/process/IPC handoff remain delivery steps; exclusions unchanged.

Evidence: `native-empty-welcome`, `fixture-startup`, `fixture-performance-10000-r2`, `qa-refresh-r3`, `native-workspace-data`, `fixture-sidebar-ax-native`, `go-batch-secret-scan`, `qa-identity-restarted`, `native-context-restart`, `native-automations-restart`, `native-execution-restart`, `native-agent-workspace-restart`, `native-goal-restart`.

### F28 — Existing test contract repairs and native test serialization

Final completed live-r4 and warm-restart results are imported; failed originals and precise superseding reasons remain.

**Remaining:** Main still owns final source/syntax/format/artifact preservation and delivery reconciliation.

Evidence: `log-engine-fixes-final`, `log-command-stack-fixed`, `log-ui-lib-r3`, `native-automations-r2`, `live-codex-goals-r2`, `app-r3-s7-claude-code`, `native-agent-workspace-restart`, `native-context-restart`.

### F29 — Documentation snapshots, retired plans and intentional exclusions

Scope remains 185 commits/29 families; fixes now include UI commit dd80eb3d and exact source/report/evidence hashes. Intentional feature exclusions unchanged.

**Remaining:** OC-16 remote/OC-18 certification, future workflow feature, unpublished updater trial and unimplemented research stay explicit. No exhaustive-all-cases claim.

Evidence: `native-empty-welcome`.

## Fix delivery ledger

Committed fixes: `916df049` backups, `a6a4a530` Claude ownership, `0e37d858` engine state/history/recovery, `8517ae33` OpenCode stream/error behavior, and `dd80eb3d` native UI/draft/AX/settings changes. The coordinator owns current remote verification and final remaining script/document delivery.

| Fix | Families / revision | Scoped verification status |
| --- | --- | --- |
| FIX-BACKUP — Accept schema 18 and retain automation tables through encrypted backup/restore | F08, F22, F27; `916df049` | committed pushed integration verified |
| FIX-CLAUDE-OWNERSHIP — Guard cold resume and history controls using correct provider home and full writer list; scrub identity and hooks | F25, F26; `a6a4a530` | committed pushed integration and live refusal verified |
| FIX-ENGINE-INPUTS — Retain carried automatic-answer deadline/hold and publish parked async card once | F23, F24; `0e37d858` | integration verified native partial |
| FIX-WORKSPACE-ROOT — Keep workspace tools available for surviving fork after source deletion | F03, F05; `0e37d858` | integration verified native partial |
| FIX-FAILED-CHILD — Report failed delegated child as failed/attention-required | F07; `0e37d858` | integration verified native partial |
| FIX-ARCHIVE-WORK — Refuse archive and idle-writer retirement while native background work remains live | F03, F07; `0e37d858` | integration verified native partial |
| FIX-SIDE-STOP — Legacy StopSideChat retains ordinary recovery hold across restart | F01, F03; `0e37d858` | integration verified native partial |
| FIX-HISTORY-SEGMENTS — Validate split-turn terminal checkpoints for rewind/file preview/native side fork; keep OpenCode compacted history supported | F04, F20, F26; `0e37d858` | integration verified native partial |
| FIX-COMMAND-STACK — Box one large command-intent future to preserve default-stack goal resume | F01, F11, F26; `0e37d858` | committed pushed default stack verified |
| FIX-OPENCODE-RECOVERY — Bound empty SSE retries; fence completed snapshots; reconcile request ownership and parked wakes | F15, F17, F18; `8517ae33` | integration verified native partial |
| FIX-OPENCODE-ERROR — Emit a terminal provider failure once; keep recovered step failure transient | F15, F19; `8517ae33` | integration verified native partial |
| FIX-SETTINGS-CATALOG — Invalidate retained Providers/Projects/title model lists on Refresh models | F12, F13, F14; `dd80eb3d` | committed native add model refresh verified remaining permutations open |
| FIX-STARTUP-ORDER — Reconcile initial splash when Shell starts after readiness | F03, F27; `dd80eb3d` | fixture and native empty welcome verified |
| FIX-UI-QUESTION-STATE — Preserve draft/amendment navigation, scope confirmed Hold receipts, label legacy answered cards, cancel timer on click | F23, F24; `dd80eb3d` | committed native draft navigation amendment verified |
| FIX-PERF-FIXTURE — Separate byte/count cache rejection and bounded positive-retention probe without changing production ceilings | F09, F28; `dd80eb3d` | native fixture verified |
| FIX-UI-AX — Expose task/project/question semantics and preserve cached-sidebar AX subtree across frames | F03, F10, F12, F24; `dd80eb3d` | committed native rendered ax and real app readback verified |
| FIX-UI-MULTILINE — Bound answer editor to six scaled lines, grow within cap and refresh per-page hints | F24; `dd80eb3d` | committed native three and sixteen line wide narrow verified |
| FIX-SCENARIO-RUNNER — Permit explicit-model Codex S9Q T3 only; request blocking tools for blocking scenarios; wake reply waits on session changes | F23, F24, F28; runner working tree | corrected blocking question skill path and waiter acceptance verified |
| FIX-AUTOMATION-READBACK-RUNNER — Boundedly wait for settled transcript after occurrence completion before asserting assistant marker | F08, F28; runner working tree | fresh live rerun verified |
| FIX-GOAL-BUDGET-RUNNER — Make finite native goal fixture budget configurable without weakening original checks | F11, F26, F28; runner working tree | fresh live rerun verified |

The native Settings add-model case passes but does not close all catalogue-removal/concurrent-save cases. Actual native question/AX/layout failures now have matching acceptance; size 12/20 and unsent-draft restart remain explicitly unclaimed. The automation transcript-read and goal-budget fixes are fixture/runner corrections, not scheduler/budget-policy repairs.

## Intentional exclusions and next reconciliation

- **D01: Claude native workflow live QA, workflow Stop, post-turn workflow approvals.** Explicit 2026-10-08 decision d3819203; future LUNA workflow feature is separate.
- **D02: OpenCode OC-16 remote-host parity.** Open pending package; current local acceptance is not remote certification.
- **D03: OpenCode OC-18 full certification.** Audit can add evidence but package is not complete until all certification gates have receipts.
- **D04: Control this Mac M0-M4 and new agent-workspace research ideas.** Plan/research only; do not report as regressions in merged implementation.
- **D05: Two-version public GitHub updater trial.** Existing plan blocked by private repository/no published releases; not authorized to publish.
- **D06: QA wrapper integration verifier and build-cache verifier.** Create worktrees or extra target directories; excluded by no-worktree/no-storage-heavy constraint.

The completed `live-r4` batch, resumed agent-workspace and all reported warm-restart workflows are now imported. Remaining delivery work is final evidence preservation/checksums, safe QA quit/scoped bundle cleanup and primary **LUNA Dev** refresh/process/IPC/signature handoff. The main owner will append that final delivery evidence. No exhaustive-all-cases certification is claimed.

When appending: inspect completed decisive criteria, retain informational/skipped rows, record source/binary/app identities and artifact hashes, update only the affected family gap/fix state, and retain the failed original attempt with a precise superseding reason. For restart, distinguish preserved held work from actual execution. For compaction, distinguish admission/refusal from a completed lifecycle.

## Final coordinator delivery

All code and QA runner changes were pushed and remote-verified through `8c3ca759`.
The final complete Rust summaries total **1,767 passing tests across 59 targets**,
with **10 existing ignored tests** still ignored. Final runner syntax, Rust format
and Clippy checks passed; Clippy retains pre-existing warnings.

LUNA Dev was refreshed successfully in **4 seconds** of build time (5 seconds
for the refresh). Its exact bundle process was PID **11363**, listening on
**59777**; strict deep signature verification passed. The original primary data
directory and `luna://` handler were preserved. Its native AX tree shows the
loaded workbench. `primary-handoff-pre-report.json` preserves this check;
`primary-handoff.json` holds the last readback after committing this report.

The QA app was idle, quit gracefully, unregistered and its exact bundle removed.
Both audit-owned mock servers were stopped. The temporary resource helper was
preserved with the evidence and removed from its disposable location. **29**
scenario directories were archived, **1,718** file hashes checked, and only those
closed disposable directories removed. QA data, native provider evidence, the
primary app and shared Cargo cache remain. The unrelated pending research folder
was untouched. The artifact secret scan found zero findings in **2,318** files;
`SHA256SUMS` is regenerated and verified after the final handoff.

Disk review: `target/debug` was about **89 GiB**, aggregate accounted usage
**112.39 GiB** against the 170 GiB operating ceiling, with approximately
**301 GiB** free. No broad cache cleanup or additional Rust worktree was needed.

The preserved evidence root is `target/qa/claude-review-20261008/`; see
`cleanup.json`, `disposable-scenarios-cleanup.json`, `qa-final-acceptance.json`,
`primary-handoff-pre-report.json` and the final `primary-handoff.json`.
