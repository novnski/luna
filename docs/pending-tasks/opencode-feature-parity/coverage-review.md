# OpenCode v2 parity: coverage review

Research date 2026-10-06/07. This document accounts for every relevant OpenCode v2 docs section, public API and event family, and capability category from the existing Codex and Claude Code parity inventories. Each item maps to a matrix row in [capability-inventory.md](capability-inventory.md) (`F-…`), a work package (`OC-…`), a scenario (`OCE-…`), or an explicit exclusion with a reason. It also records product decisions, the separate omissions review (§8) and the unresolved-questions ledger (§9).

## 1. Method and completeness statement

- **Native inventory:** workstream A crawled all **63** v2 docs pages (HTTP 200, retrieved 2026-10-06 20:31–20:32Z) and the v2.0.22 source, giving 183 native rows (A-…).
- **Protocol:** workstream B covered the 136 HTTP operations (v2.0.22 OpenAPI) and all **94** event types.
- **LUNA:** workstream C traced 18 representative features through LUNA.
- **Prior parity:** workstream D extracted **524** rows from the prior parity inventories: 465 requirement-bearing plus 59 reference-implementation rows.
- **Coordinator:** independently re-checked the G-0 boot blocker, the request-body contracts (OpenAPI per tag), restart-claim ownership, `mcp.add` semantics, revert `files`, and the Form schema.

The coverage is complete for every docs page, API family, event type and prior-inventory category listed below. The row-level reconciliation of the 524 prior-parity rows and the independent omissions review are in §8. Coverage is **not** a claim of verification: every row stays `unverified` until the scenarios in [validation-plan.md](validation-plan.md) produce retained artifacts.

## 2. Native inventory → matrix (all 183 rows)

Script-checked: every A row maps to at least one matrix row. A-ADV-05 (warming) was folded into F-MOD-08.

| A category | A row → matrix row(s) |
| --- | --- |
| SES (18) | SES-01→F-SES-01; SES-02→F-SES-02; SES-03→F-SES-01; SES-04→F-SES-03; SES-05→F-SES-05; SES-06→F-SES-06; SES-07→F-SES-07; SES-08→F-SES-08; SES-09→F-SES-09; SES-10→F-SES-10; SES-11→F-SES-11; SES-12→F-SES-12; SES-13→F-SES-13; SES-14→F-SES-14; SES-15→F-SES-15; SES-16→F-SES-16; SES-17→F-SES-17; SES-18→F-SES-18 |
| INP (14) | INP-01→F-INP-01/F-INP-02/F-INP-03; INP-02→F-INP-04; INP-03→F-INP-05; INP-04→F-INP-05; INP-05→F-INP-07; INP-06→F-INP-08; INP-07→F-INP-09; INP-08→F-INP-10; INP-09→F-INP-11; INP-10→F-EXT-08/F-INP-07; INP-11→F-INP-12; INP-12→F-INP-13; INP-13→F-INP-14; INP-14→F-INP-14 |
| TURN (10) | TURN-01→F-INP-03; TURN-02→F-TURN-02/F-TURN-03; TURN-03→F-TURN-04; TURN-04→F-TURN-05; TURN-05→F-TURN-06; TURN-06→F-TURN-07; TURN-07→F-TURN-08; TURN-08→F-TURN-09; TURN-09→F-SES-15; TURN-10→F-TURN-11 |
| OUT (9) | OUT-01→F-OUT-01; OUT-02→F-CLIENT-02; OUT-03→F-OUT-02; OUT-04→F-OUT-04; OUT-05→F-OUT-05; OUT-06→F-OUT-06; OUT-07→F-OUT-07; OUT-08→F-OUT-08; OUT-09→F-OUT-09 |
| TOOL (21) | TOOL-01→F-TOOL-01; TOOL-02→F-TOOL-02; TOOL-03→F-TOOL-03; TOOL-04→F-TOOL-04; TOOL-05→F-TOOL-05; TOOL-06→F-TOOL-06; TOOL-07→F-OUT-03/F-TOOL-07; TOOL-08→F-TOOL-08; TOOL-09→F-TOOL-09; TOOL-10→F-TOOL-10; TOOL-11→F-EXT-08/F-TOOL-11; TOOL-12→F-AGT-06/F-TOOL-12; TOOL-13→F-TOOL-13; TOOL-14→F-SES-07/F-TOOL-14; TOOL-15→F-TOOL-14; TOOL-16→F-TOOL-15; TOOL-17→F-TOOL-16; TOOL-18→F-TOOL-17; TOOL-19→F-TOOL-18; TOOL-20→F-TOOL-19; TOOL-21→F-TOOL-20 |
| MOD (16) | MOD-01→F-MOD-01; MOD-02→F-MOD-02; MOD-03→F-MOD-03; MOD-04→F-MOD-04; MOD-05→F-MOD-04; MOD-06→F-MOD-04; MOD-07→F-MOD-05; MOD-08→F-MOD-05; MOD-09→F-MOD-06; MOD-10→F-MOD-08; MOD-11→F-MOD-09; MOD-12→F-MOD-10; MOD-13→F-OUT-07; MOD-14→F-MOD-11; MOD-15→F-MOD-12; MOD-16→F-MOD-13 |
| PERM (13) | PERM-01→F-PERM-01; PERM-02→F-PERM-01; PERM-03→F-PERM-02/F-PERM-03; PERM-04→F-PERM-04; PERM-05→F-PERM-01; PERM-06→F-PERM-05; PERM-07→F-PERM-06; PERM-08→F-PERM-07; PERM-09→F-PERM-08; PERM-10→F-PERM-09; PERM-11→F-PERM-10; PERM-12→F-PERM-11; PERM-13→F-PERM-12 |
| EXT (20) | EXT-01→F-EXT-01; EXT-02→F-EXT-01; EXT-03→F-EXT-02/F-INP-11; EXT-04→F-EXT-03; EXT-05→F-EXT-04; EXT-06→F-EXT-05; EXT-07→F-EXT-05; EXT-08→F-EXT-05; EXT-09→F-EXT-06; EXT-10→F-EXT-05; EXT-11→F-EXT-07; EXT-12→F-EXT-07; EXT-13→F-EXT-08; EXT-14→F-EXT-09; EXT-15→F-EXT-10/F-INP-11; EXT-16→F-EXT-11; EXT-17→F-EXT-13; EXT-18→F-EXT-13; EXT-19→F-EXT-14; EXT-20→F-EXT-15 |
| AGT (13) | AGT-01→F-AGT-01; AGT-02→F-AGT-02; AGT-03→F-AGT-03; AGT-04→F-AGT-03; AGT-05→F-AGT-04; AGT-06→F-AGT-05; AGT-07→F-AGT-05; AGT-08→F-AGT-06; AGT-09→F-AGT-07; AGT-10→F-AGT-08; AGT-11→F-AGT-09; AGT-12→F-AGT-10; AGT-13→F-AGT-11 |
| ADV (8) | ADV-01→F-ADV-01; ADV-02→F-ADV-02; ADV-03→F-ADV-03; ADV-04→F-OUT-06; ADV-05→F-MOD-08; ADV-06→F-ADV-04; ADV-07→F-ADV-05; ADV-08→F-AGT-02 |
| WS (14) | WS-01→F-WS-01; WS-02→F-WS-02; WS-03→F-WS-03; WS-04→F-WS-04; WS-05→F-WS-05; WS-06→F-WS-06; WS-07→F-WS-07; WS-08→F-WS-08; WS-09→F-WS-08; WS-10→F-WS-08; WS-11→F-WS-09; WS-12→F-WS-10; WS-13→F-WS-10; WS-14→F-WS-10 |
| HOST (12) | HOST-01→F-HOST-01; HOST-02→F-HOST-02; HOST-03→F-HOST-03; HOST-04→F-HOST-01; HOST-05→F-HOST-04/F-SES-17; HOST-06→F-HOST-05; HOST-07→F-HOST-06; HOST-08→F-WS-03; HOST-09→F-HOST-07; HOST-10→F-HOST-07; HOST-11→F-HOST-08; HOST-12→F-HOST-09 |
| CLIENT (15) | CLIENT-01→F-CLIENT-01; CLIENT-02→F-CLIENT-01; CLIENT-03→F-CLIENT-01; CLIENT-04→F-CLIENT-01; CLIENT-05→F-CLIENT-01; CLIENT-06→F-CLIENT-02; CLIENT-07→F-CLIENT-02; CLIENT-08→F-CLIENT-03; CLIENT-09→F-CLIENT-04/F-WS-03; CLIENT-10→F-CLIENT-05; CLIENT-11→F-CLIENT-06; CLIENT-12→F-CLIENT-06; CLIENT-13→F-CLIENT-07; CLIENT-14→F-CLIENT-01; CLIENT-15→F-CLIENT-01 |


## 3. Official documentation coverage (63 pages)

| Docs page (`https://opencode.ai/v2/docs/…`) | Matrix rows |
| --- | --- |
| Intro `/` | F-HOST-07 (distribution), F-WS-01; stale 2.0.6 links noted (A §8 #16) |
| Config `config/` | F-MOD-12, F-WS-05 (worktrees), F-OUT-02 (`tool_output`), F-HOST-05 (`update`), F-SES-17 (`share`), F-EXT-09 (`instructions` ignored) |
| Migrate from V1 `migrate-v1/` | F-HOST-08, F-SES-18, F-TOOL-20, F-EXT-09/13, F-ADV-04 |
| Troubleshooting `troubleshooting/` | F-SES-01 (DB path), F-WS-01, F-CLIENT-01 (diagnostics) |
| Agents `agents/` | F-AGT-01..05, F-MOD-10 |
| Models `models/` | F-MOD-01..04, F-OUT-01 |
| Skills `skills/` | F-EXT-08, F-INP-07 |
| Themes `themes/` | F-CLIENT-01 (n/a) |
| Commands `commands/` | F-INP-11, F-EXT-02/10, F-AGT-08, F-ADV-01 |
| Plugins `plugins/` | F-EXT-05 |
| Providers `providers/` | F-MOD-04/09, F-TURN-06 |
| Websearch `websearch/` | F-TOOL-09, F-EXT-14 |
| Network `network/` | F-MOD-09 (proxy/CA env; n/a) |
| Snapshots `snapshots/` | F-SES-16 |
| Compaction `compaction/` | F-MOD-06, F-TURN-07 |
| Formatters `formatters/` | F-EXT-13 |
| References `references/` | F-INP-08 |
| Attachments `attachments/` | F-INP-05/06 |
| Tools `tools/` | F-TOOL-01..19 |
| MCP servers `mcp-servers/` | F-EXT-01..04, F-TOOL-15/17, F-PERM-08 (elicitation undocumented) |
| Permissions `permissions/` | F-PERM-01..05, F-PERM-09, F-PERM-11/12, F-TURN-11 |
| Policies `policies/` | F-PERM-06, F-MOD-13 |
| Instructions `instructions/` | F-EXT-09, F-INP-12 |
| Sharing `sharing/` | F-SES-17, F-HOST-04 |
| Warming `warming/` | F-MOD-08 |
| CLI Intro `cli/` | F-WS-01/02, F-CLIENT-01 |
| CLI TUI `cli/tui/` | F-CLIENT-01, F-INP-01/03/14, F-OUT-06 |
| CLI Settings `cli/config/` | F-CLIENT-01 (cli.json), F-INP-14, F-OUT-01 |
| CLI Web `cli/web/` | F-WS-02/03 |
| CLI Providers `cli/providers/` | F-MOD-05, F-SES-18 |
| CLI Commands `cli/commands/` | F-SES-02/09/10, F-CLIENT-02, F-MOD-11, F-HOST-05 |
| CLI ACP `cli/acp/` | F-CLIENT-05, F-AGT-10 |
| CLI Theme `cli/theme/` | F-CLIENT-01 (n/a) |
| CLI Plugins `cli/plugins/` | F-EXT-07 (n/a) |
| CLI Keybinds `cli/keybinds/` | F-CLIENT-01 (n/a) |
| Build Intro / Plugins Overview / RPC / CLI / Migrate v1 `build/…` | F-EXT-05/06/07, F-PERM-10, F-TURN-06 (retry hook) |
| Build Client JavaScript `build/client/` | F-CLIENT-07, F-LIFE-05 ("live-only, no replay") |
| Build SDK Overview / Cloudflare `build/sdk/…` | F-CLIENT-07, F-HOST-07 |
| Build Effect Plugins / RPC / Client / SDK `build/…/effect/…` | F-EXT-05/06, F-CLIENT-07 |
| API reference `api/` (29 families, below) | §4 |
| Console Intro / Models / Providers / Web Search / Go `console/…` | F-HOST-01/02/03, F-EXT-14 |
| Console Workspace: Basics, Members, SSO, SCIM, Budgets, Billing | F-HOST-03 (gated, out of band) |
| Console API: Overview, Inference, Providers, Budgets | F-HOST-03 (gated) |

All 63 pages are accounted for. The multi-page rows above group pages with one disposition.

## 4. Public API family coverage (v2.0.22; families from the docs sidebar)

Operation counts depend on the source:
- The committed `packages/protocol/openapi.json` @`527f0b93` has **136** operations.
- The 2.0.22 source groups define **140**: they add `server.pair`, `server.connect`, `credential.list` and `credential.create`.
- The docs sidebar shows **141**.

The counts below follow the docs sidebar. Committed-JSON differences: server 1, credential 3, vcs 5. The coordinator's per-tag OpenAPI checks used the committed artifact, which may lag the source; OC-00 captures the live `/openapi.json` as the fingerprint baseline.

| Family (ops) | Matrix rows | LUNA use today | Target WP |
| --- | --- | --- | --- |
| server (3: `info`, `pair`, `connect`) | F-LIFE-01, F-WS-03 | none (`/api/health` used instead) | OC-02 |
| location (2) | F-WS-04, F-MOD-11 | none | OC-14 |
| agent (2) | F-AGT-05 | none | OC-14 |
| plugin (3) | F-EXT-05 | none | OC-14 (P3) |
| session (44): create, list, get, update, remove, fork, move, prompt, command, synthetic, shell, compact, interrupt, background, wait, revert ×3, context, diff, messages ×2, inbox ×3, permission ×4, form ×5, generate, log, view, environment, active, switch agent/model, skill, instructions ×3, export/import, stats | F-SES-*, F-INP-*, F-TURN-*, F-PERM-*, F-OUT-04/06, F-MOD-02/06 | create, get, update, prompt, command, interrupt, compact, active, messages, fork, permission reply, switch model | OC-02..OC-15 |
| model (2) | F-MOD-01 | `model.list` | OC-10 |
| generate (1) | F-OUT-06 | none | OC-15 (P2) |
| provider (2) | F-MOD-01/04 | none on v2 | OC-10 |
| integration (11), credential (5) | F-MOD-05 | none | OC-16 (read-only) |
| mcp (6) | F-EXT-01/03, F-EXT-12 | none | OC-13, OC-14 |
| project (2) | F-WS-04 | none | — |
| form (1, global list) | F-PERM-07/08 | none | OC-05 |
| permission (7) | F-PERM-03/04/13 | reply only | OC-04 |
| filesystem (4) | F-WS-08 | none | n/a |
| command (1) | F-INP-11 | `command.list` | OC-14 |
| skill (1) | F-EXT-08 | none | OC-14 |
| rpc (1) | F-EXT-06 | none | n/a |
| event (1) | F-LIFE-04 | yes | OC-08 |
| pty (7), persistentPty (11), shell (5) | F-WS-08, F-ADV-03 | none | n/a; shell jobs OC-15 |
| reference (1) | F-INP-08 | none | OC-14 (P2) |
| worktree (4), vcs (6) | F-WS-05/06 | none | observe only (PD-5) |
| debug (2) | F-WS-04 (location eviction) | none | n/a |
| migration (1) | F-SES-18 | none | OC-00 (hazard notice only) |
| websearch (2) | F-EXT-14 | none | n/a (native tool) |
| config (3) | F-MOD-11/12 | none | OC-14 |

The **94 event types** are each listed with today's handling and target handling in [tool-and-event-matrix.md](tool-and-event-matrix.md) §5. The **content families**, with their matrix rows:

| Family | Matrix rows |
| --- | --- |
| text and reasoning parts | F-OUT-01 |
| tool input, called, progress, success content (text, file, image), failed | F-OUT-02/03/05, F-TOOL-* |
| prompt `files`, `agents`, `skills` | F-INP-05/07 |
| form fields: string, number, integer, boolean, multiselect, external | F-PERM-07 |
| usage and cost | F-OUT-07 |
| structured errors | F-TURN-12 |
| diffs (`FileDiff`) | F-OUT-04 |
| inbox items (user, synthetic, compaction, move) | F-INP-02/09, F-SES-07 |

## 5. Product decisions applied

| # | Decision | Source | OpenCode consequence (rows) |
| --- | --- | --- | --- |
| PD-1 | Dedicated Plan/Build UI and replacement `/plan` cancelled (2026-09-30) | `codex-feature-parity/README.md:18,70,193-195`; `claude-code-feature-parity/README.md:67` | `plan` agent inventoried (F-AGT-02); any LUNA plan surface is `excluded (PD)`; imported plan-agent sessions handled safely (OC-12, OC-14; OCF-94/105) |
| PD-2 | Native steering only; never interrupt-restart; follow-ups default to Queue | `codex-feature-parity/README.md:13,134-137`; `docs/reference/message-queue.md:33-36` | OpenCode native `delivery:"steer"` adopted only with inbox evidence (OC-07) |
| PD-3 | Use installed integrations; no plugin or marketplace management in the picker | `codex-feature-parity/README.md:15,150,264` | Read-only MCP/plugin inventory at most (F-EXT-05) |
| PD-4 | Everyday capabilities first; terminal themes and unrelated internals don't define the backlog | `codex-feature-parity/README.md:17` | CLIENT rows `n/a` |
| PD-5 | No worktree creation by LUNA; observe native transitions | `claude-code-feature-parity/README.md:69` | F-WS-05 excluded for creation; F-SES-07 observe and re-validate |
| PD-6 | Native subagents ≠ LUNA child chats | `claude-code-feature-parity/current-gap-assessment.md:56` | F-AGT-06 vs F-AGT-12/13 |
| PD-7 | Child placement is presentation-only; parent-agent creation; cross-provider children start fresh | `docs/reference/chat-placement-and-child-models.md:5-24` | F-AGT-13 |
| PD-8 | In-app coordination asks no extra cards; Full never parks; no persistent "always" | `docs/design/agent-workspace-controls/README.md:50-56`; `approval-failures.md` | OC-04 mode compilation |
| PD-9 | Unavailable saved model blocks sending; no silent alternative | `docs/design/agent-workspace-controls/README.md:19-23` | OC-10 (OCF-72) |
| PD-10 | Advertise tools only when actually delivered | `docs/pending-tasks/agent-created-child-threads.md:186,261` | OC-13 gating |
| PD-11 | Reuse shared surfaces; no duplicate composers, child systems, viewers or approval dialogs | `claude-code-feature-parity/current-gap-assessment.md:3` | All packages |
| PD-12 | No orchestration resumption, credential changes or cache cleanup as part of parity work; no credential-store reads | `claude-code-feature-parity/HANDOVER.md:9` | F-MOD-05/14 (the existing Go-limits reader is kept by PD-17; no new credential reads) |
| PD-13 | Hosted and external services are separate certification lanes | `claude-code-feature-parity/README.md:47` | F-HOST-* gated rows |
| PD-16 | File-restore button for OpenCode deferred to a later phase (user, 2026-10-07) | this conversation | OC-12 step 4 deferred; F-SES-16 |
| PD-17 | Keep the OpenCode Go usage-limits reader that reads OpenCode's saved key (user, 2026-10-07) | this conversation | F-MOD-14; OC-16 step 2 closed; no new credential reads elsewhere |
| PD-18 | Real-AI testing authorized for all scenarios, including the Claude comparisons (user, 2026-10-07) | this conversation | Every OCE needs a T3 pass; T2 kept for fault injection; account rules in validation-plan §1 item 6 |
| PD-15 | **Drop OpenCode v1 (1.18) support (user, 2026-10-07).** LUNA runs OpenCode 2 only. | this conversation | Resolver skips v1 binaries; clear "OpenCode 2 required" state; the v1 wire and v1 tool names are removed (OC-00, OC-02, OC-06). F-HOST-08 and the v1 parts of F-TOOL-20 become `excluded (PD)`. |
| PD-14 | **Parity scope (user, 2026-10-07):** OpenCode should behave like the other providers through LUNA's existing shared mechanisms. Don't tailor features that exist only in OpenCode. Subagents, side chats and child chats must work the way they do for Codex and Claude. | this conversation; [provider-parity-contract.md](provider-parity-contract.md) | 14 matrix rows marked *not pursued*; OC-11..OC-16 re-scoped to Claude/Codex hooks; OCE-19..24 re-scoped or added |

Full list with dates: D §6.

## 6. Reconciliation with the Claude Code parity inventory

| Claude unit | OpenCode equivalent | Disposition |
| --- | --- | --- |
| CC-00 Baseline and executable conformance | OC-00, OC-01 | applies (v1/v2 split is the key case) |
| CC-01 Transport decision | OC-02 (F-LIFE-10) | applies; **decision recorded: keep HTTP+SSE**, launch `--stdio` |
| CC-02 Lossless events and unknown-frame retention | OC-06, OC-08 | applies |
| CC-03 Permissions, questions, dialogs, cancellation | OC-04, OC-05, OC-07 | applies |
| CC-04 All-turn image input | OC-09 | applies |
| CC-05 Complete tool results and structured output | OC-06 | applies; structured output `unavailable` natively (F-OUT-08) |
| CC-06 Effective models, usage, truthful delivery | OC-10, OC-07 | applies |
| CC-07 Callable LUNA/Executor bridge | OC-13 | applies; native runtime `mcp.add` plus instruction entries |
| CC-08 Native agents and background task graph | OC-11, OC-15 | applies (`subagent` tool, background jobs) |
| CC-09 Persistent LUNA children and native peer messages | OpenCode is **already a child destination** (`cross_task_tools.rs:33-37` @`36cd741a`); as a parent via OC-13, like Claude. Peer messaging `unavailable` (F-AGT-11). | applies (children) / unavailable (peers) |
| CC-10 Native history and file checkpoints | OC-12 | applies; file checkpoints deferred (PD-16) |
| CC-11 MCP inventory, resources, apps | OC-14: Claude-style `native_extensions` / `session_mcp_observation` (read-only); OC-06 resources; MCP apps/widgets `unavailable` | partial |
| CC-12 Skills, plugins, hooks, memory, styles | OC-14 (skills, commands, plugins read-only); hooks native-only (F-EXT-05); memory `unavailable` (F-EXT-11); output styles n/a | partial |
| CC-13 Workflows, teams, channels, external sessions | teams/channels `unavailable` (F-AGT-11); external sessions → native import (OC-12, F-SES-02) | partial |
| CC-14 Goals and scheduling | `unavailable` natively (F-ADV-04); LUNA gate stays | excluded (native gap) |
| CC-15 Review and structured findings | OC-15: like Claude's command-line review (`/review` ↔ `/code-review`); no structured findings, like Claude | applies (Claude parity) |
| CC-16 Cloud, remote, browser, computer integrations | F-HOST-01..04, F-TOOL-16, F-WS-02/03, F-SES-17: share `unavailable`; desktop browser `gated`; Console `gated` | gated or unavailable |
| CC-17 Operational compatibility and accounts | OC-17, OC-16, OC-02 compatibility contract | applies |
| CC-18 Certification and delivery | OC-18 | applies |
| G1 Discovery ≠ execution config | OC-00/02/14 (discovery in the run location; same binary) | applies (F-MOD-12, OCF-04) |
| G2 Result bodies dropped | OC-06 (4 KiB cap) | applies |
| G3 Root completion vs foreign results | OC-07 (inbox correlation, `ProviderTurnStarted`) | applies |
| G4 No callable LUNA tools | OC-13 | applies |
| G5 Rich approvals and dialogs | OC-04, OC-05 | applies |
| G6 Image and selection holes | OC-09 | applies |
| G7 Agent and task visibility | OC-11 | applies |
| G8 History, reset, context lifecycle | OC-12 (history), OC-10 (context and compaction). **Reset and prompt suggestions are `unavailable`:** no native operation among the operations and no event among the 94. This covers CCI-42, CCM-17 and CCM-18. | applies (reset unavailable) |
| G9 Goals, scheduling, findings | F-ADV-01/04 | partial or unavailable |
| G10 Mixed results, resources, widgets | OC-06 | applies |
| G11 Effective settings, extensions, accounting | OC-10, OC-14 | applies |
| G12 Hosted features; external background sessions | OC-16; F-SES-02 import; F-SES-15 hazard | applies |
| E01–E28 scenarios | OCE-00..24 (see the table below) | mapped |
| CCF-01..15 failure cases | OCF-01..168 (superset) | mapped |
| Certification ledger CERT13-01..13, CERT16-01..10 (Claude-specific) | OC-18 creates an OpenCode ledger. Claude workflow and service lanes have no OpenCode counterpart, except the provider-neutral rules: CERT13-03 sticky terminal → OCE-02 (terminal unchanged); CERT13-07 parked-gate auto-decline → OCE-05 (c); CERT13-11 external sessions → F-SES-02; CERT13-12 two-writer protection → OCF-15 / OCE-19. | n/a except as listed |

**Claude scenarios → OpenCode scenarios.**

| Claude | OpenCode | Claude | OpenCode |
| --- | --- | --- | --- |
| E01 | OCE-00, 12 | E15 | 10 |
| E02 | 13, 20 | E16 | 22 |
| E03 | 03, 09, 12 | E17 | 20, 22 |
| E04 | 06 | E18 | n/a |
| E05 | 02, 06, 22 | E19 | 23 |
| E06 | 16, 05 | E20 | 23 |
| E07 | 05, 16 (no OS sandbox) | E21 | n/a or gated |
| E08 | 08 | E22 | 04, 07, 09, 12 |
| E09 | 06, 22 (no notebook/LSP natively) | E23 | 12, 13 |
| E10 | 13, 14, 07 | E24 | 01, 17 |
| E11 | 17 | E25 | 01 |
| E12 | 18 | E26 | 08, 20 |
| E13 | 01, 21 | E27 | 15, 23 |
| E14 | 15 | E28 | 06, 22 |

## 7. Reconciliation with the Codex parity inventory

| Codex unit | OpenCode equivalent | Disposition |
| --- | --- | --- |
| CXM-01 Session start, resume, import, fork | F-SES-01/02/04/06 → OC-03, OC-12 | applies |
| CXM-02 Model, reasoning, service-tier choices | F-MOD-01..03 → OC-10 (service tier = provider option, n/a) | applies |
| CXM-03 Native steering (accepted vs applied) | F-INP-02/03 → OC-07 | applies (OpenCode has native inbox steering) |
| CXM-04 Durable queue | shared; F-INP-02 → OC-07 | applies |
| CXM-05..08 Typed read/search/list, file changes, command output, web | F-TOOL-01..09, F-OUT-02/03/04 → OC-06 | applies |
| CXM-09/10 Image output and input | F-OUT-05, F-INP-05/06 → OC-06, OC-09 | applies |
| CXM-11/12 MCP lifecycle and elicitation | F-TOOL-17, F-PERM-08 → OC-06, OC-05 | applies (elicitation exists natively, undocumented) |
| CXM-13 LUNA dynamic tools | F-EXT-12 → OC-13 | applies |
| CXM-14 Plugin/app mention identities | Codex-specific | n/a (OpenCode equivalents: agent mentions and skills, F-INP-07) |
| CXM-15 Explicit skill selection | F-INP-07, F-EXT-08 → OC-09, OC-14 | applies |
| CXM-16 Plan/Build | F-AGT-02 | excluded (PD-1) |
| CXM-17 Native review | F-ADV-01 → OC-15 (like Claude's command-line review) | applies (Claude parity) |
| CXM-18 Same-thread revert | F-SES-16 → OC-12 | applies (`files:false`) |
| CXM-19 Goals | F-ADV-04 | unavailable natively |
| CXM-20 Rich permission decisions | F-PERM-02/03/15 → OC-04 | applies |
| CXM-21 Mermaid rendering | F-OUT-09 | n/a (LUNA-owned renderer) |
| CXS-1..5 Implementation sequence | OC-09, OC-06, OC-07, OC-15/12, OC-05 | mapped |
| CXP-1..6 Preserve behaviours | OC-06 (typed chips, per-file ids, completion after steer split, images, reopen) | applies |
| CXL-1..6 Concrete losses | OC-06 (cap, progress, MCP text-only, unknown fallback, web sources), OC-13 (dynamic lifecycle) | applies |
| CXT-1..14 Steering contract | OC-07. CXT-1 (expected-turn fence): no OpenCode equivalent; correlation is by inbox id instead. CXT-13 (`instant_interrupt`): Codex-only, n/a. | applies with noted exceptions |
| CXI-1..11 Plugin/app/skill catalogs | CXI-1 n/a (Codex concepts); CXI-2..11 patterns → OC-14 (commands, skills, agents catalogs; cache scope; older-CLI fallback = OC-02 compatibility) | applies (pattern) |
| CXW-1..10 Other workflows | CXW-1 PD-1; CXW-2 OC-15; CXW-3 OC-05; CXW-4 OC-04; CXW-5 OC-12; CXW-6 unavailable; CXW-7 n/a; CXW-8 OC-11 (background terminals as shared `NativeTaskChanged{Shell}` rows) and OC-10 (live settings, like Claude `can_reconfigure_live`); CXW-9 memory unavailable; CXW-10 n/a | mapped |
| CXE-01..18 E2E scenarios | 01→OCE-06; 02/03/04→06; 05→06; 06→08; 07→22; 08→18; 09–12→17; 13→03; 14→20; 15→08/11; 16→20; 17→10 (plan excluded)/23; 18→23 (unavailable) | mapped |
| Codex supporting audits | **Requirement-bearing rows (72)** are mapped one by one in [notes/E-omissions-review.md](notes/E-omissions-review.md) §3: CXA (19, LUNA tools), CXG (12, goals), CXTS (11, T3 integration requirements), CXNS (6, native steering), CXSS (16, steering status), CXGV (8, goal validation). Only the **BB/T3/competitor observation rows (45)** are reference-only (folded into D §4 and the OC-07/OC-13 patterns). | mapped (E §3) / reference |

**`docs/PARITY.md`** (54 rows, native-rewrite checklist): provider-agnostic shared surfaces (composer, transcript, accounts, terminal, changes, connection), already counted as LUNA-owned reuse targets. §4 has **no OpenCode harness row** (Q-4; OC-18 adds one). D's historical item 15 in its §6 (token-usage display dropped) is stale and superseded by the context ring.

## 7a. Doc/source disagreements (A §8; docs retrieved 2026-10-06 20:31Z)

| # | Topic | Docs vs v2.0.22 source | Resolves in | Matrix row |
| --- | --- | --- | --- | --- |
| 1 | ACP terminal auth (`acp --login`) | documented; absent in 2.0.22 and 2.0.24 | head only | F-CLIENT-05 |
| 2 | ACP standard compaction updates | documented; 2.0.22 sends only a `_meta` marker | 2.0.23 | F-CLIENT-05 |
| 3 | ACP child-session updates | docs omit the `_meta["opencode/child-session-updates"]` client gate | still gated at head | F-AGT-10 |
| 4 | Skill `disable-model-invocation` | documented; 2.0.22 reads only `opencode/autoinvoke` | 2.0.23 | F-EXT-08 |
| 5 | OpenAI login method labels | docs use 2.0.24 "Codex" naming | 2.0.24 | F-MOD-05 |
| 6 | `explore` agent permissions | docs match 2.0.22; 2.0.24 also allows `shell` | docs stale vs 2.0.24 | F-AGT-03 |
| 7 | Default `external_directory: ask` | docs match 2.0.22/24; removed at head | docs stale vs head | F-PERM-01 (Q-20) |
| 8 | Compaction model | "no separate compaction model" matches 2.0.22; 2.0.23+ uses the compaction agent's model | 2.0.23 | F-MOD-06 |
| 9 | `username` | docs "not displayed" vs schema "displayed and used for telemetry identity" | — | F-HOST-06 |
| 10 | `subagent` tool params | `model` param undocumented | — | F-TOOL-12, F-AGT-09 |
| 11 | Code Mode `opencode` namespace | `models` and the MCP resource tools undocumented | — | F-TOOL-14/15 |
| 12 | MCP elicitation | undocumented; implemented through Forms | — | F-PERM-08 |
| 13 | GitHub agent | repo action runs the v1 `opencode github run`, absent from the v2 CLI | — | F-ADV-05 |
| 14 | LSP | docs: not run (matches); desktop still shows "Configured LSPs" strings | — | F-EXT-13 |
| 15 | Accepted-but-inactive config (`share`, `instructions`, `lsp`, agent `request`) | acknowledged in docs; confirmed in source | — | F-SES-17, F-EXT-09/13, F-MOD-10 |
| 16 | Download links | Intro links 2.0.6 binaries | stale docs | F-HOST-07 |
| 17 | Undocumented capabilities (restart resume budget, session rules, `serve --stdio/--service`, hidden `--yolo`/`--dangerously-skip-permissions`, Mercurial, SSH/WSL, IDE bridge, `OPENCODE_SESSION_ID`, OTLP, workspace drivers, most env vars, `vcs.init`) | source-only | — | F-SES-15, F-PERM-05, F-LIFE-02, F-WS-06/07/10, F-INP-13/14, F-HOST-06 |
| 18 | MCP sampling | not claimed; not implemented | — | F-EXT-04 |
| 19 | PDF via `read` vs attachments | both documented; the asymmetry is real | — | F-INP-05, F-TOOL-01 |

Overall, the docs track the development line. There is no versioned 2.0.22 docs snapshot, so docs-only claims about 2.0.22 are E3 at best and were checked against source wherever they drive work.

## 8. Separate omissions review

Performed after the deliverables were combined, by an independent reviewer (workstream E, [notes/E-omissions-review.md](notes/E-omissions-review.md)). It covers the row-level mapping of all 524 prior-parity checklist rows, a cross-check of the docs, API and event lists against the deliverables, ID consistency, and missing categories from the brief. Findings and the coordinator's resolutions:

**Counts.** All **524** prior-parity rows were mapped one by one ([E §3](notes/E-omissions-review.md)):
- **342 mapped.** 319 reach a scenario step; for the other 23, no step actually exercises the row yet.
- **181 excluded with a reason.** 175 had their reason stated in the deliverables. For the other 6 (CXI-10, RS-4, COR-2, CCI-42, CCM-17, CCM-18) the reviewer supplied the reason, now recorded here: reset and prompt suggestions are `unavailable` (§6 G8); CXI-10 icons are LUNA-owned; RS-4 and COR-2 are Claude-specific.
- **1 omitted:** RS-1, a defect in the shared approval card (radio role on immediate-action choices). Resolved by adding an AX-role assertion to OCE-16. The defect itself belongs to the shared surface's owner.

Coverage tables: 63/63 docs pages, 29/29 API families, 94/94 events and 183/183 native rows. The event table matches B row for row. Spot checks: 17 claims, 15 confirmed, **no P0/P1 driver refuted**.

| # | Sev | Finding | Resolution |
| --- | --- | --- | --- |
| 1 | high | Contradictory fork-boundary guidance (an assistant id vs a user id). `before` is exclusive (`projector.ts:136-138`) | Fixed: `tool-and-event-matrix.md` E15 and F-SES-06 now say `before` is the first message after the kept prefix |
| 2 | med | §7 labelled requirement-bearing Codex audit rows as reference only; Claude rows not mapped one by one | Fixed: §7 relabelled; the one-by-one mapping is in E §3 (counts above) |
| 3 | med | OCE-17 lacked the canonical steering fixtures | Fixed: steps (f)–(i) and inbox-based pass criteria added |
| 4 | med | OCE-00 and OCE-02 not named by any package; 8 owner mismatches | Fixed: OC-00/01 name OCE-00; OC-06/08 name OCE-02; OC-03, 07, 09, 11, 12, 13 acceptance updated |
| 5 | med | No scenario rendered a web result | Fixed: OCE-06 adds webfetch, mock websearch and 429 failover |
| 6 | med | A §8 disagreements not consolidated | Fixed: §7a table |
| 7 | med | Context inheritance of LUNA tools and contract by native children unaddressed | Fixed: OCE-18 child step. The decision was later made by PD-14 (Q-28 closed: refused like Codex and Claude, OC-13 step 3) |
| 8 | low | F-SES-06 "OpenAPI requires `before`" refuted | Fixed (body required; `before` optional and exclusive) |
| 9 | low | Op-count provenance (136 / 140 / 141) | Fixed: §4 and the tool-and-event-matrix version note; OC-00 captures the live `/openapi.json` |
| 10 | low | Five note questions not carried to the ledger | Fixed: Q-23..Q-27 |
| 11 | low | Head drift affecting MCP tool naming missing | Fixed: F-TOOL-17, F-EXT-12, matrix version note |
| 12 | low | OpenCode-only table omitted 4 items | Fixed: row added |
| 13 | low | OCF-15 and OCF-165 orphaned | Fixed: OCE-19 and OCE-12 steps; OC-02 step 5 |
| 14 | low | G8 "reset" and the CERT13 row | Fixed: §6 |
| 15 | low | F-INP-13 and F-AGT-01 cells inconsistent | Fixed |
| 16 | low | Provider-gate rows absent from the audit (deep link, historical usage, others) | Fixed: audit §4 rows; new F-SES-19 |
| 17 | low | Sandbox descriptor `supports_workspace_sandbox: true` untruthful on v2 | Fixed: G-19, F-PERM-01 note, OC-04 step 7 |
| 18 | low | `SHA256SUMS` claimed but absent | Fixed: generated last |
| 19 | low | RS-1 omission | Fixed: OCE-16 AX assertion |
| 20 | low | `lib.rs` line drift | No change: deliverable refs are pinned at `9b3e9a2e` (`lib.rs:228-230` there). The reviewer read the working tree (+4 uncommitted lines). Drift note added to the audit §10 and the baseline JSON. |
| 21 | low | CXE-04 stdin unaddressed | Fixed: F-TOOL-07, no stdin parameter (`shell.ts:47-60`), so `unavailable` |
| 22 | info | Auto mode classification | No change (deliverables already correct) |

**Coordinator's own completeness check.** No category from the brief lacks a matrix row, package and scenario. The remaining risk is evidentiary, not coverage: every row is `unverified`, and the questions in §9 stay open.

## 9. Unresolved-questions ledger

None of these were answered by this research. Each names the evidence that would close it.

| # | Question | Why it matters | Closes with | Owner |
| --- | --- | --- | --- | --- |
| Q-3 | Does v2.0.22 ignore or reject excess request keys (`permission`, `messageID`)? OpenAPI declares `additionalProperties:false`; runtime enforcement unknown | Decides whether today's v2 session create silently drops rules or fails outright | One T2 POST (OCE-00 / OC-03) | OC-03 |
| Q-4 | Should `docs/PARITY.md` get an OpenCode row, or is it frozen? | Completeness of the shared checklist | Doc-owner decision | user |
| Q-5 | Live confirmation of G-0 (404 / HTML / 300 s timeout) on 2.0.22 | Source-only so far | OCE-00 | OC-01 |
| Q-6 | Can the managed-service sweep resume a session while a LUNA-owned server is still running it? | Concurrent writers on one session | T2 with a managed service on isolated storage (OCE-09) | OC-08 |
| Q-7 | Stability of experimental endpoints LUNA would rely on: session log, instruction entries, `mcp.add` | Replay and tool-delivery design | Upstream roadmap plus fingerprint monitoring | OC-02 |
| Q-8 | Is `session.usage.updated` always the last assistant-shaped frame before the terminal, making the v2 checkpoint `"usage"`? | Confirms G-7's mechanism | One T2 turn plus fork | OC-12 |
| Q-9 | Does npm `opencode-ai@latest` point to v1 or v2? | Update offers for `~/.opencode/bin` | `npm view opencode-ai dist-tags` (read-only; not run here) | OC-17 |
| Q-11 | How are MCP elicitation forms owned by the `"global"` sentinel attributed to a chat? | Correct routing of elicitation | Design decision plus OCE-16 | OC-05 |
| Q-12 | Do nested tool calls inside Code Mode `execute` emit their own `session.tool.*` events? | MCP tools run through Code Mode by default | T2 with a mock MCP server (OCE-22) | OC-06 |
| Q-14 | Does the in-app login-shell probe behave in the real launchd environment as in the `env -i` timing test? | Silent v1 fallback risk | OCE-12 (b) | OC-00 |
| Q-16 | What PATH and binary does luna-headless see on remote hosts? | Remote generation selection | OCE-11/12 (d) | OC-16 |
| Q-17 | Agent and command file discovery walks to the filesystem root (source) vs the project root (docs). Do QA fixture repos under `$HOME` pick up parent `opencode.json` or agents? | QA isolation correctness | T2 with a parent-dir config probe | OC-01 |
| Q-18 | Does a background subagent's completion always start a new parent execution, and can LUNA suppress or adopt it? | Ownership of wake turns | OCE-01/21 | OC-07, OC-11 |
| Q-19 | Do instruction entries survive compaction and fork (`session.forked.instructionEntries` suggests fork does)? | LUNA runtime contract durability | OCE-14/18 | OC-13 |
| Q-20 | Which v2 default permissions apply when no LUNA rules are applied (today's broken state)? At 2.0.22: allow all, ask `external_directory` and `.env` reads; head removes the `external_directory` ask | Current safety posture of Ask mode on v2 | OCE-00 readback of `session.permissions` | OC-04 |
| Q-21 | Doc inconsistency: manual child creation (Claude reassessment) vs "users do not create children directly" (placement contract) | Shared contract hygiene | Doc-owner decision | user |
| Q-22 | Should `LUNA_QA_MCP_ISOLATION` also strip OpenCode MCP servers? | QA isolation | QA infra decision | OC-01 |
| Q-23 | Does v2 return 400 for JSON `null` bodies on `compact` (likely) and accept them on `interrupt` (likely)? | Drives G-13 and the OC-03 body fixes | One T2 call each | OC-03 |
| Q-24 | What is the v2 message-list item shape, as used by fork readback (`info.id` vs `id`)? | OC-12 readback mapper | T2 `GET …/message` capture | OC-12 |
| Q-25 | Is the hidden `summary` agent invoked anywhere in v2.0.22? | Unexpected token spend or unknown sessions | Source or T2 observation | OC-10 (low) |
| Q-26 | Is `models.opencode.ai/api.json` the same catalog as models.dev? | Catalog provenance for capability and cost data | Read-only fetch comparison | OC-10 (low) |
| Q-27 | Do Console-delivered MCP servers (`auth: console`) appear in `mcp.list`? | Read-only MCP inventory completeness | Console-connected T3 run (gated) | OC-14 (low) |

**Closed by user decision (2026-10-07):**
- **Q-2 (v1 support):** dropped (PD-15).
- **Q-13 (file-restore button):** later (PD-16).
- **Q-15 (Go-limits credential read):** keep as is (PD-17).
- **Q-10 (real-model evidence):** authorized for every scenario, including Claude comparisons (PD-18).

**Questions closed by the parity refinement (PD-14, 2026-10-07):**
- **Q-1 (custom-agent picker):** not pursued. No provider has an agent picker in LUNA. The effective agent is shown read-only and the `plan` agent is handled safely (PD-1).
- **Q-28 (LUNA tools from native subagents):** refused, matching Codex (`luna_mcp.rs:458-461`, root-thread fence) and Claude (`luna_tools.rs`, tagged subagent tool uses refused). Implemented in OC-13 and OC-04 step 9.
- **Q-13 reframed:** OpenCode file-restoring snapshots would back Claude's shared `rewind_native_files` hook (optional P2), not an OpenCode-only feature. (Later decided: deferred, PD-16.)

**Questions closed by this research** (were open in workstream notes):
- **Generation probe:** `/api/info`, from 2.0.4 onward (B §2.2; coordinator OpenAPI check).
- **Native steering:** v2.0.22 has `delivery:"steer"` and `session.inbox.*` (B §2.8).
- **Subagent tool:** named `subagent` in 2.0.22. Background completions arrive as `session.synthetic` and start a parent execution (B §2.10).
- **Revert:** `revert.stage` accepts `files` (OpenAPI, coordinator).
- **Runtime MCP:** `mcp.add` is an in-memory per-location override (`oc2:core/src/mcp/index.ts:635-639`, coordinator).
- **Forms:** field kinds are string, number, integer, boolean, multiselect and external, with conditional `when` (`oc2:schema/src/form.ts`, coordinator).
- **Shared storage:** v1 and v2 share `~/.local/share/opencode` unless the wrapper is used (`oc2:cli/src/database-path.ts`; E4 `67c960f4`).
- **Legacy status events:** `session.status` and `session.idle` have no v2.0.22 producer (B §2.7).
- **Abort cascade:** parent abort reaches foreground children but not background ones (B §2.10).
- **Restart claims:** only the managed service sweeps them (coordinator).

## 10. Parity refinement (PD-14, 2026-10-07)

After the first delivery, the user narrowed the meaning of parity: OpenCode should behave like the other providers rather than gain OpenCode-specific features. Changes made:
- **New document:** [provider-parity-contract.md](provider-parity-contract.md). It compares the shared hooks and events across Codex, Claude and OpenCode, specifies subagents, side chats and child chats against the Codex/Claude contract, and lists the OpenCode-only features that are not pursued.
- **Matrix:** 14 rows marked *not pursued*. New row F-SES-20 (side chats). F-AGT-13 corrected (OpenCode is already a child destination). Route notes added where the target is "the same hook as Claude/Codex".
- **Plan:** OC-07 (StepBoundary like Codex/Claude; inbox only as receipt evidence), OC-09 (skills like Claude; no agent/file mentions), OC-11 (full Claude/Codex subagent contract, `stop_native_child`, background shells as native tasks), OC-12 (user-row fork, native-fork side chats, rewind and edit through Claude's hooks; no import), OC-13 (inline `OPENCODE_CONFIG_CONTENT` attachment like Claude's `--mcp-config`; root-only evidence fence; side-chat seed in the instruction channel), OC-14 (Claude discovery hooks), OC-15 (Review like Claude), OC-16 (remote-host parity only).
- **Validation:** OCE-19..23 re-scoped; OCE-24 added (side chats, Claude comparison); comparative parity proof rule (§1 item 8).
- **New findings:** native-fork side chats are offered for OpenCode and fail on v2 (G-20). Snapshot side chats send the seed as a quoted user prefix (G-21). LUNA tools must be root-only (parity). Claude's review is a command line, so OpenCode's `/review` is its counterpart.

The prior-parity reconciliation in §6–§8 still holds. Rows whose OpenCode equivalent was an OpenCode-only feature now map to "not pursued (PD-14)" instead of a package.

**T3 usage comparison (2026-10-07, user request).**
- **Plan limits:** LUNA's OpenCode Go reader already matches T3's (same endpoint, windows and key sources) and also covers OpenCode 2's Console sign-in, so PD-17 stands.
- **History:** T3 also reads OpenCode usage **history** from OpenCode's local database (`e5a46d6c5d`, 2026-09-25). LUNA marks it unsupported, with an outdated note. Added as F-MOD-15 (OC-10, OCE-13), following T3's read-only method.
