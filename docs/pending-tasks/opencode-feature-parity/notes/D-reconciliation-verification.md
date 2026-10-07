# D — Coverage reconciliation and verification planning

Owner: workstream D. Research date: **2026-10-06**. Conventions, pins, status vocabulary, evidence levels (E1–E5) and category codes follow `notes/00-baseline.md`. LUNA pin `9b3e9a2e`. Everything here is research only; "done"/"certified" labels quoted from older inventories are **historical (E4)**, not re-run.

First-pass OpenCode mapping vocabulary used in §1: `likely-applies` (generic agent capability; OpenCode v2 plausibly has it), `needs-check` (OpenCode may have it but shape/semantics uncertain), `n/a-provider-specific` (tied to another provider; reason given), `opencode-equivalent-unknown` (no known OpenCode analogue yet; workstream A/B to confirm).

## Contents

1. Master reconciliation checklist (from existing parity inventories) — §1
2. Coverage structure and evidence standards to mirror — §1.Z
3. Shared LUNA contracts → OpenCode reuse points — §2
4. Historical OpenCode evidence (E4) — §3
5. T3 and BB OpenCode patterns — §4
6. Verification planning inputs, failure cases, E2E skeletons — §5
7. Product decisions — §6
8. Unresolved questions — §7

## 1. Master reconciliation checklist

Row IDs below are **D-local reconciliation IDs** (prefix = source family), except where the source already defines an ID (`CC-00…CC-18`, `G1…G12`, `E01…E28`), which are kept verbatim. All paths are relative to the LUNA root. "Src" line numbers are at LUNA `9b3e9a2e` (docs are untracked/unchanged research drafts). Abbreviations for the OC column: **LA** `likely-applies`, **NC** `needs-check`, **NA** `n/a-provider-specific`, **UNK** `opencode-equivalent-unknown`. The OC column is a first pass for the coordinator; workstreams A/B/C own the authoritative native/protocol/LUNA findings.

Codex dir abbreviation: `cx/` = `docs/pending-tasks/codex-feature-parity/`. Claude dir: `cc/` = `docs/pending-tasks/claude-code-feature-parity/`.

### 1.A Codex parity (research 2026-09-30, Codex CLI 0.159.0, LUNA `e71d90ca`) — historical E4

The Codex inventory defines **no package IDs** (no `CX-nn`). Its units are: the capability matrix, the 4-step implementation sequence, the "concrete losses" list, the E2E scenario table, plus per-audit tables. All are listed.

#### 1.A.1 Current capability matrix — `cx/README.md:53-75`

| ID | Capability (historical LUNA status) | Src | Cat | OC | Note for OpenCode |
| --- | --- | --- | --- | --- | --- |
| CXM-01 | Native session start/resume/import/fork (Supported) | cx/README.md:55 | SES | LA | OC has sessions/fork; check v2 import of CLI-created sessions |
| CXM-02 | Native model/reasoning/service-tier choices; live model catalog → Traits picker (Supported) | :56 | MOD | NC | Models+variants likely; "service tier" is OpenAI-specific → treat as provider option |
| CXM-03 | Ordinary native steering (accepted labelled applied too early) | :57 | TURN | NC | Does OC v2 accept a prompt while busy (queue vs inject)? B owns |
| CXM-04 | Queued follow-ups via LUNA durable queue | :58 | INP | LA | Shared LUNA queue; provider-agnostic |
| CXM-05 | Read/Search/list-files typed activity | :59 | TOOL | LA | OC `read`/`grep`/`glob`/`list` tools |
| CXM-06 | Write/Edit/Patch per-file chips + diffs (large diffs truncated) | :60 | TOOL/OUT | LA | OC `write`/`edit`/`patch`; per-file identity needs check |
| CXM-07 | Command result/error/output at completion; no live stdout; capped | :61 | OUT | LA | OC bash tool streams metadata output; cap check |
| CXM-08 | Web search/open/find (raw JSON) | :62 | TOOL | NC | OC `webfetch`; `websearch`/`codesearch` gated |
| CXM-09 | Viewed/generated output images | :63 | OUT | NC | OC read-tool image attachments; generated images unknown |
| CXM-10 | Native image prompt input (was Missing) | :64 | INP | LA | OC file parts with mime/data URL |
| CXM-11 | MCP lifecycle (only text blocks survive) | :65 | EXT/OUT | LA | OC MCP tool parts; rich content check |
| CXM-12 | Generic MCP elicitation (was Missing) | :66 | PERM | UNK | No known OC MCP elicitation surface; A to confirm |
| CXM-13 | LUNA dynamic tool execution (call/result hidden) | :67 | EXT | NC | Requires an OC-callable bridge (MCP/plugin); C/B |
| CXM-14 | Native plugin/app selection (was Missing) | :68 | INP/EXT | NA | Codex `plugin://`/`app://` mention identities; OC analogue = agent `@` mention / commands (NC) |
| CXM-15 | Explicit native skill selection (Partial) | :69 | INP/EXT | NC | OC skills exist; native typed selection shape unknown |
| CXM-16 | Native Plan/Build workflow (Dropped by user) | :70 | ADV | NA | `excluded-by-product-decision`; still inventory OC `plan` agent |
| CXM-17 | Native Review workflow (was Missing) | :71 | ADV | NC | OC review command/agent existence to confirm |
| CXM-18 | Same-thread conversation revert (was Missing) | :72 | SES | LA | OC session revert/unrevert |
| CXM-19 | Goal engine (potentially inherited) | :73 | ADV | UNK | No known OC goal engine |
| CXM-20 | Rich permission decisions (Partial) | :74 | PERM | LA | OC once/always/reject + rules |
| CXM-21 | Mermaid in chat (Missing) | :75 | OUT | LA | LUNA-side renderer; provider-agnostic |

#### 1.A.2 Implementation sequence — `cx/README.md:79-84`

| ID | Step | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CXS-1 | Native composer inputs: image, plugin/app, skill selection; durable identity; send/queue/steer | :79 | INP | LA (image), NA (plugin/app), NC (skill) |
| CXS-2 | Tool activity fidelity: full results on host, live command activity, dynamic tools, rich MCP/unknown | :80 | OUT/TOOL | LA |
| CXS-3 | Steering delivery semantics: accepted vs recorded/incorporated, correlation, races | :81 | TURN | NC |
| CXS-4 | Common workflows: Review, MCP prompts, revert, Mermaid, goals (Plan/Build dropped) | :82 | ADV | NC |
| CXS-5 | Generic MCP prompts may move earlier for selected plugins | :84 | PERM | UNK |

#### 1.A.3 Preserve-behaviors and concrete losses — `cx/README.md:90-106`

| ID | Item | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CXP-1 | Single native read/search/list action → typed chip; composite stays Run | :92 | TOOL | LA (OC has typed tools; no shell classification needed) |
| CXP-2 | Don't relabel shell writes as Write; only native file change | :93 | TOOL | LA |
| CXP-3 | Multi-file patch fan-out to stable per-file IDs; add=raw content, update=unified patch | :94 | OUT | NC (OC `patch` part / edit metadata diff) |
| CXP-4 | Tool completion resolves original item even after steer split | :95 | TURN/OUT | LA |
| CXP-5 | Viewed/generated images imported into profile storage after validation | :96 | OUT | NC |
| CXP-6 | Failure state, labels, detail, diff stats, reopened history survive | :97 | OUT/SES | LA |
| CXL-1 | 64 KiB command output cap before journal; normaliser limits | :101 | OUT | LA (check OC adapter caps; C) |
| CXL-2 | outputDelta/terminalInteraction/file-change progress ignored | :102 | OUT | LA (OC running tool-part metadata updates) |
| CXL-3 | MCP projection keeps only text blocks | :103 | OUT/EXT | LA |
| CXL-4 | LUNA dynamic calls lack lifecycle events except luna_status | :104 | EXT | NC |
| CXL-5 | Unknown items/notifications disappear; need bounded fallback | :105 | LIFE/OUT | LA (OC event/part types evolve per release) |
| CXL-6 | Web results raw; optional source cards | :106 | OUT | NC |

#### 1.A.4 Steering contract and queue rules — `cx/README.md:114-146`

| ID | Item | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CXT-1 | Steer with exact expectedTurnId + optional clientUserMessageId | :114 | TURN | NC (no known OC expected-turn fence) |
| CXT-2 | Atomic validate + append pending input | :115 | TURN | NC |
| CXT-3 | RPC reply = acceptance only | :116 | TURN | NC (OC `prompt_async` 204 semantics; B) |
| CXT-4 | Default: response continues, in-flight tools drain before incorporation | :117 | TURN | NC |
| CXT-5 | Several steers keep order | :118 | TURN | NC |
| CXT-6 | Distinct accepted / recorded / rejected / unknown delivery states | :126 | TURN/INP | LA (shared queue states) |
| CXT-7 | Client message ID is correlation not idempotency; no blind replay | :130 | TURN | NC (OC client-supplied messageID semantics) |
| CXT-8 | Follow-ups default to Queue | :134 | INP | LA |
| CXT-9 | Explicit Steer requires active turn, rejects attachments, no substitute turn | :135 | TURN/INP | NC |
| CXT-10 | Ordinary warm delivery fallback / orphan-recovery duplicate risk | :136 | TURN | LA |
| CXT-11 | Send now and Stop separate from Steer | :137 | TURN | LA |
| CXT-12 | Interrupt cancels turn, clears pending input; not a background-kill promise | :138 | TURN | NC (OC abort semantics) |
| CXT-13 | instant_interrupt opt-in | :142 | TURN | NA (Codex feature flag) |
| CXT-14 | turn/completed status authoritative (completed/failed/interrupted); interrupted-without-token edge | :144 | TURN | NC (OC idle/error/aborted signalling; B) |

#### 1.A.5 Installed plugin/app/skill selection — `cx/README.md:150-189`, `cx/plugin-mentions-research.md:8-32`

| ID | Item | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CXI-1 | Plugin bundles vs connector apps are distinct catalogs | cx/README.md:154-160; plugin-mentions:8 | EXT | NA (Codex concepts) |
| CXI-2 | Installed-only, enabled, policy-allowed eligibility; no IDs from display names | cx/README.md:162 | EXT | NC (applies to OC agents/commands/skills catalogs) |
| CXI-3 | Host-routed read-only catalog per account/home/cwd/thread | :170 | EXT/WS | LA (pattern) |
| CXI-4 | Reuse composer popup; edit-safe chips with native kind/ID | :171 | INP | LA |
| CXI-5 | Persist selections in draft/queue; generate native input at dispatch (start+steer) | :172 | INP | LA |
| CXI-6 | Identities survive restart/reorder/account change/remote dispatch | :173 | INP/WS | LA |
| CXI-7 | Unavailable selection preserved with explanation | :174 | INP | LA |
| CXI-8 | Distinguish metadata error / old CLI / policy-disabled / empty | :175 | EXT | LA |
| CXI-9 | Cache scope: host, account/home, cwd, CLI version, thread; ~5-min TTL; invalidation | :179-183 | EXT | LA |
| CXI-10 | Icons host-owned, lazy, fallback | :185 | EXT/WS | NC |
| CXI-11 | Older-CLI fallbacks capability-tested | :187 | HOST | LA (v1/v2 split) |

#### 1.A.6 Other workflows — `cx/README.md:193-217`

| ID | Item | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CXW-1 | Plan/Build dropped; ordinary chat + plan files | :193-195 | ADV | NA (`excluded-by-product-decision`) |
| CXW-2 | Native Review targets (working tree/branch/commit/custom; inline/detached) | :197-199 | ADV | NC |
| CXW-3 | Generic schema-aware MCP forms + URL flow | :203 | PERM | UNK |
| CXW-4 | Permission categories, turn/session scope, network context, proposed rules | :205 | PERM | NC (OC "always" patterns) |
| CXW-5 | Conversation revert separate from file restore | :209 | SES | LA (OC revert also restores files via snapshots — semantics differ; NC) |
| CXW-6 | Goals stable/default-on, idle continuation | :211 | ADV | UNK |
| CXW-7 | Mermaid chat rendering | :213 | OUT | LA |
| CXW-8 | Background terminals, live turn settings (experimental) | :215 | TOOL/MOD | NC |
| CXW-9 | Memory inherited when configured | :215 | EXT | NC |
| CXW-10 | Attach Chrome / Record skill / Sketch are product affordances | :217 | CLIENT | NA |

#### 1.A.7 Codex E2E acceptance scenarios — `cx/README.md:235-256`

| ID | Scenario | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CXE-01 | Read/search/list and shell-created file truthful | :237 | TOOL | LA |
| CXE-02 | Add/edit/delete patch: paths, diff, stable IDs, failure | :238 | OUT | LA |
| CXE-03 | Large command/diff result >64 KiB head/tail survives | :239 | OUT | LA |
| CXE-04 | Live command and stdin progress | :240 | OUT | LA (stdin NC) |
| CXE-05 | Web search/open/find labels + sources | :241 | TOOL | NC |
| CXE-06 | Native image input and output | :242 | INP/OUT | LA |
| CXE-07 | MCP mixed result + unknown fallback | :243 | OUT/EXT | LA |
| CXE-08 | LUNA dynamic tools lifecycle + receipt | :244 | EXT | NC |
| CXE-09 | Held sequential/parallel tools + two steers | :245 | TURN | NC |
| CXE-10 | ACK then terminal failure or Stop | :246 | TURN | NC |
| CXE-11 | Stale turn / wrong reply ID / lost reply | :247 | TURN | NC |
| CXE-12 | Ordinary delivery recovery / duplicate policy | :248 | TURN | LA |
| CXE-13 | Explicit or external interruption status | :249 | TURN | LA |
| CXE-14 | Plugin/app/skill selection reaches start+steer | :250 | INP/EXT | NA (plugin/app), NC (skill/agent) |
| CXE-15 | Draft/queue/restart/remote account scope of selections | :251 | INP/WS | LA |
| CXE-16 | Catalog refresh and same-name collisions | :252 | EXT | LA |
| CXE-17 | Native Plan and Review survive reopen | :253 | ADV | Plan NA (excluded); Review NC |
| CXE-18 | Goal continuation | :254 | ADV | UNK |

Evidence rule (`cx/README.md:233`): retain CLI version, feature state, source SHA, fixture inputs, native payloads or redacted readback, `WatchDocMessages`/`WatchQueue`/receipts, host journal/blob readback, screenshots, hashes, reproduction commands. "A screenshot alone cannot prove delivery or retained bytes."

#### 1.A.8 Supporting audits (capability rows)

Native Codex source audit — `cx/codex-source-audit.md:10-23` (capability table) and `:45-51` (prioritized candidates):

| ID | Item | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CXA-01 | Thread lifecycle incl. archive/delete/unarchive/compact/revert/list/read, paged history | :12 | SES | LA |
| CXA-02 | Steer/interrupt; experimental durable editable queue | :13 | TURN/INP | NC |
| CXA-03 | Review and rewind (revert keeps files) | :14 | ADV/SES | NC |
| CXA-04 | Model list capabilities, live turn settings | :15 | MOD | LA (models), NC (live change) |
| CXA-05 | Plan and progress (plan/diff updates; TUI /plan /review /diff /compact) | :16 | OUT/ADV | NC (OC todo + session diff) |
| CXA-06 | Permission/approval callback families, scopes, amendments | :17 | PERM | LA |
| CXA-07 | Prompt media variants (text elements, image inline/fileId/local, audio, skill, mention) and durable thread attachments | :18 | INP | NC |
| CXA-08 | Item union incl. hook prompt, reasoning, collab, web, image gen, review markers, compaction | :19 | OUT | LA (map to OC part types) |
| CXA-09 | Skills, hooks, plugins, apps, MCP OAuth/status/resources | :20 | EXT | LA (MCP/skills/plugins), NA (apps) |
| CXA-10 | Goals and memory | :21 | ADV/EXT | UNK |
| CXA-11 | Background processes, realtime voice, remote control | :22 | TOOL/HOST | UNK |
| CXA-12 | Config/account/search; layered effective config | :23 | MOD/WS | LA (OC config layers) |
| CXA-P1…P7 | Priorities: review; rewind; lossless rich items; settings/discovery; approval fidelity; goals+queue; realtime/live settings | :45-51 | mixed | as above |

LUNA Codex coverage audit — `cx/luna-coverage-audit.md:18-37`:

| ID | Gap | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CXG-1 | P1 native image input omitted | :18 | INP | LA |
| CXG-2 | P1/P2 generic MCP elicitation declined | :20 | PERM | UNK |
| CXG-3 | P1/P2 native review unavailable | :22 | ADV | NC |
| CXG-4 | P2 goals lack controls/readback | :24 | ADV | UNK |
| CXG-5 | Plan/Build dropped | :26 | ADV | NA (excluded) |
| CXG-6 | P2 dynamic-tool execution invisible | :28 | EXT | NC |
| CXG-7 | Richer approvals simplified | :32 | PERM | LA |
| CXG-8 | Native history revert absent (fork/checkpoint alternatives) | :33 | SES | LA |
| CXG-9 | Integrations inherited but hard to manage/verify | :34 | EXT | LA |
| CXG-10 | New item types silently vanish | :35 | LIFE | LA |
| CXG-11 | Mermaid in file preview, not chat | :36 | OUT | LA |
| CXG-12 | Experimental controls (editable queue, live settings, memory, bg terminals, voice) not core | :37 | mixed | NC |

LUNA tools/steering audit — `cx/luna-tools-steering-audit.md:14-21` (tool table), `:39-41` (E2E):

| ID | Item | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CXTS-1 | Read/Search path exploration typed chips | :14 | TOOL | LA |
| CXTS-2 | Write/Edit/apply_patch typed + diffs | :15 | TOOL | LA |
| CXTS-3 | Run final result/error/lazy output | :16 | OUT | LA |
| CXTS-4 | Web search/open/find raw | :17 | TOOL | NC |
| CXTS-5 | View image / image generation end-to-end | :18 | OUT | NC |
| CXTS-6 | MCP calls mixed content lossy | :19 | OUT | LA |
| CXTS-7 | LUNA cross-task dynamic tools: execution yes, lifecycle missing | :20 | EXT | NC |
| CXTS-8 | Subagents: parent chip + child transcript with early-traffic buffering | :21 | AGT | LA (OC task tool → child session) |
| CXTS-E1 | Native default tool-boundary steer | :39 | TURN | NC |
| CXTS-E2 | ACK vs incorporation/failure/Stop (deterministic fixture) | :40 | TURN | NC |
| CXTS-E3 | Tool representation/data preservation sweep | :41 | OUT | LA |

Native steering acceptance — `cx/native-steering-audit.md:28-33`: CXNS-1 held command + steer (:28); CXNS-2 two parallel tools + two steers (:29); CXNS-3 steer during streaming, instant_interrupt off (:30); CXNS-4 steer after end / stale ID / non-steerable turn (:31); CXNS-5 nonfatal tool error then steer; terminal failure (:32); CXNS-6 interrupt during held tool loses pending steer (:33). All TURN; OC = NC (depends on whether OC v2 has mid-turn injection at all — B).

Steering-status validation — `cx/steering-status-validation.md:7-12` (contract), `:32-41` (10 failure triggers), `:47-52` (6 real-provider steps):

| ID | Trigger/step | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CXSS-F1 | Ack while active tools run → pending, no early bubble | :32 | TURN | NC |
| CXSS-F2 | Same text, wrong/missing client ID, foreign thread, stale turn → no settlement | :33 | TURN | LA (root/child + session attribution) |
| CXSS-F3 | Two accepted inputs, duplicate started/completed records → once, in order | :34 | TURN/LIFE | LA (duplicate SSE) |
| CXSS-F4 | Lost ack then native record → settle, no resend | :35 | TURN | LA |
| CXSS-F5 | Lost notification, terminal history read settles | :36 | TURN/LIFE | LA (SSE gap + message list readback) |
| CXSS-F6 | No record before completion/failure/Stop/EOF → durable unknown | :37 | TURN | LA |
| CXSS-F7 | Native external interruption reported | :38 | TURN | LA |
| CXSS-F8 | Resume history has one old ID, omits another → only recorded settles | :39 | SES/TURN | LA |
| CXSS-F9 | Evidence races unknown/ack projection | :40 | TURN | LA |
| CXSS-F10 | Timeout/failed read bounded; waiters cleaned | :41 | LIFE | LA |
| CXSS-R1…R6 | Real: held 20 s command + steer; two steers/two tools; Stop/failure/external interrupt; controlled transport faults; restart/resume unresolved delivery; Queue/Steer/Send now/Stop independence | :47-52 | TURN | NC/LA |

Goal controls validation — `cx/goal-controls-validation.md:59-66` (8 native UI scenarios): CXGV-1 editor/budget validation; -2 AX roles/ids/keyboard; -3 pause/edit/resume/complete/clear; -4 navigate-away + unknown receipt; -5 inherited active goal on adopted thread; -6 queued marker during continuation; -7 stop runtime mid-continuation + recovery; -8 clear context/switch provider/two accounts. Cat ADV; OC = UNK (no known goal engine) except -2 (AX conventions, LA) and -8 (account isolation, LA).

BB Codex matrix — `cx/bb-integration-audit.md:18-30` (13 rows): start/resume/fork/idle restore; edit/rewind (BB-owned fork); turns/steer/queue/stop; model/reasoning/fast; permissions; structured questions (off by default); plan/goal textual; compaction/context/usage; skills/plugins/apps/MCP; subagents; attachments/images/search; auth/usage/version; not exposed (review/revert/live settings/voice/queue/attachments). IDs CXBB-01…13 in row order. These are reference rows, not LUNA requirements; mapped in §4.

T3 Codex matrix — `cx/t3-integration-audit.md:21-37` (17 rows): thread start/resume/read; CLI import (partial); fork (missing); revert/rewind; follow-up while running; approvals; user questions; MCP elicitation (partial); dynamic tools (passive); plan (off by default)/review (missing); model/reasoning/tier; skills/plugins/apps/MCP config; subagents; attachments/search; compaction/context/traces; auth/usage/feedback; realtime/goals/live settings (not exposed). IDs CXT3-01…17. Reference only; see §4.

Competitor tools/plugins — `cx/competitor-tools-plugins-audit.md:8-35`: CXC-1 BB typed timeline shapes (:10); CXC-2 BB web normalization (:11); CXC-3 BB MCP content omitted (:12); CXC-4 BB live deltas (:13); CXC-5 BB 32k inline cap + on-demand full output with expired state (:14); CXC-6 BB unknown → `unhandled` raw (:15); CXC-7…11 T3 item mapping, output-delta drop, work-log grouping, computer-use MCP titles, web rows (:19-23); CXC-12 T3 skills/list per cwd (:27); CXC-13 BB skill-root indexing + content hash (:29-30); CXC-14 BB explicit steer with expected-turn protection (:34); CXC-15 T3 sends turn/start while running (:35). Patterns CXC-5/6/13 are provider-agnostic (LA as patterns).

**1.A subtotal:** 21 matrix + 5 sequence + 12 preserve/loss + 14 steering + 11 selection + 10 workflow + 18 E2E + 19 source-audit (12 capability rows + 7 priorities) + 12 LUNA gaps + 11 tools/steering + 6 native-steering + 16 steering-status + 8 goal-UI + 13 BB + 17 T3 + 15 competitor = **208 rows**.

### 1.B Claude Code parity (research 2026-09-30, reassessed 2026-10-01, Claude CLI 2.1.286, LUNA `d0f12bcf`) — historical E4

No inventory file in either parity directory, `docs/PARITY.md` or the certification ledger mentions OpenCode (`rg -i opencode` → 0 hits, E1). Every mapping below is therefore new.

#### 1.B.1 Work packages CC-00…CC-18 — `cc/implementation-plan.md:36-54` (table), `:58-206` (bodies + gates), refreshed ledger `cc/current-gap-assessment.md:126-146`

| ID | Package (one line) | Gate scenarios | Src (plan / ledger) | Cat | OC | OpenCode note |
| --- | --- | --- | --- | --- | --- | --- |
| CC-00 | Baseline + executable conformance; capability ledger; sanitized init capture | E01/E02 | plan:36,58-64 / gap:128 | HOST/LIFE | LA | Exact binary (v1 vs v2), `/api/health` vs `/global/health`, version, cwd |
| CC-01 | Transport decision (CLI driver vs SDK sidecar) + versioned control interface | E01/E03 | plan:37,66-72 / gap:129 | LIFE | LA | OC: HTTP+SSE vs `@opencode-ai/sdk` vs ACP (`opencode acp`) — B owns |
| CC-02 | Lossless events, content, storage; unknown-frame retention | E04/E05 | plan:38,74-80 / gap:130 | OUT/LIFE | LA | `normalize_v2_frame` losses (B/C) |
| CC-03 | Permissions, questions, dialogs, cancellation, effective mode | E06/E07 | plan:39,82-88 / gap:131 | PERM | LA | OC permission asked/replied; question tool; reject/abort |
| CC-04 | All-turn typed image/attachment input | E08 | plan:40,90-96 / gap:132 | INP | LA | OC file parts on every prompt path |
| CC-05 | Complete tool results + structured output | E04/E05/E09 | plan:41,98-104 / gap:133 | OUT/TOOL | LA | OC tool part state (input/output/metadata/attachments); structured output NC |
| CC-06 | Effective models/settings/usage + truthful delivery | E02/E10/E11 | plan:42,106-114 / gap:134 | MOD/TURN | LA | OC providers/models/variants, cost/tokens on assistant messages |
| CC-07 | Callable LUNA/Executor bridge (app tools) | E12 | plan:43,116-122 / gap:135 | EXT | NC | OC can attach MCP servers via config / runtime `mcp.add`? — B/C |
| CC-08 | Native agents + background task graph | E13 | plan:44,124-130 / gap:136 | AGT | LA | OC `task` tool → child session (`parentID`) |
| CC-09 | Persistent LUNA children + native peer messages | E14 | plan:45,132-138 / gap:137 | AGT | LA (children) / NA (peer messaging) | LUNA child chats ≠ OC subagents |
| CC-10 | Native history (list/read/fork/truncate) + file checkpoints | E15 | plan:46,140-146 / gap:138 | SES | LA | OC fork, revert/unrevert (with snapshot file restore) |
| CC-11 | MCP inventory/resources/apps | E16 | plan:47,148-154 / gap:139 | EXT | LA (inventory/status), NC (resources), UNK (apps/widgets) | |
| CC-12 | Skills/plugins/hooks/memory/styles | E17 | plan:48,156-162 / gap:140 | EXT | LA | OC skills, plugins (JS hooks), AGENTS.md instructions, commands, agents |
| CC-13 | Workflows, teams, channels (+ external sessions) | E18 | plan:49,164-170 / gap:141 | AGT/ADV | NA | Claude-specific lanes; check OC equivalents only for external-session listing |
| CC-14 | Native goals + scheduling (cron/monitor/wakeup) | E19 | plan:50,172-178 / gap:142 | ADV | UNK | |
| CC-15 | Review + structured findings | E20 | plan:51,180-184 / gap:143 | ADV | NC | OC review command (A) |
| CC-16 | Cloud/remote/browser/computer integrations | E21 | plan:52,186-192 / gap:144 | HOST | NC | OC share links, `opencode web`/`serve`, GitHub agent, enterprise — A |
| CC-17 | Operational compatibility + account variants | E22 + E01–E21 | plan:53,194-200 / gap:145 | HOST/LIFE | LA | v1/v2 coexistence, auth providers, OS matrix |
| CC-18 | Full certification + delivery | all | plan:54,202-206 / gap:146 | (meta) | LA | Mirror for OpenCode certification |

Staging (`cc/README.md:53-59`): A = CC-00/01; B = CC-02…06; C = CC-07…10; D = CC-11…15; E = CC-16…18. "Definition of full parity" `cc/implementation-plan.md:208-212` — every row has accurate native availability + verified LUNA outcome or explicit blocker/exclusion; core / advanced native / hosted reported separately.

Proposed conceptual contracts (`cc/implementation-plan.md:20-27`), provider-neutral and directly reusable for OpenCode: capability snapshot; native event envelope; content reference; input delivery receipt (written/accepted/recorded/incorporated/unknown); pending interaction; native task binding. Cat LIFE/OUT/TURN/PERM/AGT; OC = LA.

#### 1.B.2 Gap groups G1…G12 — `cc/current-gap-assessment.md:62-120`

| ID | Gap | Src | Cat | OC | OpenCode note |
| --- | --- | --- | --- | --- | --- |
| G1 | P0 Ask/Auto config differs from discovery (`--restricted`; discovery via separate unrestricted init cached in OnceCell) | :66-72 | MOD/PERM | NC | Same class: discovery server vs run server must share binary/home/cwd/config; OC agent/permission config |
| G2 | P0 result bodies dropped before journal | :74-76 | OUT | LA | Check OC tool `state.output`/metadata survive (C) |
| G3 | P0 root completion doesn't distinguish another native turn's result (UUID/origin) | :78-80 | TURN | LA | OC: correlate message IDs / parentID; background child idle must not settle root |
| G4 | P0 no callable LUNA tool transport | :82-84 | EXT | NC | |
| G5 | P0 rich approvals/dialogs/cancellation not wired; unknown control requests abandoned | :86-88 | PERM | LA | |
| G6 | P0 image/native-selection input holes (cold skip, warm text-only, steer rejects) | :90-92 | INP | LA | |
| G7 | P1 native agent/task/hook visibility narrower (forwarding opt-ins, nested ownership race) | :96-100 | AGT/EXT | LA | OC child sessions emit their own events; nested task ownership |
| G8 | P1 native history/reset/context lifecycle missing | :102-104 | SES/MOD | LA | OC fork/revert/compaction (`summarize`) |
| G9 | P1 goals/scheduling/findings disconnected (Codex-only routes) | :106-108 | ADV | NC | |
| G10 | P1 mixed results/resources/widgets exceed `Option<String>` output | :110-112 | OUT | LA | OC tool `attachments` (file parts) |
| G11 | P1 effective settings/extensions/live accounting partial | :114-116 | MOD/EXT | LA | OC config layers; cost per message |
| G12 | Advanced/hosted features own coverage; external native background sessions | :118-120 | HOST/AGT | NC | OC sessions created outside LUNA on the same server/storage |

Residual shared issues (`cc/current-gap-assessment.md:173-178`): RS-1 approval choices use radio role while acting immediately (:175, PERM/CLIENT, LA — shared wizard); RS-2 fresh dispatch writes user row before native resume reconciliation → delivered-looking rejected row (:176, TURN, LA); RS-3 failed child-creation chip generic (:177, AGT, LA); RS-4 prior suite failures (task_identity×3, turn_quiesce, UI SIGABRT) (:178, meta).

Corrections (`:52-60`): COR-1 follow-up images take fresh resumed process (INP); COR-2 Claude already has integration/usage infra (EXT/MOD); COR-3 manual app children vs native subagents (AGT); COR-4 shared delivery ≠ certification (meta); COR-5 permission-flag spelling (PERM, NA).

#### 1.B.3 Capability inventory — `cc/capability-inventory.md`

Local session and I/O (`:15-29`):

| ID | Capability | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CCI-01 | Native executable/version/account selection, same for discovery/launch/resume/maintenance | :15 | HOST | LA |
| CCI-02 | Native prompt/runtime preset + appended app instructions | :16 | EXT | LA (OC agent prompt + `system`/instructions) |
| CCI-03 | Multi-turn streaming, deterministic per-turn completion | :17 | LIFE/TURN | LA |
| CCI-04 | Text, images, local files typed across all routes | :18 | INP | LA |
| CCI-05 | Image-only and mixed input; ordered blocks | :19 | INP | LA |
| CCI-06 | Document/PDF content; no invented audio/video | :20 | INP/OUT | NC (OC PDF file parts per model modality) |
| CCI-07 | Streaming tool arguments; one execution | :21 | OUT | NC (OC tool part `pending` with raw input) |
| CCI-08 | Tool results + generated assets, host lazy retrieval | :22 | OUT | LA |
| CCI-09 | Reasoning summaries; redacted blocks | :23 | OUT | LA (OC reasoning parts) |
| CCI-10 | Structured output separate from text | :24 | OUT | NC |
| CCI-11 | Model/effort/thinking/fast/context options; effective state | :25 | MOD | LA (variants) |
| CCI-12 | Usage/context/cache/limits/background cost | :26 | MOD | LA |
| CCI-13 | Session naming/import/export | :27 | SES | LA (OC title, export/import CLI) |
| CCI-14 | Native history fork/truncation at exact boundary | :28 | SES | LA |
| CCI-15 | File checkpoint rewind separate from transcript and Git | :29 | SES/WS | NC (OC revert couples messages + snapshot files) |

Permissions, tools, extensions (`:37-50`):

| ID | Capability | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CCI-16 | Built-in tool coverage + unknown tool retention | :37 | TOOL | LA |
| CCI-17 | Tool policies/approvals with context + one terminal response | :38 | PERM | LA |
| CCI-18 | Native automatic approval truthfully labelled | :39 | PERM | LA (OC `allow` rules / `--dangerously-skip-permissions`?) NC |
| CCI-19 | Questions/dialogs: options, multi-select, free text, form/URL, cancel, timeout | :40 | PERM | NC (OC question tool) |
| CCI-20 | Engine-owned app tools via real session bridge | :41 | EXT | NC |
| CCI-21 | External MCP via Executor; no duplicate registrations | :42 | EXT | LA |
| CCI-22 | MCP search/resources | :43 | EXT | NC |
| CCI-23 | MCP Apps widgets | :44 | EXT | UNK |
| CCI-24 | Native memory/instructions roots + precedence | :45 | EXT | LA (AGENTS.md, CLAUDE.md compat, `instructions`) |
| CCI-25 | Skills and commands; namespaced; argument hints; live refresh | :46 | EXT | LA |
| CCI-26 | Hooks | :47 | EXT | NC (OC plugin hooks) |
| CCI-27 | Plugins/marketplaces/LSP | :48 | EXT | LA (plugins, LSP) |
| CCI-28 | Output styles | :49 | EXT | NA (Claude feature) |
| CCI-29 | Dynamic settings session vs persistent | :50 | MOD | NC |

Agents and ongoing work (`:58-70`):

| ID | Capability | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| CCI-30 | Native subagents (parent tool ID, IDs, model, tools, transcript) | :58 | AGT | LA |
| CCI-31 | Custom subagent definitions | :59 | AGT/EXT | LA (OC agents `mode: subagent`) |
| CCI-32 | Background shell/task lifecycle | :60 | TOOL/AGT | NC |
| CCI-33 | Dynamic workflows | :61 | AGT | NA |
| CCI-34 | Agent teams | :62 | AGT | NA |
| CCI-35 | Native cross-session messaging | :63 | AGT | NA |
| CCI-36 | Persistent LUNA child chats | :64 | AGT | LA (shared LUNA) |
| CCI-37 | Native goals | :65 | ADV | UNK |
| CCI-38 | Native schedules/monitors | :66 | ADV | UNK |
| CCI-39 | Channels | :67 | ADV | NA |
| CCI-40 | Review and findings | :68 | ADV | NC |
| CCI-41 | Native workspace transitions (cwd/worktree) | :69 | WS | NC (OC worktree/workspace experimental) |
| CCI-42 | End/reset conversation | :70 | SES | NC |

Hosted/other surfaces (`:80-91`): CCI-43 Chrome (:80, NA); CCI-44 computer use (:81, NA); CCI-45 Remote Control/mobile (:82, NC — OC has its own remote/web client; distinguish from LUNA remote); CCI-46 cloud sessions/teleport (:83, NA); CCI-47 Cloud Projects (:84, NA); CCI-48 hosted artifacts (:85, NA — OC share links are the nearest analogue, NC); CCI-49 routines/cloud schedules (:86, NA); CCI-50 ultrareview (:87, NA); CCI-51 enterprise providers (:88, LA — OC multi-provider/models.dev/Bedrock etc.); CCI-52 organization policy (:89, NC — OC enterprise/managed config); CCI-53 CLI health/update/auth (:90, LA — `opencode upgrade`, `auth`); CCI-54 CI/editor/desktop (:91, NC — OC GitHub action, IDE, desktop app are CLIENT).

Product exceptions (`:97-101`): CCI-X1 Plan/Build excluded; suppress mode-entry tools where policy allows; handle imported/unexpected mode requests (ADV; OC: `plan` agent and any plan-enter/exit tools → inventory + safe handling, UI `excluded-by-product-decision`). CCI-X2 provider request names stay in adapter (LIFE, LA). CCI-X3 all LUNA-wide features need provider scenarios (composer, queue, cross-chat tools, output, approvals, child lineage, recovery, inventory, changes/terminals, diagrams, review/history) (LA).

#### 1.B.4 Tool, content, event and control matrix — `cc/tool-and-event-matrix.md`

Built-in tools (`:11-36`, 26 rows), CCT-01…26:

| ID | Native family | Src | Cat | OC | OpenCode analogue (first pass) |
| --- | --- | --- | --- | --- | --- |
| CCT-01 | Bash, PowerShell | :11 | TOOL | LA | `bash` (PowerShell NA) |
| CCT-02 | Read, Write, Edit | :12 | TOOL | LA | `read`, `write`, `edit` |
| CCT-03 | MultiEdit, legacy shell-output | :13 | TOOL | NC | `multiedit` (v1); v2 status unknown |
| CCT-04 | Glob, Grep | :14 | TOOL | LA | `glob`, `grep`, `list` |
| CCT-05 | NotebookEdit | :15 | TOOL | UNK | |
| CCT-06 | WebFetch, WebSearch | :16 | TOOL | NC | `webfetch`; `websearch`/`codesearch` gated |
| CCT-07 | LSP | :17 | TOOL | NC | OC LSP tool (experimental) / diagnostics |
| CCT-08 | Agent, legacy Task | :18 | AGT | LA | `task` |
| CCT-09 | ListAgents, SendMessage, SubagentHandback | :19 | AGT | NA | |
| CCT-10 | Workflow | :20 | AGT | NA | |
| CCT-11 | TaskCreate/Update/Get/List, TodoWrite | :21 | TOOL | LA | `todowrite`/`todoread` |
| CCT-12 | TaskStop, TaskOutput | :22 | AGT | NA | |
| CCT-13 | Monitor | :23 | ADV | NA | |
| CCT-14 | Cron*/ScheduleWakeup | :24 | ADV | NA | |
| CCT-15 | AskUserQuestion | :25 | PERM | NC | `question` tool |
| CCT-16 | Enter/ExitPlanMode | :26 | ADV | NC | plan-enter/exit tools if present → excluded UI, safe handling |
| CCT-17 | Enter/ExitWorktree | :27 | WS | NC | OC worktree features |
| CCT-18 | Skill | :28 | EXT | LA | `skill` |
| CCT-19 | ToolSearch, WaitForMcpServers, RefreshMcpTools | :29 | EXT | NA | |
| CCT-20 | MCP resource tools | :30 | EXT | NC | |
| CCT-21 | `mcp__<server>__<tool>` | :31 | EXT | LA | OC MCP tool naming `<server>_<tool>` |
| CCT-22 | ReportFindings | :32 | ADV | NA | |
| CCT-23 | Artifact, SendUserFile | :33 | OUT | NA | |
| CCT-24 | Projects, RemoteTrigger, PushNotification | :34 | HOST | NA | |
| CCT-25 | EndConversation, SendFeedback, onboarding | :35 | HOST | NA | |
| CCT-26 | Any future/custom native tool | :36 | TOOL | LA | OC custom tools (`.opencode/tool/*.ts`), plugin tools |

Result envelope (`:42-46`): CCT-ENV preserve IDs/ownership/ordered content/structured/metadata/is_error; normalize once; empty ≠ missing (OUT, LA).

Message families (`:54-72`, 19 rows), CCM-01…19: assistant/user/replay (:54, LA); partial assistant (:55, LA — part deltas); result (:56, LA — idle/assistant completion); system init (:57, LA — config/agents/providers snapshot); compact boundary/memory recall (:58, LA — compaction part / `session.compacted`); status/session state (:59, LA — `session.status` busy/idle/retry); task started/progress/notification (:60, LA — child session events); tool progress/summary (:61, LA); hook events (:62, NC); auth/rate limit/API retry (:63, LA — retry status, provider errors); thinking tokens (:64, LA — reasoning tokens); plugin install/commands changed (:65, NC); local command output (:66, NC — OC `/command` and shell `!` parts); permission denied/elicitation complete (:67, LA — permission replied); files persisted/mirror error (:68, NC — OC snapshot/file events); worker shutting down (:69, LA — server dispose); prompt suggestion (:70, UNK); conversation reset (:71, NC); unknown future frame (:72, LA). All cat LIFE/OUT unless noted.

Control/admin surface (`:80-86`), CCC-1…7: input/lifecycle (streamInput/interrupt/stopTask/close/init) (:80, LIFE, LA — prompt/abort/dispose); runtime selection (setModel/permission mode/thinking) (:81, MOD, LA — per-prompt model/agent/variant); discovery (commands/models/agents/account/context) (:82, MOD/EXT, LA); extensions (reload/MCP status/reconnect/toggle) (:83, EXT, NC); history (list/read/info/name/tag/fork/rewind) (:84, SES, LA); persistent settings writes (:85, MOD, NC — OC config PATCH); interactive requests + cancellation (:86, PERM, LA). Rule `:88`: one terminal response across adapter→engine→pending→RPC→UI→provider; unknown inbound request gets explicit rejection.

Hooks (`:96-100`), CCH-1…5 families (tool/permission; prompt/session/turn; agents/tasks; context/config; requests/workspace). Cat EXT; OC = NC (OC plugin hook names differ: e.g. tool before/after, permission ask, chat message/params, event bus).

Coverage artifact rule (`:106`): per observed native tool/event/control store `nativeName`, version prerequisites, sample hash, normalization outcome, owner IDs, persistence/RPC/UI outcome, E2E evidence; mark exclusions. **Mirror for OpenCode** (part type / event type / tool name).

#### 1.B.5 Claude LUNA coverage audit — `cc/luna-coverage-audit.md:17-29` and `CC-G01…G08` (`:33-77`)

CCL-01 native launch/persistence (:17, LIFE, LA); CCL-02 models/commands discovery + caching (:18, MOD, LA); CCL-03 text/reasoning (:19, OUT, LA); CCL-04 common tool calls typed (:20, TOOL, LA); CCL-05 tool results ID+error only (:21, OUT, LA); CCL-06 images first-turn only (:22, INP, LA); CCL-07 native subagents routing (:23, AGT, LA); CCL-08 approvals/questions (:24, PERM, LA); CCL-09 app tools instructions-only (:25, EXT, NC); CCL-10 history resume only (:26, SES, LA); CCL-11 integrations inherited (:27, EXT, LA); CCL-12 usage/limits (:28, MOD, LA); CCL-13 new provider events narrow (:29, LIFE, LA). Gap headings: CC-G01 result loss before storage (:33); CC-G02 image delivery differs by turn type (:41); CC-G03 instructions don't attach tools (:47); CC-G04 unknown controls leave provider waiting (:53); CC-G05 child lifecycle narrower (:61); CC-G06 steering locally accepted not proven incorporated (:67); CC-G07 native history provider-specific (:73); CC-G08 discovery without readback (:77). All map to the same OpenCode questions as G2/G6/G4/G5/G7/G3/G8/G11.

#### 1.B.6 Reference integrations (Claude) — `cc/reference-integrations.md`

T3 sections: session construction + app tools (:18), image content + skill invocation (:26), tool/stream normalization (:34), interactive requests + mutable controls (:42), native history (:48). BB sections: resident bridge (:58), typed tools/output/tasks (:66), permission/question contract (:74), history/input limitations (:82). "Do not infer" list (:92-96): SDK presence ≠ coverage; native subagents ≠ persistent app chats; forks ≠ file rewinds and path text ≠ image bytes; executes-inside ≠ represented; T3/BB plan controls and permissive sandbox fallbacks are not LUNA requirements. IDs CCR-T1…T5, CCR-B1…B4, CCR-N1…N5; all reference-only, the "do not infer" rules apply verbatim to OpenCode (LA).

#### 1.B.7 Validation plan — failure cases, scenarios, artifacts — `cc/validation-plan.md`

Failure cases recorded before implementation (`:9-23`), CCF-01…15: (1) discovery vs execution binary/account/cwd mismatch; (2) result/error/child attribution lost or tool re-executed to repair display; (3) partial+complete duplicates, ID reuse across generations, unknown events vanish; (4) image works initially then text on warm/steer/resume, image-only fails, invalid silently removed; (5) approval answered twice, cancel races late allow, recovery answers wrong request, unknown control hangs; (6) Ask/Auto labels ≠ enforcement, no sandbox shown as protection, org deny bypassed; (7) ack labelled incorporation, duplicate text mismatched, ambiguous retries repeat mutation; (8) app bridge advertises tools without handlers/leaks authority/stale tokens; (9) root completion hides child/background work, output routed to wrong agent, notification resurrects settled generation; (10) peer text treated as human, child creation duplicates or shares parent session; (11) fork changes source, rewind restores untracked files without scope evidence, crash splits provider/UI history; (12) inventory installed≠attached, stale cwd/account, expansion drops prose/images, reload duplicates MCP; (13) goal/schedule false Idle, duplicate timers, stopped task still active; (14) hosted unavailable called supported, out-of-scope browser/OS/publish; (15) observer polling changes scheduling, delay labelled failure, artifacts leak credentials, cleanup deletes active data. **All 15 apply to OpenCode** (LA) except 10-peer (NA) and 13 (UNK); reused in §5.3.

Test layers (`:27-32`): captured-native replay through the real harness process boundary; real bounded native sessions; native UI E2E; hosted/platform verification; persistent subscriptions, never poll `WatchDocMessages` (observer side effects).

Scenario matrix E01–E22 (`:38-59`):

| ID | Scenario (one line) | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| E01 | Exact executable/version/hash + init; old version → explicit capability state | :38 | HOST | LA (v1/v2 split is the key case) |
| E02 | Account/cwd-sensitive model/command discovery; stale cache; unavailable option rejection | :39 | MOD | LA |
| E03 | Start → warm → resume → interrupt → close; unknown control, EOF, child exit | :40 | LIFE | LA |
| E04 | Root/child tool success/error/empty/large output above preview limit | :41 | OUT | LA |
| E05 | Mixed MCP result, partial tool JSON, unknown content/event | :42 | OUT | LA |
| E06 | Rich permission/question/form/URL, concurrent, one-shot | :43 | PERM | LA (form/URL UNK) |
| E07 | Managed deny, workspace/network enforcement, sandbox failure | :44 | PERM | NC (OC has no OS sandbox — A) |
| E08 | Same image fixture initial/image-only/warm/steer/resumed; invalid images | :45 | INP | LA |
| E09 | Notebook/LSP/web/task/findings tools + structured output | :46 | TOOL/OUT | NC |
| E10 | Live model/effort/settings changes + usage/cache/context/limit/retry | :47 | MOD | LA |
| E11 | Identical text twice; interrupt before/after write; ambiguous reconnect | :48 | TURN | LA |
| E12 | App bridge discovery/call/error/cancel; stale token/cross-project rejected | :49 | EXT | NC |
| E13 | Nested agents, background shell, root finishes early, late notification, reused IDs | :50 | AGT | LA |
| E14 | Persistent child creation/retry/restart + native peer arrival | :51 | AGT | LA (peer NA) |
| E15 | History list/read/fork at real ID; source unchanged; file rewind preview | :52 | SES | LA |
| E16 | Executor connection, auth/disconnect, MCP resources/reload, widgets | :53 | EXT | LA (widgets UNK) |
| E17 | Native scopes/collisions, skill+image, plugin reload, hook deny, LSP failure | :54 | EXT | LA |
| E18 | Workflow/team/channel | :55 | AGT | NA |
| E19 | Goal/cron/monitor | :56 | ADV | UNK |
| E20 | Review pinned revision → findings → reopen | :57 | ADV | NC |
| E21 | Per hosted/device integration incl. stop/revoke | :58 | HOST | NC (OC share) |
| E22 | Restart/crash/auth expiry/network/proxy/limits/idle exit + platform matrix | :59 | LIFE/HOST | LA |

Additional scenarios E23–E28 (`cc/current-gap-assessment.md:196-203`):

| ID | Scenario | Src | Cat | OC |
| --- | --- | --- | --- | --- |
| E23 | Effective configuration: init and launch under same binary/home/cwd/policy agree | :198 | MOD/PERM | LA (discovery server ≡ run server) |
| E24 | Autonomous-result correlation: foreign result must not complete human receipt | :199 | TURN | LA |
| E25 | Foreground/nested native ownership | :200 | AGT | LA |
| E26 | Input/discovery: all image turn types, invalid files, typed skill, catalog changes | :201 | INP/EXT | LA |
| E27 | Provider-aware shared controls: manual child, goals/review/compact/history via verified native ops or truthful unavailable | :202 | AGT/ADV | LA |
| E28 | Rich results: text+structured+image+resource; root/child ownership; restart | :203 | OUT | LA |

Artifact contract (`cc/validation-plan.md:65-77`) — see §1.Z.

### 1.C Claude Code advanced certification ledger — `docs/reference/claude-code-advanced-certification.md` (pinned CLI 2.1.286)

Status vocabulary (`:9-16`): Certified · Supported · live QA pending · Not enabled by LUNA · Unsupported in LUNA · Externally gated · Native Claude Code only. Paired UI ledger `crates/ui/src/claude_advanced.rs` `CLAUDE_ADVANCED_LEDGER` (`:5`). Rule `:18`: no row certified by a model turn.

CC-13 lanes (`:24-36`), CERT13-01…13: dynamic workflow runs (:24); `workflow_progress` field (:25); terminal/lifecycle rules (sticky terminal, process-end settles Stopped) (:26); workflow availability gate (:27); stop a workflow (:28); pause/resume/restart (:29); workflow approvals after launching turn — parked gate auto-declines (:30); agent teams (:31); channels (:32); cross-session peer messages (child env scrubbing) (:33); external Claude sessions read-only listing (:34); two-writer protection on resume (:35); remote agents (:36). OC mapping: all NA except **CERT13-03 sticky terminal states (LA: child-session terminal rules)**, **CERT13-07 parked-gate auto-decline of permission requests with no active turn (LA: affects OC child/background sessions asking permission after root idle)**, **CERT13-11 external sessions listing (NC: OC sessions created by TUI on the same storage)** and **CERT13-12 two-writer protection (LA: OC TUI and LUNA's server may both drive one session)**.

CC-16 services (`:49-58`), CERT16-01…10: Chrome; computer use; Remote Control; cloud sessions/teleport; Cloud Projects; artifacts; routines; ultrareview; push notifications; desktop handoff. All NA as named; the **ledger format** (service · status · interface · identity/permissions · results/assets · stop/revoke · LUNA) is reusable for OpenCode hosted rows (share, `opencode web`, GitHub agent, console/enterprise).

### 1.D `docs/PARITY.md` (native-rewrite checklist, legend done/partial/deferred)

Sections (`:8-105`): §1 Desktop app 1.1–1.13 (13 rows, CLIENT — provider-agnostic, LA as shared surfaces: composer 1.7, transcript 1.8, accounts 1.9, terminal 1.10, changes 1.11, state/connection 1.13); §2 Control plane (11 rows; Run/Steer/Interrupt/RespondInput ride `QueueCommand` :31 — LA); §3 Backend engine 3.1–3.7 (6 rows; steer→new-turn fallback :46, run journal/steering mailbox/stall watchdog gap :47 — LA); §4 Harness (8 rows: Claude, Codex, Cursor, Devin, Grok, Hermes, Pi ACP, Mock) — **no OpenCode row** (E1 gap in the checklist itself; flag to coordinator); §5 Session doc schema (5 rows; render-parts privacy policy :73 — LA); §6 Edge (4), §7 Server→edge (2), §8 Exclusions (1), deferred cross-cutting (4). IDs PAR-1.1…PAR-8.1. Only §2–§5 rows touch provider integration; summary `:109` "40 done · 6 partial" is historical.

### 1.E Checklist size by source

| Source | Rows extracted |
| --- | ---: |
| Codex parity (README + 8 audits + 2 validations) | 208 |
| Claude: CC-00…18 packages | 19 |
| Claude: G1–G12 + 4 residual + 5 corrections | 21 |
| Claude: capability inventory (54 + 3 exceptions) | 57 |
| Claude: tool matrix 26 + envelope 1 + messages 19 + controls 7 + hooks 5 | 58 |
| Claude: LUNA coverage 13 + CC-G01…G08 | 21 |
| Claude: reference integrations | 14 |
| Claude: failure cases 15 + E01–E28 + 6 contracts | 49 |
| Certification ledger CC-13 + CC-16 | 23 |
| PARITY.md rows | 54 |
| **Total** | **524** |

Counting caveat: Codex BB/T3/competitor rows (45) and Claude reference-integration rows (14) are reference-implementation observations, not LUNA requirements; the coordinator may fold them into §4 rather than the matrix. Excluding them leaves 465 requirement-bearing rows.

### 1.Z Coverage structure and evidence standards to mirror

**Dimensions.** Claude defines four independent dimensions (`cc/README.md:38-45`): Execution, Fidelity, Product connection, Verification — identical to `00-baseline.md`'s four. The capability inventory adds a finer per-row split (`cc/capability-inventory.md:9`): *native support · configured state · effective session state · LUNA presentation · control support · verification evidence*, plus "an exported type is not a loaded tool; a generic chip is not lifecycle fidelity". Recommend the OpenCode matrix keep the four baseline dimensions and record configured-vs-effective under Execution notes.

**Status vocabularies seen (historical):**
- Claude parity: `supported`, `partial`, `unverified`, `gated`, `unavailable`, `excluded by product decision` (`cc/README.md:47`). Ledger qualifier "Shared ready = relevant code merged, not package done" (`cc/current-gap-assessment.md:124`).
- Codex parity matrix: Supported / Partial / Missing / Native path supported / Dropped by user / Potentially inherited (`cx/README.md:53-75`) — free text, no fixed enum.
- Certification ledger: Certified / Supported · live QA pending / Not enabled by LUNA / Unsupported in LUNA / Externally gated / Native Claude Code only (`docs/reference/claude-code-advanced-certification.md:9-16`). The last maps to baseline `n/a` (native-client-only); "Not enabled by LUNA" has no baseline equivalent — suggest expressing it as `gated` with reason "LUNA never enables; decodes safely".
- `docs/PARITY.md:4-6`: done / partial / deferred (oldest, coarse).
- Baseline vocabulary (`00-baseline.md:73`) is a superset except "Not enabled by LUNA" and "Supported · live QA pending" (= Execution supported, Verification `unverified`).

**Package format.** Sequence table: `Package | Priority (P0/P1/P2/required) | Depends on | Ownership area` (`cc/implementation-plan.md:34-54`). Each package body ends with `**Deliverable:** … **Gate:** E-ids … Relevant files: …` (e.g. `:64`, `:72`, `:80`). Ledger refresh table: `Package | Current assessment | Remaining scope` (`cc/current-gap-assessment.md:126-146`). Stages A–E group packages (`cc/README.md:53-59`). Rule: packages are ownership boundaries, not authorization to parallelize; one owner per shared proto/engine file (`cc/implementation-plan.md:31`).

**Scenario format.** `ID | Production-path scenario and expected outcome | Repeatable artifact` (`cc/validation-plan.md:36-59`); additions use `ID | Failure expectations and required evidence` (`cc/current-gap-assessment.md:196-203`). Codex uses `Scenario | Required result` (`cx/README.md:235-254`) and `Trigger | Required evidence/result` failure tables (`cx/steering-status-validation.md:30-41`). Failure cases are written *before* implementation (`cc/validation-plan.md:5-7`; `cx/steering-status-validation.md:28`; goal failure list kept outside the clone, `cx/goal-controls-validation.md:21-23`).

**Artifact contract** (`cc/validation-plan.md:65-77`), per scenario/run directory: `manifest.json` (scenario ID, UTC, source SHA, CLI/transport/dependency versions, binary hash, sanitized account, cwd, capabilities, configured/effective policy); `commands.jsonl` (exact commands, env var *names*, exit, duration, log paths); `native-frames.jsonl` (redacted, ordered, IDs, redaction manifest); `normalized-events.jsonl`, `state-before.json`, `state-after.json`, subscription/RPC captures, blob/attachment manifests; `observer.json` (subscription lifecycle, snapshot calls, timeouts, side effects); screenshots/video where they substantiate interaction; `SHA256SUMS` + `result.md`. Original-content hash and redacted hash both recorded; redacted ≠ byte-identical.

**Evidence rules** (verbatim intent): a screenshot proves presentation, not execution; a successful RPC write proves admission, not consumption; a model's claim is not verification (`cc/validation-plan.md:77`). "A screenshot alone cannot prove delivery or retained bytes" (`cx/README.md:233`). Synthetic peers must not be labelled real-provider observations (`cx/steering-status-validation.md:26,50`). Don't poll `WatchDocMessages` (`cc/validation-plan.md:32`). Coverage artifact per native name (`cc/tool-and-event-matrix.md:106`).

**Research-baseline convention.** Claude parity kept `research-baseline.json` (keys: schema_version, sources, upstream_comparisons, runtime_versions, cli_help, official_tool_table_names, public_document_urls/hashes, verified, not_performed, product_constraints, work_packages, planned_e2e_scenarios, limitations) and `reassessment-baseline.json`, plus `SHA256SUMS` over the docs (E1, read 2026-10-06). Codex kept an external bundle with `review-manifest.json` and hashes (`cx/README.md:49`). Recommend the OpenCode docs ship an equivalent `research-baseline.json` + `SHA256SUMS`.

**Historical-label rule.** Treat every "done/certified/supported" label above as E4. The Claude reassessment itself says no CC package is certified (`cc/current-gap-assessment.md:58,124`); the Codex research says scenarios were not run (`cx/README.md:9`).

## 2. Shared LUNA contracts → reuse points

Scope: what each contract *says* (E4 doc statements at LUNA `9b3e9a2e`), not a Rust gate audit (workstream C). Where a single E2 lookup clarifies a named flag it is marked E2. "Explicit OpenCode statement" = verbatim mention of OpenCode in that contract; most contracts mention only Codex/Claude.

**Cross-cutting E2 fact:** the harness trait default `luna_runtime_delivery()` returns `RuntimeDelivery::Unsupported` (`crates/harness/src/lib.rs:228-229`); Codex overrides with `CodexMcpTools` (`crates/harness/src/codex/mod.rs:675-676`), Claude with `ClaudeMcpTools` (`crates/harness/src/claude/mod.rs:399-400`); the OpenCode adapter has no override (no `RuntimeDelivery` match under `crates/harness/src/opencode/`). Enum variants: `CodexDynamicTools` (legacy), `CodexMcpTools`, `CodexInstructionsOnly`, `ClaudeInstructionsOnly`, `Unsupported`, `ClaudeMcpTools` (`crates/proto/src/runtime_contract.rs:155-167`). There is **no OpenCode variant**; every contract that keys on "verified tool delivery" therefore treats OpenCode as unsupported today.

### 2.1 Native composer inputs — `docs/reference/native-composer-inputs.md`

- **Offers:** `luna_proto::ComposerInput` tagged `mention` (`plugin://`/`app://` URI) or `skill` (absolute host skill path); `inputItems` persisted through drafts, submitted snapshots, saved prompts, queued rows, queue edit, run requests, steering (`:3`). Images via existing `attachments` staging: `pending://` refs resolve to absolute host paths; empty text + images valid (`:5`). Recoverable `RejectedBeforeAcceptance` for invalid images/selections with the whole message restorable (`:5,7`). Offline proof script `scripts/qa-native-inputs.sh` (`:13-17`).
- **Provider hooks/flags:** admission policy is pinned to "what the standard Codex 0.159 CLI decodes" (PNG/JPEG/GIF/WebP, TIFF macOS, BMP Windows) (`:5`). "Native selections require Codex" and are re-validated against the loaded runtime's account/workspace/thread policy before dispatch, steer and fallback (`:7`).
- **Explicit OpenCode statement:** "Claude/OpenCode retain their existing image handling." (`:5`); verification step 6: "Submit ordinary Claude/OpenCode text and image requests to check their existing behavior." (`:28`).
- **Provider-specific gates mentioned:** Codex-only native selections; Codex decoder-based image admission.
- **What an OpenCode adapter must supply:** (a) an OpenCode image-admission policy (per-model input modalities from the model catalog, MIME set OpenCode/providers accept) instead of the Codex decoder list; (b) a dispatch mapping from staged absolute host paths to OpenCode file parts on **every** route (initial, follow-up, queued, steer if supported, correlated-rejection fallback, retry, restart); (c) if OpenCode skill/agent/command selection becomes a native typed input, a catalog source + validation hook analogous to the Codex catalog re-validation, else keep `inputItems` empty and text-only; (d) explicit `RejectedBeforeAcceptance` for unsupported content rather than path text.

### 2.2 Shared message queue — `docs/reference/message-queue.md`

- **Offers:** Loro movable list `SessionDoc.queue` with stable IDs, attachments, hold-until-turn-end, delivery gate (`:8-12`); host-only delivery/removal/edit lease (`:14-17`); delivery table by state (`:24-31`); queue ID retained as user-message ID (`:38`); failed dispatch restores row and pauses queue (`:38-41,90`); protected 60 s edit leases (`:45-55`); capability negotiation `message-queue-v1`, `-actions-v1`, `-attachments-v1`, `-clean-attachment-text-v1`, `-edit-lease-v1` (`:72-76`).
- **Provider hooks:** "Working or awaiting input, mid-turn steering supported → steer a text-only head"; "steering unavailable → keep queued until turn end"; "Head has attachments → wait for a new turn that can accept files" (`:27-29`). "Provider metadata must explicitly resolve the available action" (`:36`). Send-now interrupts before dispatch (`:33`).
- **Explicit OpenCode statement:** none.
- **What OpenCode must supply:** truthful provider metadata saying whether mid-turn steering exists (if OpenCode v2 only queues prompts server-side, advertise steering **unavailable** and let LUNA hold the head); a working interrupt for Send-now; turn-end signal to drain; whether follow-up turns accept file parts (they should, for attachments-wait rule); delivery evidence levels (accepted/recorded/unknown) keyed on the queue ID → OpenCode message ID mapping.

### 2.3 Tool output retention and live activity — `docs/reference/tool-output.md`

- **Offers:** host-journal retention of received output before preview limits (`:3`); synced doc keeps ≤160-char summary or ≤2 KiB live tail plus byte counts/diff stats/lazy refs (`:5`); `ToolActivity { id, kind, text }` for stdout/stdin/patch progress sharing the call/result owner; steering-boundary updates hit the original entry (`:7`); full retrieval keeps completion text plus separately labelled streamed history (`:9`); subagent detail scoped to its spawn so child IDs can't overwrite parent output; remote child detail routes to parent's host (`:9`); `lunaToolOutput` envelope for rich MCP/dynamic results; computer-use image bytes replaced by byte counts; unknown items get bounded type/field-name diagnostics (`:11`); Claude `kind: "native"` envelope with ordered typed content, `empty: true` vs unavailable `output: None` (`:30`).
- **Provider hooks:** adapter must emit `ToolCall`/`ToolActivity`/`ToolResult` with stable owner IDs and supply the envelope; Codex and Claude have provider-specific projections (`:3`, `:28-39`).
- **Explicit OpenCode statement:** none (contract names Codex and Claude only).
- **What OpenCode must supply:** an OpenCode projection into `lunaToolOutput` (e.g. a `kind` for OpenCode tool parts) carrying tool input, `state.output`, `metadata` (bash streaming output, edit diff), `attachments` (file parts/images), error, title, timing; `ToolActivity` from running-state part updates without repainting; child-session tool parts attributed to the spawning `task` call so child IDs cannot collide with parent IDs; empty-vs-unavailable distinction; bounded diagnostics for unknown part types.

### 2.4 MCP forms and URL prompts — `docs/reference/mcp-elicitation.md`

- **Offers:** blocking question panel for form/URL elicitation, schema-validated fields (`:3-5`); engine run-owned pending map with concurrent requests, retransmit dedupe (`:7`); limits (64 KiB, 16 fields, 2-min lifetime) and Stop/expiry/recovery semantics (`:9`); URL secrecy + owner-only `McpElicitationUrl` RPC (`:11-13`); script `scripts/qa-mcp-prompts.sh` (`:19-23`).
- **Provider hooks:** "Requests require the active native Codex thread and turn and a concrete server name and opaque request ID. The adapter owns that binding…" (`:7`).
- **Explicit OpenCode statement:** none. Codex-specific (`mcpServer/elicitation/request`, `:3`).
- **What OpenCode must supply (only if OpenCode v2 surfaces MCP elicitation — A/B to confirm):** a native request ID, server name, session/turn binding, a reply channel with accept/decline/cancel, and cancellation on turn end. If OpenCode has no elicitation, record `unavailable` and ensure such servers degrade visibly. Separately, OpenCode's **question** tool (if present in v2) should reuse the same blocking question panel through the engine pending-input path rather than a new dialog.

### 2.5 Chat placement and child execution choices — `docs/reference/chat-placement-and-child-models.md`

- **Offers:** presentation-only placement (no reparenting) (`:7-17`); parent-agent `create_thread` with optional `provider` and `modelOptions`, bounded read-only model catalog tool, validation before provisioning, persisted execution config (`:19-37`); failure list (`:41-71`); evidence root `target/qa/chat-placement-models/`, driver `scripts/qa-child-models-native.mjs` (modes seed/start-codex/start-claude/snapshot/verify…) (`:75-135`).
- **Provider hooks:** cross-provider children "must not carry the parent's provider-specific model, reasoning, options or account" (`:22-24`); "Permission ceilings remain enforced by the destination adapter" (`:61-62`); "Test both directions between Codex and Claude" (`:70`).
- **Explicit OpenCode statement:** none.
- **What OpenCode must supply:** (a) as **destination** of a cross-provider child: live model catalog with variants/options for the bounded catalog tool, a fresh session start with only the brief, a permission-ceiling mapping (Ask/Auto/Full → OpenCode permission config) that cannot widen access; (b) as **parent**: a verified callable tool path (see 2.7) — without it OpenCode cannot create children at all.

### 2.6 QA infrastructure — `docs/reference/qa-infrastructure.md` (rules detailed in §5.1)

- **Explicit OpenCode statement:** none. Isolation note: `LUNA_QA_MCP_ISOLATION=1` makes QA provider processes omit saved user MCP servers while keeping LUNA's `luna` tools; "the QA data directory alone does not isolate `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, or Executor" (`:83-88`). OpenCode's XDG config/data (`~/.config/opencode`, `~/.local/share/opencode`) is likewise **not** named — an OpenCode QA run would share the user's OpenCode storage/config/auth unless the adapter or QA wrapper sets OpenCode's own isolation (cf. the user's `opencode1` wrapper which isolates XDG for v1, `00-baseline.md:40`).

### 2.7 Delegation / tool discovery / workspace controls — `docs/design/agent-delegation-ux/README.md`, `tool-discovery.md`, `mcp-startup-investigation.md`; `docs/design/agent-workspace-controls/README.md`, `approval-failures.md`; `docs/pending-tasks/agent-created-child-threads.md`; `docs/qa/child-chats.md`

- **Offers:** `get_workspace`, `manage_workspace`, `create_thread`, `read_thread`/`send_message_to_thread`/`wait_threads` via the per-process `luna` MCP endpoint (`agent-workspace-controls/README.md:13-17`; `agent-created-child-threads.md:139`); async creation with `reportResults`/`continueParent` (`tool-discovery.md:48-53`); compact bootstrap in the provider's durable system/developer channel, ≤800 bytes, topic help on demand, survives compaction (`agent-delegation-ux/README.md:38-42`; `tool-discovery.md:33-46,65-66`); `#` chat references (`agent-delegation-ux/README.md:23-32`); no extra approval cards for in-app coordination; children capped at parent access (`agent-workspace-controls/README.md:50-56`).
- **Provider hooks/gates:** "Codex and Claude use their existing LUNA MCP delivery, including resumed runs. Other providers advertise these operations only when their adapter actually delivers the tools." (`agent-workspace-controls/README.md:9-11`). "Other providers remain omitted from the agent creation catalog." (`qa/child-chats.md:10`). "A Claude or other-provider adapter needs an actual verified delivery path; instructions alone do not make a tool callable." (`agent-created-child-threads.md:186`); "Additional provider transport — a harmless real creation through that transport proves the capability before it is advertised" (`:261`). "The actual attached catalog remains authoritative; unsupported adapters are not described as having tools they cannot call." (`tool-discovery.md:45-46`). Approval failure cases are Codex/Claude-specific mode mappings (`approval-failures.md:5-28`). MCP-startup isolation matrix covers Codex and Claude launch purposes only (`mcp-startup-investigation.md:17-25`).
- **Explicit OpenCode statement:** none. (Stale note: `agent-created-child-threads.md:139` still says Claude MCP transport is unverified; superseded by `ClaudeMcpTools`.)
- **What OpenCode must supply:** (1) attach LUNA's `luna` MCP endpoint to every OpenCode session start/resume/fork (OpenCode MCP config — local stdio or remote URL — injected per server process or per session; B/C to determine whether v2 can add MCP servers at runtime without touching the user's config file); (2) a new `RuntimeDelivery` variant (e.g. `OpenCodeMcpTools`) returned from `luna_runtime_delivery()` only after verification; (3) runtime contract delivered through a durable system channel (OpenCode agent prompt / `system` field on prompt / instructions file) — **not** prepended to user text; (4) catalog readback proving the tool is attached on fresh, resumed, forked and post-compaction sessions; (5) mode mapping for Ask/Auto/Full against OpenCode permission rules with the approval-failure cases re-stated for OpenCode (Full must not create persistent "always" rules; stale/foreign requests never auto-granted); (6) launch-purpose MCP isolation (metadata probes/title runs should not start user MCP servers) — OpenCode is absent from the startup matrix.

### 2.8 History: rewind, forks — `docs/qa/same-chat-rewind.md`, `docs/design/persistent-fork-threads/README.md`, `docs/reference/related-chats-redesign.md`

- **Offers:** same-chat rewind removing selected user turn and later turns, files kept, checkpoint restore separate and previewed; provider-reconciled exact retained prefix; never replay revert (`same-chat-rewind.md:6-9,28-33`); script `scripts/qa-same-chat-rewind.mjs` run/snapshot (`:46-70`). Persistent forks: provenance not lifetime; native vs portable transcript snapshot context (`persistent-fork-threads/README.md:4-27`; `agent-workspace-controls/README.md:36-37`).
- **Gates:** rewind operates on "the same Codex session"; "Native legacy/ephemeral threads are unavailable" (`same-chat-rewind.md:6-9`). Fork failures name "Codex or Claude native forks" (`persistent-fork-threads/README.md:72-74`).
- **Explicit OpenCode statement:** none.
- **What OpenCode must supply:** a history-capability declaration distinguishing native fork (OpenCode session fork at a message ID) from portable snapshot; native truncation semantics — OpenCode's revert/unrevert is a **reversible pointer that also restores files from snapshots** (E5, to confirm by A/B), which differs from LUNA's "files are kept" rewind contract, so either map revert with file restore disabled/declined, or declare rewind unavailable and keep fork-only; provider readback (message list) for `reconcileConversationRewind`-style exact-prefix checks; idle-writer release before mutation.

### 2.9 Native review — `docs/qa/native-review.md`

- **Offers:** context-menu Review with four targets, inline or separate chat, durable `nativeReview` admission kind, stable findings part IDs, intent preserved for exact retry (`:3-7`); script `scripts/qa-native-review.mjs` (`:26-29`).
- **Gate:** "Reject … a non-Codex harness" (`:12`); every review is native `review/start` (`:5`).
- **Explicit OpenCode statement:** none.
- **What OpenCode must supply:** only if OpenCode v2 has a native review operation (command/agent/endpoint — A to confirm): a typed dispatch route plus findings mapping; otherwise ordinary-chat review remains and the action stays Codex-only (`unavailable` for OpenCode, not a duplicate review UI).

### 2.10 Goals — `cx/goal-controls-validation.md` (contract) and Claude assessment G9

- **Offers:** goal editor/status row, provider-owned scheduling, cached readback with `checkedAtMs`, `ProviderTurnStarted` for autonomous turns, task-command fences, unknown never replayed (`:9-17`); script `scripts/qa-goals-live.mjs` (`:41-53`).
- **Gate:** Codex `thread/goal/*`; Claude reassessment: "Engine and UI require Codex" (`cc/current-gap-assessment.md:41`).
- **Explicit OpenCode statement:** none.
- **What OpenCode must supply:** nothing unless a native goal mechanism exists (none known; UNK). **Reusable piece regardless of goals:** `ProviderTurnStarted` handling for provider-initiated turns — relevant if OpenCode starts autonomous turns (e.g. after compaction auto-continue or a child completion re-prompting the parent), so they open a new segment/checkpoint instead of being folded into the previous turn.

### 2.11 Permissions/approvals — `docs/design/agent-workspace-controls/approval-failures.md`, `docs/qa/thread-approvals-2026-10-03.md` (see §3 for its OpenCode mention), CC reassessment "Permission details" row (`cc/current-gap-assessment.md:39`)

- **Offers:** shared approval detail contracts, choices and UI; one-use approval cards; Ask/Auto/Full semantics; failure cases 1–8 (`approval-failures.md:5-28`).
- **Gate:** "Claude emits `details: None`" (shared details available but per-provider projection required) (`cc/current-gap-assessment.md:39`).
- **What OpenCode must supply:** projection of OpenCode permission requests (type/tool, patterns, metadata such as command or diff, call ID, session ID) into the shared approval detail contract; reply mapping once/always/reject with LUNA's rule that Full never creates persistent rules; child-session permission requests attributed to the child chat or the parent's spawning call (stale-approval cases in §5).

### 2.12 Reuse summary (what not to duplicate)

| Shared surface | Contract | OpenCode plug-in requirement | Current gate as stated |
| --- | --- | --- | --- |
| Composer staging/drafts/typed inputs | native-composer-inputs | OpenCode image admission + file-part dispatch on all routes | Native selections Codex-only; "Claude/OpenCode retain their existing image handling" |
| Durable queue | message-queue | Steering capability truth, interrupt, turn-end, delivery evidence | "Provider metadata must explicitly resolve the available action" |
| Output journal/viewer | tool-output | OpenCode `lunaToolOutput` projection + `ToolActivity` | Codex + Claude projections only |
| Question panel / MCP forms | mcp-elicitation | Native request ID + binding + reply, or `unavailable` | Active native Codex thread/turn required |
| Child chats / placement / model choice | chat-placement-and-child-models; agent-created-child-threads | Destination: catalog + fresh session + ceiling; Parent: verified `luna` MCP delivery | "Other providers remain omitted from the agent creation catalog" |
| Workspace tools / delegation | agent-workspace-controls; agent-delegation-ux | `OpenCodeMcpTools` delivery + system-channel bootstrap + post-compaction discovery | "Other providers advertise … only when their adapter actually delivers the tools" |
| Rewind / forks | same-chat-rewind; persistent-fork-threads | Native fork vs snapshot declaration; revert semantics reconciliation | Codex session only |
| Review | native-review | Native review op or `unavailable` | "Reject … a non-Codex harness" |
| Goals | goal-controls-validation | none known; reuse `ProviderTurnStarted` | Codex only |
| Approvals | approval-failures; shared details | OpenCode permission projection + mode mapping | Per-provider `details` projection |
| QA isolation | qa-infrastructure | OpenCode storage/config isolation for QA bundles | Only `CODEX_HOME`/`CLAUDE_CONFIG_DIR`/Executor discussed |

## 3. Historical OpenCode evidence (E4)

Method: `rg -il opencode docs` (E1, 2026-10-06) → 45 files; excluding this study's own notes/baseline (6), UI-mock SVGs (5) and workspace-research evidence (3 Conductor/LUNA AX dumps), 31 substantive mentions remain. Supplemented with `git log -i --grep=opencode` on LUNA (commit messages are historical evidence too). **No LUNA doc records a real OpenCode model turn through the LUNA app or engine.** The only live real-binary OpenCode evidence is in commit messages and harness probe examples, not retained artifacts.

### 3.1 Docs catalogue

| Date | Doc (file:line) | OpenCode version | What was verified / stated | Evidence class | Artifact |
| --- | --- | --- | --- | --- | --- |
| 2026-09-05 | `docs/performance-resource-usage.md:94-98,138-139,164` | not stated (v1 era) | `/provider` catalog decode was 112 MB heap peak; now streams required IDs/names/variant keys; OpenCode child processes sampled separately | real binary (heaptrack profiling) | not retained in docs |
| 2026-09-17 | `docs/research/model-discovery-reliability.md:33,38,48` | not stated (v1 era) | full HTTP provider catalog read, refresh + overlapping-request sharing; errors surfaced not cached; `opencode_models_probe` → 8 live models (Linux) | real binary probe (no LUNA app) | none |
| 2026-09-26 | `docs/qa/backend-audit-2026-09-26.md:24,30,109` | not stated | Defect: OpenCode silently started a fresh session when resume lookup failed (missing session and HTTP 503) → corrected to surface failure, preserve binding, send nothing | fixture (synthetic HTTP) | audit logs referenced, path not in doc |
| 2026-09-26 | `docs/qa/provider-audit-2026-09-26.md:97-105` | n/a | Two inherited OpenCode HTTP fixtures (`4902c7925`) fixed for Clippy (assert request bytes arrived); "Claude and OpenCode coverage is synthetic" | fixture | `clippy.log`, `clippy-rerun.log` (worktree e577) |
| 2026-09-26 | `docs/qa/parallel-audit-2026-09-26.md:55` | n/a | "Claude/OpenCode lifecycle fault injection remains fixture coverage" | fixture | — |
| 2026-09-26 | `docs/qa/provider-native-qa-2026-09-26.md:30` | n/a | "Claude/OpenCode coverage remains fixture-based" | fixture | — |
| 2026-09-26 | `docs/qa/sidebar-side-chats-2026-09-26.md:39,137` | n/a | "LUNA uses the existing receipt-backed native fork path for Codex and OpenCode"; Claude/OpenCode lack a read-only sandbox contract so strict ReadOnly side chats must refuse creation | source/design statement | — |
| 2026-09-26 (undated in doc) | `docs/qa/child-chats.md:33` | n/a | "Claude/OpenCode agent transports remain omitted until actual creation delivery is verified" | source statement | — |
| 2026-10-03 | `docs/qa/backend-review-2026-10-03.md:97-100` | **2.0.14 installed** | Version recorded only; live logs exist for Codex and Claude, **none for OpenCode** | version readback only | `live-codex.log`, `live-claude.log` (no OpenCode log) |
| 2026-10-03 | `docs/qa/thread-approvals-2026-10-03.md:37,129` | n/a | "OpenCode Auto — Unavailable: no verified native automatic reviewer"; "OpenCode's blanket approval flag is not represented as a native reviewer" | source/design | — |
| 2026-10-04 | `docs/pending-tasks/orchestrator-v2-and-bb-2026-10-04/luna-integration-plan.md:27,60,213` | 1.x + 2.x | "OpenCode adapter supports both 1.x and 2.x wire layouts — Existing; do not propose 'add OpenCode 2' as a blank feature"; E2E acceptance for runtime settings must record requested/effective settings for Codex, Claude **and OpenCode** | source (links `crates/harness/src/opencode/mod.rs:459`) | — |
| 2026-10-04 | `…/orchestrator-v2-and-bb-2026-10-04/README.md:40,65` | 1/2 | Provider loop belongs to OpenCode etc.; "OpenCode 1/2 integration" listed as an existing foundation | source | — |
| 2026-10-04 | `…/orchestrator-v2-and-bb-2026-10-04/evidence-and-tests.md:108` | n/a | "The two tested providers do not certify … every OpenCode version" | statement | — |
| 2026-10-04 | `…/orchestrator-v2-and-bb-2026-10-04/bb-delta.md:113` | v2 | BB added "OpenCode v2 credentials/questions" (see §4) | reference repo log | `evidence/bb-since-parity-20261004.txt:63,66` |
| 2026-10-04 | `…/orchestrator-v2-and-bb-2026-10-04/t3-orchestrator-v2.md:149` | 1 and 2 | "OpenCode 1 and 2 have distinct adapters" in T3 (see §4) | reference repo source | `evidence/t3code-since-parity-20261004.txt:111,134` |
| 2026-10-05 | `docs/design/harness-evolution/README.md:46` | n/a | "Codex and Claude are the live acceptance targets; OpenCode is outside this delivery's acceptance scope" | scope exclusion | — |
| 2026-10-05 | `docs/design/harness-evolution/verification.md:91` | n/a | "No live OpenCode model session was exercised in this slice" (execution observations/runtime settings) | scope exclusion | — |
| 2026-10-05 | `docs/design/harness-evolution/slice-2-verification.md:99-100` | n/a | Portable context: "no native OpenCode continuity is claimed" | scope exclusion | — |
| 2026-10-05 | `docs/design/harness-evolution/slice-3-ui.md:5`, `slice-5-ui.md:7` | n/a | Shared background work and automations UI: "OpenCode is out of scope" | scope exclusion | — |
| 2026-10-05 | `docs/qa/agent-workspace-controls-2026-10-05.md:102-104` | n/a | "The existing OpenCode adapter does not yet deliver these tools, and discovery reports that limitation" | source + discovery behaviour | — |
| undated (managed skills) | `docs/reference/managed-skills-qa.md:5`; `managed-skills-ax-qa.md:6-8` | n/a | Synthetic provider homes incl. OpenCode config root; signed `LUNA QA Skills` build showed the OpenCode skill library and saved `qa-opencode-skill`; AX readback omitted skill buttons (defect) | native GUI QA on synthetic homes (no model turn) | not named in doc |
| undated | `docs/reference/native-composer-inputs.md:5,28` | n/a | "Claude/OpenCode retain their existing image handling"; verification step 6 asks for ordinary OpenCode text+image requests | planned check (not recorded as run) | — |
| undated | `docs/context-usage.md:16` | n/a | Context ring source for OpenCode: latest parent assistant message total or input/output/cache; capacity from provider/model catalog; empty placeholders don't clear | source/design | — |
| undated | `docs/reference/windows-development.md:29,33` | n/a | opencode resolved from PATH/known install paths on Windows; `.cmd` shims | source | — |
| undated | `docs/research/sidebar-view-options.md:221` | n/a | OpenCode omits the external-session sidebar item until implemented | design | — |
| 2026-10-06 | `docs/design/ui-polish-audit-2026-10-06/findings/surfaces.md:61-65`; `design-system.md:139,158` | n/a | Usage card colour bug shown with an OpenCode window at 99%; OpenCode brand mark | screenshot `shots/surfaces/ss-usage-card-light-opencode.png` | UI shot |

### 3.2 Commit-message evidence (LUNA `git log -i --grep=opencode`, E1 read of E4 content)

| Date | Commit | OpenCode version | What was verified | Class |
| --- | --- | --- | --- | --- |
| 2026-08-18 | `0213cacf` / PR #157 | v1 | First adapter: **ACP** harness with subagent visualization off the sidecar event bus | source |
| 2026-08-19 | `7c6c1236`, `296813f6`, `454a1dd3`, `54b2d45e` | v1 | Surface provider failures instead of silent forever-Working; effort rides model variants; subagent steer durability incl. OpenCode resume; subagent transcripts forward user messages | source/tests |
| 2026-08-20 | `fde9b4b1`, `569793ed` | v1 | Harden model discovery; genus-gated subagent binding for every harness | source/tests |
| 2026-08-22 | `bf634445`, `0b421cc2`, `a401432f`, `4fd35579` | v1 | **Native HTTP/SSE driver replaces ACP**; bound every HTTP call (boot-window request parked forever); gate first prompt on a live event subscription; picker offers only connected providers' models | source/tests |
| 2026-09-16 | `67c960f4` / PR #376 | **1.18.31 and 2.0.3** | Retry session create once on 5xx (1.18 lazy migration on a data dir rewritten by 2.x — live `SQLiteError: no such column: project_id`); dual-wire detection via `/api/health` vs `/global/health` "verified live against 1.18.31 and 2.0.3"; 2.x session create with `location.directory`, `POST /api/session/{id}/model`, `/api/event` with Accept header, v2 frame rewrite (`session.execution.*`, `session.step.started`, `session.text.delta`, `session.tool.*`, `session.usage.updated`, `permission.asked`); models from `GET /api/model` with short poll (models.dev sync); abort → `POST /api/session/{id}/interrupt`. "Verified live … with the new opencode_turn_probe example: discovery (64 models on 2.x), plain and tool-using turns, and permission auto-approve on 2.0.3; tool turns on 1.18.31 unchanged." Follow-up in same PR: one-time permission replies only for owned sessions, honor interactive approval, preserve reasoning/failure/interruption events; wire-level regressions added to required CI | **real binary via harness probe (no LUNA app/engine/UI)** + fake-server tests |
| 2026-09-23 | `5aaa62c5`, `ff45bdba`, `4370b10e` | Go plan | Synthetic OpenCode Go limits/history metrics; WIP Go usage adapter preserved | synthetic |
| 2026-10-06 | `0c7ebc87`, `8e67abc9` | n/a | Usage card/settings gallery touch OpenCode rows | UI fixture |

**Key implications for this study.** (1) The adapter's claimed 2.x baseline is **2.0.3** (2026-09-16), 19 patch releases behind installed v2.0.22; 2.0.14 was merely installed on 2026-10-03. (2) The 2.0.3 live verification exercised discovery, plain and tool turns and permission auto-approve through `crates/harness/examples/opencode_turn_probe.rs` only — no engine, document, UI, image, child, history, reconnect or restart path. (3) The 1.18↔2.x shared-data-dir migration hazard is real historical evidence relevant to the v1/v2 resolver question (`00-baseline.md:61`). (4) All other OpenCode coverage in QA docs is fixture-based or explicitly out of scope. (5) No retained OpenCode artifact directory (manifest/hashes) exists in any doc.

## 4. T3 and BB OpenCode patterns

Both integrate OpenCode (E2). Their coverage is not the ceiling; patterns only. Pins: T3 `4ee6bfd50e` (paths under `apps/server/src/`), BB `a2bbfaad` (paths repo-relative).

### 4.1 T3 Code — two native adapters (v1 SDK + v2 client), HTTP/SSE, not ACP

- **Generation split.** `provider/opencodeVersionProbe.ts:1,14-17,29-37`: classifies 1.x vs 2.x from `opencode --version` (`1.18.32` vs `opencode v2.0.18`) and from server probe — 2.x answers `/api/info` with `{version,pid}`, 1.x answers `/global/health` `{healthy:true,version}` (`:20-27`, comment `:38-40`). Driver builds both runtimes and picks per instance (`provider/Drivers/OpenCodeDriver.ts:27-57,279-301`). v1 adapter uses `@opencode-ai/sdk/v2` types (`orchestration-v2/Adapters/OpenCodeAdapterV2.ts:1-11`); v2 adapter uses OpenCode's own `@opencode/client/effect` **2.0.18** (`apps/server/package.json:37-40`; `OpenCode2AdapterV2.ts:24-37`). **Contrast:** LUNA probes `/api/health` vs `/global/health` (`00-baseline.md:63`); T3 uses `/api/info` for 2.x — B should confirm which is stable across 2.0.22→2.0.24.
- **Server ownership.** One shared `opencode serve` per provider instance serving all directories (`provider/opencode2/OpenCode2Server.ts:1-6`); lazy owner with borrower count and 30 s idle TTL (`provider/OpenCodeServerOwner.ts:11-18,30`); a process-group **ledger** that records pgid + process identity (pid, start time, command line) before signalling orphaned servers (`provider/OpenCodeServerLedger.ts:14-25`). External (user-run) servers are supported (`OpenCode2Server.ts:24-27` `external`).
- **Turn model and correlation.** "A turn is one `session.prompt` (or `session.command`, or `session.compact` for `/compact`); the session's next `session.execution.*` terminal ends it" (`OpenCode2AdapterV2.ts:6-7`). T3 supplies its **own prompt id** (`turnPromptId(sessionId, attemptId)`) so fork/rollback can cut before that turn (`:3499-3500`, `:685-701`); `session.command` takes no client id, so T3 records the newest message id before sending (`:3514-3519`). Terminal event set `session.execution.succeeded|failed|interrupted` (`:2137-2139`, `:2280-2290`). Capability declaration: `nativeTurnIds: "weak"`, `nativeItemIds/RequestIds: "strong"`, `terminalStatusQuality: "strong"` (`:100-196`, esp. `:118-128,184-189`). Handled event families: text/reasoning started/delta/ended, `session.tool.input.started|called|progress|success|failed`, `session.compaction.*`, `session.step.ended|failed`, `session.execution.*` (`:2180-2290`), plus `session.created` with `parentID`, `permission.asked|replied`, `form.created|replied|cancelled`, `session.model.selected`, `session.inbox.enqueued|delivered|cancelled`, `unreadable.execution.started|ended` (`:2428-2600`).
- **Steering and queue.** "A steer is another prompt with `delivery: "steer"`, which OpenCode reads at the running execution's next step boundary, so it ends with the turn it joined" (`:8-10`). Steers settle on `session.inbox.delivered|cancelled` by inbox ID (`:2573-2581`). T3 does **not** use OpenCode's own `queue` delivery; it holds queued messages itself (`:124-127`) — same shape as LUNA's durable queue. Prompt failures: only `InvalidRequestError`/`ConflictError`… mean "refused"; "any other failure may have been accepted" → marked unsettled/stranded, not replayed (`:657-660`, `:3830`, `:3890`).
- **Reconnect.** "The stream is the only terminal signal, and it is volatile: events sent while it is down are gone, and a restarted server never ends the execution it lost" (`:2619-2624`). Resubscribe 5× at 2 s; reconcile within 15 s; new turns wait ≤30 s for a reconnect (`:645-650`, `:2829-2832`, `:3685`). Reconcile reads server state: running session keeps its turn; stopped session ends with the outcome of the `idle` item after the turn start; no `idle` ⇒ interrupted; pending permission asks and forms are read back; background subagent work is settled as interrupted, not resumed (`:2712-2733`). Lost background result text constant (`:651-653`).
- **Approvals and questions.** Runtime modes compile to **session permission rules** (`sessionRules`, last match wins) — full-access `*: allow`, else `shell`/`edit`/`external_directory` ask; plan mode denies `edit` except plan paths (`:466-485`). Permission asks show Cancel / Decline / optional session grant / Approve; "'Always' in OpenCode saves a grant for the whole project, so the session-wide choice is T3's own rule on this session instead" (`:2034-2064`, `:510-514`). "A reject without a message is OpenCode's 'stop': it ends the whole execution, which is Cancel" — declines send a message, plus a steered note so the model doesn't retry (`:495-508`). Question **forms** map to structured questions; "OpenCode's question tool only asks text and multi-select fields"; unsupported fields fall back to a link or decline (`:519-560`, `:2066-2075`). Subagent asks route to the parent's thread (`:13-15`, `:152`). `pendingRequestsSurviveRestart: false` (`:107`).
- **Child sessions.** "A `subagent` call runs in a child session shown as a subagent thread. A background one outlives its turn; when it ends, OpenCode starts a parent execution T3 did not ask for, which waits here for the continuation turn T3 opens for it" (`:17-20`). Child ownership via `session.created.parentID` before the call names it (`:2456-2462`); nested depth handled deepest-first (`:937-959`, `:2740-2745`); `:wake:` native IDs mark wake executions (`:640`); subagent tool ids are prefixed to avoid collisions (`:267-270`); subagent output unwrapped from `<subagent …>` (`:630-632`); usage excludes subagents (`:271`, `:792-809`). Note the **tool name is `subagent`** in 2.0.18 (A to confirm vs v1 `task`).
- **App tools (MCP) and instructions.** Per-thread remote MCP registered with `client.mcp.add({server, location:{directory}, config: RemoteConfig{url, headers:{Authorization}, oauth:false}})`, removed on unload; skipped for external servers; re-added after server restart (`:3217-3267`, `:3200-3212`, `:2827-2828`). Isolation by permission rules: deny `t3-code-*`, allow `t3-code-<thread>_*`; OpenCode names MCP tool permissions `<server>_<tool>` (`:445-465`). Instructions via `client.session.instructions.entry.put({key:"t3-code"})` because "OpenCode 2 has no per-prompt system field", applied from next step, rewritten only on change (`:3219-3224`, `:3270-3284`, `:643`). Declared `acceptsSystemContext/DeveloperContext: false`, `acceptsSyntheticUserContext: true` (`:168-176`).
- **Images.** **Weakness:** attachments become path text via `providerMessageTextWithAttachmentPaths` on prompt and steer (`:3195-3199`, `:3875-3880`); no file parts. Typed skills **are** sent: `$skill` mentions resolved against `client.skill.list` become `skills:[{id}]` on the prompt (`:3528-3545`).
- **History/fork.** Rollback = `session.revert.stage({messageID, files:false})` then `revert.commit`, clearing a staged revert on failure because "OpenCode commits a staged revert on the next prompt" (`:3092-3115`); `revert.clear` triggers an empty wake execution that must be ignored (`:2429-2443`, `:884-887`). Fork = `client.session.fork({sessionID, before?})` — copies history, model, location and rules with new message IDs (`:4183-4189`). Capabilities: rollback/fork/fork-from-turn yes, fork-from-subagent no (`:109-116`).
- **Model discovery.** v2 model loader polls until non-empty (250 ms spacing, 5 s timeout) and keeps last good list (`provider/Layers/OpenCodeProvider.ts:395-411`); variants from `model.variants[].id` with inferred default (`:486-500`, `:150-162`); workspace skills/commands need a directory scan — first read may list nothing until `command.updated`/`skill.updated` (`:413-440`).
- **Verification assets.** Live contract test against a real 2.x binary gated by `OPENCODE2_BIN`, free model `opencode/big-pickle` (`provider/opencode2/OpenCode2Client.live.test.ts:1-9`); HTTP-boundary transcript replay testkit using the real client (`orchestration-v2/Adapters/OpenCode2AdapterV2.testkit.ts:1-5`).
- **Known weaknesses:** images as text; pending requests lost on restart; background subagent results lost on stream loss; `streamsToolOutput: false` (`:131`); weak native turn IDs; `canCloseSubagents: false`; MCP tools only on spawned servers; OpenCode 1.15 MCP nested-union JSON-string quirk worked around (`packages/contracts/src/orchestratorMcp.ts:50-54`).

### 4.2 BB — OpenCode over ACP (`opencode acp`)

- **Transport:** generic ACP agent definition `acp-opencode`, `command: "opencode", args: ["acp"]`, `fork: "tip"`, `supportsManualCompaction: true`, `providerUsage: true` (`plugins/provider-acp/src/known-agents.ts:86-113`). OpenCode dialect only normalizes command results (`output`/`metadata.exit`) into shared stdout/exitCode (`packages/provider-bridge-acp/src/dialect.ts:397-447`).
- **Events/turns:** ACP `session/update` + prompt `stopReason` → turn status (`packages/provider-bridge-acp/src/delta-translation.ts:806-837,885`).
- **Approvals/questions:** ACP `session/request_permission` (`bridge/bridge.ts:1399-1401,2229`). **Weakness:** native OpenCode `question` tool is **disabled** via `OPENCODE_CLIENT=acp` and `OPENCODE_ENABLE_QUESTION_TOOL=false` because it otherwise left turns waiting indefinitely (commit `1f73cd73`, 2026-10-01); BB substitutes its own AskUserQuestion plugin.
- **Images:** ACP image content blocks only when the agent advertises `promptCapabilities.image`; otherwise or for URL/unreadable images, text markers (`bridge/bridge.ts:1305-1326,1789-1790`; `wire.ts:234-238`).
- **App tools:** BB tool-proxy MCP server `bb-bridge` passed in ACP `session/new` `mcpServers` (`bridge/tool-proxy-mcp.ts:12-19`; `bridge/bridge.ts:1801-1875`).
- **Child sessions / history / model discovery:** no OpenCode-specific child-session handling (ACP generic); fork limited to `tip` (`plugins/provider-acp/src/agents.ts:29`); native skill roots from `$XDG_CONFIG_HOME/opencode` or `~/.config/opencode`, `OPENCODE_CONFIG_DIR`, `.opencode/skills` (`plugins/provider-acp/src/native-roots/opencode.ts:8-30`; `known-agents.ts:103-110`).
- **Usage/credentials:** reads OpenCode Go credentials from v2 `opencode.db` (tables `account`, `account_state`, `credential`) or v1 `auth.json` under XDG data (`packages/provider-bridge-acp/src/bridge/opencode-usage.ts:4,57-67,81,100,148`; commit `a312ed2d`). Pattern noted, **not** recommended (reads secrets directly; LUNA rule is no credential copying).
- **Weaknesses:** ACP flattens OpenCode's richer v2 events (no child-session attribution, no forms, tip-only fork); question tool removed; images depend on ACP capability.

### 4.3 Adoptable patterns for LUNA (ranked)

1. **Dual generation detection by CLI version string + server probe**, cached per binary path (T3 `opencodeVersionProbe.ts`), combined with LUNA's resolver evidence (`00-baseline.md:55-62`). Record the generation and version in every E2E manifest.
2. **Client-supplied prompt/message IDs** as the turn correlation and history-cut anchor; treat non-refusal prompt errors as **delivery unknown**, never auto-replayed (T3 `:657-660,3499,3830`). Maps directly to LUNA's queue ID = user-message ID and accepted/recorded/unknown receipts.
3. **Inbox-based steering truth** — if v2.0.22 has `delivery: "steer"` + `session.inbox.*`, LUNA can advertise real mid-turn steering with acceptance (`enqueued`) vs incorporation (`delivered`) vs `cancelled` evidence, satisfying the Codex-derived steering contract (§1.A.4) without interrupt/restart. B must confirm availability in 2.0.22.
4. **Reconnect + reconcile from server state** (running/idle item/pending asks), with a bounded retry budget and truthful interrupted outcome when no `idle` exists (T3 `:2619-2733`).
5. **Permission rules per session for Ask/Auto/Full**, never using OpenCode "always" (project-wide persistence) for LUNA's session grant or Full mode (T3 `:466-485,2047-2050`) — aligns with LUNA approval-failure cases 1–2.
6. **Per-session durable instructions entry** for the LUNA runtime contract (T3 `:3219-3284`) — satisfies the delegation contract's "durable system/developer channel, not user text".
7. **Per-thread MCP registration + permission-rule isolation** for LUNA's `luna` endpoint, re-added after server restart (T3 `:445-465,3217-3267,2827`).
8. **Child-session ownership from `session.created.parentID`** before the spawning call names it; prefix child tool IDs; deepest-first reconciliation; wake executions after background subagents are provider-initiated turns (reuse LUNA `ProviderTurnStarted`).
9. **Revert with `files:false`** + clear staged revert on failure — matches LUNA's rewind contract "files are kept" (`docs/qa/same-chat-rewind.md:6-9`).
10. **Live contract test gated by env + free model; HTTP-boundary replay testkit** (T3) — mirror as LUNA E2E tiers (§5).

Do **not** adopt: path-text images (T3, BB fallback), disabling the native question tool (BB), reading credential stores (BB), ACP as primary transport for v2 (BB) — LUNA already moved from ACP to native HTTP/SSE on 2026-08-22 (`bf634445`).

## 5. Verification planning

### 5.1 QA infrastructure rules (as stated; E4 docs, binding on any future implementation)

| Rule | Source |
| --- | --- |
| One LUNA GUI app at a time, including QA and installed copies; `run-macos-dev.sh` refuses a second running app and serializes app operations across linked worktrees (`luna-app-operation.lock`, `.refresh-lock`; exit 75 = slot busy) | `AGENTS.md` "macOS developer app handoff" ¶4; `docs/reference/qa-infrastructure.md:26-28,33-35,104-111` |
| Before native QA, let the current app save and quit gracefully; never force-quit to free the slot; if it declines to quit, leave it and report | `AGENTS.md` ¶3-4; `qa-infrastructure.md:33-35` |
| QA bundles require all four overrides `LUNA_DEV_APP_NAME`, `LUNA_DEV_BUNDLE_ID`, `LUNA_DEV_IPC_PORT`, `LUNA_DEV_DATA_DIR`; verify process, port and signature; never refresh main's identity from a QA worktree. Primary = `LUNA Dev`, `com.novnski.luna.dev`, port 59777 | `AGENTS.md` ¶4; `qa-infrastructure.md:61-77,94-98` |
| Assigned QA identities table (Providers 59781 … Sidebar 59787); a new OpenCode QA identity needs a distinct name/bundle suffix/port/data dir assigned before packaging (collision scan is not atomic) | `qa-infrastructure.md:51-59,99-102` |
| QA bundles set `LUNA_WORKTREES_DIR` inside the QA data dir and `LUNA_QA_MCP_ISOLATION=1` (omit saved user MCP servers, keep LUNA `luna` tools); data dir alone does **not** isolate `CODEX_HOME`/`CLAUDE_CONFIG_DIR`/Executor — and nothing documents OpenCode config/data isolation | `qa-infrastructure.md:77-92` |
| Verify readiness: `codesign --verify --deep --strict`, `plutil -p Info.plist`, `lsof -iTCP:<port> -sTCP:LISTEN`, `ps -p <pid>`; record refresh duration, retain `last-build.log` | `qa-infrastructure.md:68-75,94-95`; `AGENTS.md` ¶2 |
| Build budget: `scripts/cargo.sh` (mbx wrapper), `--locked`; `python3 scripts/build-budget.py --status` before builds; 170 GiB aggregate ceiling, 25 GiB per-build reservation, exit 75 = coordinate, don't bypass; `CARGO_INCREMENTAL=0` for one-off validation | `AGENTS.md` "Rust build and disk hygiene"; `cc/validation-plan.md:83-89` |
| Evidence preservation: preserve and hash evidence after review; confirm no process/task needs the bundle; quit QA app gracefully; unregister from LaunchServices; remove only that disposable bundle; keep its data and the Cargo cache; reopen primary `LUNA Dev` after QA stops | `AGENTS.md` ¶5; `qa-infrastructure.md:35-42` |
| Failure-first: write failure cases before implementing; no unit tests after implementation; E2E is the proof | `qa-infrastructure.md:3,41-42`; user global CLAUDE.md |
| Evidence must live outside disposable checkouts; synthetic fixtures must never be labelled real-provider proof | `docs/reference/native-composer-inputs.md:31`; `cx/steering-status-validation.md:26,50` |
| No worktree-creating QA harnesses for provider acceptance (use ordinary disposable Git repos) | `cx/steering-status-validation.md:45`; `docs/qa/child-chats.md:61` |

### 5.2 Existing harnesses, evidence conventions and OpenCode-capable assets

**Live app drivers (WebSocket JSON-RPC to the app IPC port).** Pattern (e.g. `scripts/qa-task-work-live.mjs:10-37,48-68`): assert exact bundle signature, plist identity/port/data dir, single listener PID whose executable is the bundle; create evidence dir `target/qa/<feature>/…/run-*` with a `latest` pointer; init an ordinary disposable Git fixture repo; save `manifest.json` **before** each attempted send; never replay failed/unknown sends; call `WatchDocMessages`, `GetTaskWork`, `GetTaskIdentity`, etc.; write `pass.json` on success; modes `run` / `verify-restart` / `snapshot` / `inspect`. **None of the 25 `scripts/qa-*.mjs` drivers accepts OpenCode**; they hard-code `codex` and/or `claude-code` (E1 grep, 2026-10-06). `scripts/qa-child-models-native.mjs` has `start-codex`/`start-claude` modes only (`docs/reference/chat-placement-and-child-models.md:131-135`).

**Offline/fixture harnesses with retained artifacts:** `scripts/qa-steering-status.sh` (deterministic dir, commands, exit codes, logs, adapter event traces, SHA-256 manifest; `cx/steering-status-validation.md:22-26`); `scripts/qa-native-inputs.sh` (`SHA256SUMS`, exit-code files; `native-composer-inputs.md:13-17`); `crates/engine/tests/tool_output.rs` with `LUNA_TOOL_OUTPUT_EVIDENCE` (`doc.json`, `preview-events.json`, `journal.jsonl`, `command.txt`, `patch.json`, `mcp.json`, `child-doc.json`, `child-output.txt`, `result.json`; `tool-output.md:15-24`); `scripts/qa-mcp-prompts.sh` (four assertion artifacts + SHA-256 manifest; `mcp-elicitation.md:19-23`); `scripts/verify-qa-infrastructure.sh` (`target/qa/qa-infrastructure.*`); `scripts/e2e-smoke.sh` (two headless engines + local edge via wrangler, mock harness, cross-device doc-queued Run — template for the **remote-device** scenario); `scripts/qa-backend-audit.sh:44` runs `--test opencode` among provider suites.

**OpenCode-specific assets (E2, LUNA `9b3e9a2e`):**
- `crates/harness/examples/opencode_turn_probe.rs:1-8` — one full turn through the native driver against a chosen binary/model; exits 0 only on Completed with text; isolated temp workspace. Used for the 2.0.3 live verification (`67c960f4`).
- `crates/harness/examples/opencode_models_probe.rs:1-7` — boots `opencode serve`, reads the provider catalog; anonymous Zen tier, no auth.
- `crates/harness/examples/opencode_subagent_probe.rs:1-12` — task-chip correlation + bus-tailed subagent transcript; rig uses a **mock OpenAI-compatible provider in the workspace's `opencode.json`** so no login is needed (docstring still names v1 `/global/event`).
- `crates/harness/tests/opencode.rs` + `crates/harness/src/opencode/tests.rs` — fake-server and normaliser tests for both wires (C classifies).
- Precedent for a LUNA MCP delivery probe: `crates/harness/examples/codex_luna_mcp_probe.rs`, `claude_luna_mcp_probe.rs` (no OpenCode equivalent).
- External reference: T3's env-gated live contract test with free model `opencode/big-pickle` and HTTP replay testkit (§4.1).

**Evidence directory conventions in use:** in-repo `target/qa/<feature>/…` (e.g. `target/qa/chat-placement-models/`, `target/qa/agent-delegation-ux/`, `target/qa/harness-evolution/slice-3/native`) and external `…/luna-feature-checkouts/<date>-<topic>/evidence/<feature>/…` (Codex parity). Common files: `manifest.json`, `SHA256SUMS`/SHA-256 manifest, `readback.json`, `*.pass`/`pass.json`, `result.json`, screenshots, AX readback, JSONL journals.

**Gap:** no OpenCode live driver, no OpenCode raw-SSE capture tool, no OpenCode MCP-delivery probe, no OpenCode QA identity, no documented OpenCode storage isolation. These are planning inputs, not proposals to build now.

### 5.3 What a repeatable OpenCode E2E artifact must contain

Extend the Claude artifact contract (§1.Z) with OpenCode-specific fields. Per scenario run directory:

1. `manifest.json`: scenario ID; UTC start/end; LUNA SHA + dirty flag; app bundle name/ID/port/PID/executable + codesign result; **OpenCode binary resolution record** — resolved absolute path, symlink/wrapper chain (e.g. `opencode2` → `opencode`), file SHA-256, `--version` output, LUNA's resolver source (env override / process PATH / login-shell PATH via `-l -i -c` or `-l -c` / install path / node-manager bin), the PATH string as seen by the engine process (redacted of nothing sensitive), launch context (terminal / launchd GUI / headless daemon / remote host); server **generation probe** results (`/api/health`, `/global/health`, and `/api/info` status+body) and the generation LUNA chose; OpenCode server URL/port (no credentials), server ownership (spawned vs external), XDG config/data directories in effect and whether isolated; provider/model/variant/agent requested vs effective (from server readback); LUNA permission mode and the compiled OpenCode permission rules; whether LUNA's `luna` MCP endpoint was attached (and readback of `mcp` status); account identifier sanitized.
2. `commands.jsonl`: every command/RPC with env var **names**, exit status, duration, log paths.
3. `native-frames.jsonl`: complete ordered raw SSE frames from `/api/event` (or `/global/event` for v1) with receipt timestamps and connection generation number (to prove reconnect gaps); redaction manifest. Plus `http.jsonl`: request method/path/status/body hashes for prompt, abort/interrupt, permission/form replies, fork/revert.
4. `native-readback/`: post-run `GET` session, message list (with parts), child sessions, pending permissions/forms, session status — the provider's authoritative view, captured before and after restart.
5. `normalized-events.jsonl` (LUNA `AgentEvent` stream from the host journal), `state-before.json`/`state-after.json` (`WatchDocMessages` reset frames, `WatchQueue` snapshots, task-command receipts, `GetTaskIdentity`, binding/provider session IDs), blob/attachment manifests with original-file SHA-256 and the hash of bytes the provider reports receiving (if observable).
6. `observer.json`: subscription lifecycle, snapshot timing, polling policy (persistent subscriptions; no `WatchDocMessages` polling loops), known side effects.
7. Screenshots/AX readback where UI behaviour is the claim; `SHA256SUMS`; `result.md` (expected vs actual per pass criterion, unverified claims, replay instructions); `pass.json` only when all criteria pass.

**Determinism tiers** (state which tier each claim used): **T0** fake HTTP/SSE server through the real adapter (existing tests) — transformation only; **T1** recorded real `native-frames.jsonl` replayed through the real adapter → engine → doc → RPC — recovery/ordering; **T2** real OpenCode binary with a mock OpenAI-compatible provider configured in an isolated workspace/XDG (pattern from `opencode_subagent_probe.rs`) — real server lifecycle, events, permissions, child sessions, no model cost; **T3** real binary + real/free model (authorization required; model calls are out of scope for this research) — model consumption evidence; **T4** signed QA app native UI on top of T2/T3. A T0/T1 pass never substitutes for T2+ claims.

### 5.4 Failure cases first (write before any implementation)

IDs `OCF-nn`; category code; historical precedent where one exists. Each must get an expected outcome before code is written.

**A. Binary, generation, environment (LIFE/HOST)**
- OCF-01 launchd-started LUNA resolves v1 `/opt/homebrew/bin/opencode` while a terminal-started engine resolves v2 (`00-baseline.md:43-61`); behaviour and storage differ by launch context.
- OCF-02 Login-shell probe `-l -i -c` times out/fails → `-l -c` fallback silently selects v1 before the `~/.opencode/bin` install path.
- OCF-03 `OPENCODE_EXECUTABLE` or PATH points at a wrapper (`opencode2`, `opencode1` with isolated XDG); manifest records the wrapper, not the real binary/hash/storage.
- OCF-04 Model/command discovery uses a different binary, generation, cwd or config than the run (Claude G1 analogue).
- OCF-05 v1 and v2 share XDG data; v1 lazy migration 500s on a v2-rewritten DB (`SQLiteError: no such column: project_id`, `67c960f4`), or sessions from one generation are invisible to the other.
- OCF-06 Generation probe misclassifies (2.x serves HTML on the other health path; `/api/health` vs `/api/info` drift across 2.0.x).
- OCF-07 Binary upgraded while a server is running; resume hits a new server with schema/event drift (2.0.22 → 2.0.24).
- OCF-08 Headless daemon / remote host has a different PATH/HOME → different binary than the desktop; capabilities reported from the wrong host.
- OCF-09 Update checker reports `~/.opencode/bin` while runs use Homebrew v1 (`crates/engine/src/provider_updates.rs:33,155-156`).

**B. Server lifecycle and transport (LIFE)**
- OCF-10 Request during the boot window parks forever (`0b421cc2`).
- OCF-11 First prompt before the SSE subscription is live misses start/terminal events (`a401432f`).
- OCF-12 SSE drops mid-turn; events in the gap are gone (T3: "volatile" stream, §4.1) → turn hangs or is falsely completed.
- OCF-13 Server crash/restart: lost execution never terminates; pending asks vanish; MCP registrations forgotten.
- OCF-14 Orphaned `opencode serve` after LUNA crash; port collision; two servers on one storage.
- OCF-15 OpenCode TUI or another client writes the same session concurrently (two-writer; cf. CERT13-12).
- OCF-16 LUNA quits while a turn runs; orphan keeps editing files.

**C. Attribution, duplicates, ordering (TURN/AGT/OUT)**
- OCF-20 Root goes idle while child sessions still run → root Done; later child output attributed to root or dropped.
- OCF-21 Child-session events (same types, different `sessionID`) folded into the parent transcript.
- OCF-22 Child part/tool IDs collide with parent IDs → parent output overwritten (cf. `tool-output.md:9`).
- OCF-23 Provider-initiated execution (background-subagent wake, auto-continue) settles a queued human receipt or merges into the finished turn (Claude E24 analogue).
- OCF-24 Duplicate frames (reconnect, repeated full-state part updates) duplicate text/tools.
- OCF-25 Delta arrives after final part state (or vice versa) → truncated/doubled text.
- OCF-26 Event for a session LUNA doesn't own (shared server) appears in a LUNA chat.
- OCF-27 `session.command` and `/compact` turns lack a client ID and cannot be correlated.
- OCF-28 Queue + steer in flight both claim one terminal event.

**D. Delivery, steering, queue (TURN/INP)**
- OCF-30 Prompt POST times out but was accepted → resend duplicates the user message and work.
- OCF-31 Steer acceptance labelled as incorporation; later cancellation not reflected.
- OCF-32 Steer silently drops attachments.
- OCF-33 Queue head with attachments is steered as text-only (`message-queue.md:29`).
- OCF-34 Send-now interrupt races completion; new prompt lands in the aborted execution.
- OCF-35 Client message ID treated as idempotency; retry reuses it and the server duplicates or rejects.

**E. Cancellation (TURN)**
- OCF-40 Stop aborts the root but child sessions continue.
- OCF-41 Stop with a pending permission leaves it answerable; a late answer resurrects work.
- OCF-42 Abort response lost → LUNA shows Stopped while the server continues.
- OCF-43 Interrupted execution reported as Completed.
- OCF-44 Decline sent as reject-without-message ends the whole execution (T3 note) when the user meant "skip this tool".

**F. Permissions, questions, stale approvals (PERM)**
- OCF-50 Answer after turn end/restart applies to a new execution or another session.
- OCF-51 Child-session ask after root idle is auto-declined by the parked gate or shown on the wrong chat (CERT13-07 analogue).
- OCF-52 "Always" persists a project-wide rule into the user's OpenCode state (exceeds LUNA scope; T3 `:2047-2050`).
- OCF-53 Full compiles to `*: allow` but special asks (external directory, doom-loop) still park.
- OCF-54 Concurrent asks: answering one resolves/hides the other.
- OCF-55 Legacy auto-approve path (2.0.3 era) approves for non-owned sessions.
- OCF-56 Question/form with unsupported fields waits forever (BB `1f73cd73`).
- OCF-57 Question answered twice; cancel races answer.
- OCF-58 Reconnect readback re-emits an ask → duplicate approval cards.

**G. Output fidelity and partial output (OUT)**
- OCF-60 Output above preview cap truncated before journal.
- OCF-61 Streaming bash output lost or duplicated against final output.
- OCF-62 Tool error vs empty output conflated; unavailable vs empty.
- OCF-63 Tool attachments (read-tool images, file parts) dropped.
- OCF-64 Reasoning parts lost or rendered as answer text.
- OCF-65 Partial text at abort/crash discarded or shown as complete.
- OCF-66 Unknown part/event type silently dropped instead of bounded diagnostic.
- OCF-67 Edit/patch diff metadata lost; per-file identity unstable.
- OCF-68 Todo/structured updates lost.

**H. Provider failure (MOD/TURN)**
- OCF-70 Auth/quota/rate-limit error leaves "Working" forever (`7c6c1236`).
- OCF-71 Retry status hidden or shown as terminal failure.
- OCF-72 Unavailable model/variant silently replaced by a default.
- OCF-73 Context overflow vs auto-compaction confused.
- OCF-74 5xx on session create retried blindly → duplicate sessions (vs single-retry pattern, `67c960f4`).
- OCF-75 Resume lookup failure silently starts a fresh session (`docs/qa/backend-audit-2026-09-26.md:24` regression).

**I. Images and attachments (INP)**
- OCF-80 Images sent as path text on any route (T3/BB weakness).
- OCF-81 Image-only prompt rejected or sent empty.
- OCF-82 Model lacking image modality silently ignores the image.
- OCF-83 Remote-device image path resolves on the client, not the host.
- OCF-84 Restart between staging and dispatch loses the attachment or dispatches a stale `pending://`.
- OCF-85 Retry of a failed turn drops images.
- OCF-86 Oversized/corrupt/SVG passes admission.
- OCF-87 Editing a queued row duplicates or drops the image.

**J. History, restart, reopen (SES)**
- OCF-90 Reopened LUNA doc diverges from the provider message list (missing child turns/tool output).
- OCF-91 Restart with a running turn: LUNA says interrupted while the server still runs, or vice versa.
- OCF-92 Fork mutates the source, or copies permission rules/MCP registration unexpectedly.
- OCF-93 Revert restores files contrary to LUNA's "files kept"; a staged revert is committed by the next prompt.
- OCF-94 Imported TUI session reopens with the native `plan` agent active (product decision: no Plan/Build UI).
- OCF-95 Session deleted/archived externally → binding broken silently.

**K. Models, variants, discovery (MOD)**
- OCF-100 Catalog read before models.dev sync returns empty and is cached as "no models".
- OCF-101 Variant persisted for a model that doesn't offer it.
- OCF-102 Discovery server leaks or heavy decode regresses (112 MB precedent).
- OCF-103 Connected-only filter hides models usable via env keys.
- OCF-104 Mid-session model change not reflected in effective readback.
- OCF-105 Native agents list surfaces `plan` as a LUNA mode switch.

**L. Compaction (MOD/TURN)**
- OCF-110 Auto-compaction mid-turn treated as turn end, or later output dropped.
- OCF-111 Manual `/compact` uncorrelated; LUNA tool bootstrap lost after compaction.
- OCF-112 Context ring stale after compaction.
- OCF-113 Compaction failure leaves the turn hanging.

**M. Native subagents vs LUNA child chats (AGT)**
- OCF-120 Native subagent session presented as a persistent LUNA child chat (or vice versa).
- OCF-121 `create_thread` advertised to OpenCode without verified `luna` MCP delivery.
- OCF-122 Cross-provider child from an OpenCode parent inherits OpenCode model/variant/account.
- OCF-123 LUNA-created OpenCode child gets wider permission than the parent ceiling.
- OCF-124 Background subagent completes after the parent turn; wake execution unowned.
- OCF-125 Stop on parent misses native subagents, or wrongly stops independent LUNA child chats.

**N. Remote device (WS)**
- OCF-130 Remote client shows local, not host, OpenCode capabilities/models.
- OCF-131 Remote viewer answers an approval without authority.
- OCF-132 Remote-hosted attachments resolved on the wrong device.
- OCF-133 Host restart leaves the remote view stuck in Running.

**O. QA and evidence integrity**
- OCF-140 QA run uses the user's real OpenCode config/auth/MCP/storage.
- OCF-141 Free-model run consumes quota or credentials unexpectedly.
- OCF-142 Evidence captures auth headers/keys.
- OCF-143 T0/T2 evidence labelled as real-model proof.

### 5.5 E2E scenario skeletons

Common preconditions for all: research pins refreshed; installed OpenCode version recorded; single signed QA app with all four overrides and a dedicated OpenCode QA identity (port/data dir to be assigned); OpenCode XDG config/data isolated per run (or explicit decision to use the user's, recorded); ordinary disposable Git fixture repo; evidence dir per §5.3; persistent subscriptions opened before the first send. Tier per §5.3 noted per scenario. Cross-refs point to §1 checklist rows and §5.4 failures.

**OCE-01 Root/child attribution** (T2 mock provider, then T3) — refs E13/E25/CXTS-8; OCF-20–22,26,124.
- Steps: prompt the root to spawn two native subagents (one foreground, one background), each running a marked tool (`echo CHILD-A`, `echo CHILD-B`); root prints `ROOT-DONE` before the background child finishes.
- Evidence: raw frames with `sessionID`/`parentID`; provider child-session readback; LUNA normalized events with spawn IDs; parent and child doc projections; host journal for each tool.
- Pass: every tool output appears exactly once under the correct spawn; no child text in root; root not settled until policy-defined (or settled with background child visibly running); background completion attributed to its spawn; part IDs unique across sessions.

**OCE-02 Duplicate and out-of-order events** (T1 replay of a T2 capture with injected faults) — refs CXSS-F3, E05; OCF-24,25,28,58.
- Steps: capture a real tool-using turn; replay with (a) every frame duplicated, (b) deltas reordered after final part state, (c) terminal frame before last part update, (d) permission ask re-emitted.
- Evidence: replay input + diff vs original; normalized events; doc; journal hashes.
- Pass: text/tool output byte-identical to the clean run; one tool chip per call; one approval card; terminal state unchanged.

**OCE-03 Cancellation** (T2 then T3) — refs CXE-13, E03/E11; OCF-34,40–44,125.
- Steps: (a) Stop during a 20 s marked bash command; (b) Stop while a child subagent runs; (c) Stop with a pending permission; (d) Send-now during a running turn; (e) drop the abort response (fault injection).
- Evidence: HTTP abort request/response, `session.execution.interrupted` frames, child session status readback, process list for spawned bash, LUNA status/receipts/queue.
- Pass: root and children stop (or remaining work explicitly shown); pending ask closed and unanswerable; Send-now prompt starts a new execution only after interruption; lost abort → status "stop unknown" until readback resolves; interrupted never reported Completed.

**OCE-04 Reconnects** (T2 with SSE proxy fault injection; T3 corroboration) — refs CXSS-F5, E22; OCF-12,13,58.
- Steps: break the SSE connection (a) mid-text, (b) mid-tool, (c) while a permission is pending, (d) while a background child runs; (e) restart the server process mid-turn.
- Evidence: connection-generation-tagged frames; reconnect timeline; provider readback after reconnect; LUNA events/doc.
- Pass: no duplicated or lost visible content versus provider readback; pending ask re-shown once and still answerable; a running session continues; a lost execution ends as interrupted with a visible reason; bounded retry then truthful failure.

**OCE-05 Stale approvals** (T2) — refs E06, approval-failures 4; OCF-41,50–55,57.
- Steps: (a) answer after Stop; (b) answer after app restart; (c) answer a child ask after root idle; (d) two concurrent asks answered in reverse order; (e) Full-mode run hitting an external-directory ask; (f) "allow for session" choice.
- Evidence: permission request IDs, replies, provider state readback (rules), OpenCode config/storage diff before/after.
- Pass: stale answers rejected with no provider effect; concurrent asks resolve independently; child asks routed to the right chat and not auto-declined while the child runs; no project-wide "always" rule written; Full never parks on a LUNA card.

**OCE-06 Partial output** (T2/T3) — refs CXE-03/04, E04; OCF-60–62,65.
- Steps: command printing a distinctive head, >64 KiB body, tail, with mid-run output; Stop one run mid-stream; crash the server in another.
- Evidence: streamed metadata frames, final tool part, host journal, lazy-fetch RPC result, doc preview size.
- Pass: full head/tail retrievable after reopen; live tail visible before completion without duplicate final output; aborted/crashed partial output retained and labelled partial; empty ≠ unavailable.

**OCE-07 Provider failure** (T2 mock returning 401/429/500/context-overflow; T3 invalid model) — refs E22; OCF-70–75.
- Steps: per error class, send a turn; also unreachable resume session ID and 5xx on session create.
- Evidence: provider error frames/status (incl. retry status), HTTP responses, LUNA status and visible error.
- Pass: no forever-Working; retry state visible and distinguished from failure; no silent model substitution; resume failure surfaces and sends nothing; session-create retry at most once.

**OCE-08 Images on every delivery path** (T2 asserting file parts at the HTTP boundary; T3 for model-visible content) — refs CXE-06, E08/E26, CCI-04/05; OCF-32,33,80–87.
- Paths: (1) initial turn image-only; (2) follow-up text+image; (3) queued row with image delivered at idle; (4) steer with image while busy (expected: accepted as native file part if steering supports it, else held/rejected recoverably — never text); (5) retry after a failed turn; (6) app restart between staging and dispatch; plus remote-device submission (see OCE-11) and invalid/oversized/SVG files.
- Evidence: staged file SHA-256; captured prompt request bodies showing file parts (mime, URL/data hash); provider message parts readback; model response naming a fixture property (T3 only); LUNA receipts.
- Pass: same image hash reaches the provider on every path; no path-text substitution; invalid files rejected before acceptance with draft recoverable.

**OCE-09 Restart recovery** (T2/T3, signed QA app) — refs E22, CXSS-R5; OCF-16,84,91.
- Steps: graceful app quit during (a) idle, (b) running turn, (c) pending ask, (d) queued rows; relaunch.
- Evidence: process list (orphan server?), provider readback before/after, queue snapshots, receipts, doc.
- Pass: recovered state equals provider truth; running work either continues and completes visibly or is shown interrupted per readback; queue starts paused per contract; no automatic replay of unknown deliveries.

**OCE-10 History reopen and fork/rewind** (T2/T3) — refs E15, CXM-18, same-chat-rewind; OCF-90,92–95.
- Steps: three marker turns with tools and a child; reopen chat; fork at turn 2; rewind (if supported) before turn 2 with unrelated modified files present; import a TUI-created session (incl. one using the `plan` agent).
- Evidence: provider message lists source/destination; file hashes; LUNA doc; binding IDs.
- Pass: reopened doc matches provider history incl. child attribution and tool output; fork leaves source unchanged; rewind keeps files (`files:false`) and continuation can't recall removed markers; imported plan-agent session handled safely without exposing a Plan/Build UI.

**OCE-11 Remote device** (two engines via `scripts/e2e-smoke.sh` pattern + real OpenCode on host; T2) — refs CXE-15, PAR-2; OCF-83,130–133.
- Steps: device B views/queues into a chat hosted on device A running OpenCode; send image from B; answer an approval from B (owner) and from a read-only viewer; restart A.
- Evidence: both engines' docs, host journal, prompt request on A with host paths, authority checks.
- Pass: capabilities/models shown are host A's; image bytes resolved on A; only authorized answers accepted; B converges after A restarts.

**OCE-12 v1/v2 binary resolution across environments** (E1-style runtime checks; no model) — refs E01, CXI-11, CC-00; OCF-01–09.
- Steps: start the engine from (a) interactive terminal, (b) launchd GUI app, (c) headless daemon with minimal env, (d) remote host; for each record resolver decision and probe; repeat with `-l -i -c` probe forced to fail; with `OPENCODE_EXECUTABLE` set to v1, v2, wrapper.
- Evidence: manifest resolution record (§5.3 item 1) per environment; health/info probe bodies; storage dir used.
- Pass: the selected binary and generation are deterministic, displayed to the user, and identical for discovery, runs and update checks within one environment; a v1 fallback is explicit, never silent; shared-storage hazard detected or isolated.

**OCE-13 Model and variant discovery** (T2/T3, no prompt needed for listing) — refs E02/E10, CXM-02; OCF-100–105.
- Steps: list models immediately after boot and after sync; select model+variant; switch variant mid-chat; disconnect a provider; compare LUNA picker vs provider readback.
- Evidence: catalog responses with timestamps, picker snapshot, effective model/variant from session readback.
- Pass: no cached-empty catalog; variants valid per model; effective selection read back; `plan` agent not offered as a LUNA mode; errors retryable.

**OCE-14 Compaction** (T2 with small context window mock; T3 corroboration) — refs CCM-05, E10, tool-discovery F5; OCF-110–113.
- Steps: trigger auto-compaction mid-turn; run manual `/compact`; after both, ask the agent to call `get_workspace` (only if LUNA tools are delivered).
- Evidence: compaction frames, message list, context-ring readback, MCP tool call trace.
- Pass: turn continues and completes once; compaction visible as lifecycle not as turn end; context ring updates; LUNA tool discovery survives compaction (or unavailability reported).

**OCE-15 Native subagents vs LUNA child chats** (T2/T3; LUNA tools only after delivery exists) — refs E14/E27, CCI-30/36, chat-placement 5–8; OCF-120–125.
- Steps: (a) native subagent via OpenCode's own tool; (b) LUNA `create_thread` from an OpenCode parent (expected `unavailable` until `luna` MCP delivery is verified); (c) Codex/Claude parent creates an OpenCode child with explicit model/variant; (d) Stop parent.
- Evidence: child-session readback vs LUNA lineage records; approval card content; child execution config; permission ceiling compiled rules.
- Pass: native subagent never becomes a persistent LUNA chat; OpenCode-parent creation truthfully unavailable (or works only after a verified harmless creation); OpenCode child gets only the brief, correct model/variant, ceiling ≤ parent; Stop semantics distinct.

**OCE-16 Approvals and questions fidelity** (T2) — refs E06, CCI-17/19, CCT-15; OCF-53,56,57.
- Steps: bash, edit, external-directory, webfetch asks; question tool with text, single-, multi-select; cancel; timeout.
- Evidence: ask payloads vs shared approval details shown; question panel AX readback; replies.
- Pass: context (command/diff/patterns) visible; question uses the shared panel; unsupported field types decline visibly, never hang.

**OCE-17 Steering and queue truth** (T2, then T3) — refs CXE-09–12, CXT-*, CXSS-*; OCF-28,30–35.
- Steps: queue two rows during a turn; explicit Steer (if advertised); lost prompt response; duplicate identical text twice.
- Evidence: inbox/queue frames, prompt request IDs, `WatchQueue`, receipts, provider message list.
- Pass: accepted/recorded/unknown states truthful; no duplicate user messages; no automatic replay of unknown; steering advertised only if native mid-turn delivery exists.

**OCE-18 LUNA tool delivery** (T2) — refs CC-07/E12, delegation contract; OCF-121.
- Steps: start fresh, resumed, forked and post-compaction OpenCode sessions; list attached MCP tools; call `get_workspace`; attempt a cross-project request.
- Evidence: MCP status readback, tool catalog hash, call trace, authority rejection.
- Pass: tools attached on all four starts or delivery reported `Unsupported`; bootstrap ≤800 bytes in a durable instructions channel, not user text; cross-project denied.

Scenario coverage of the required list: root/child (01), duplicates/out-of-order (02), cancellation (03), reconnects (04), stale approvals (05), partial output (06), provider failure (07), images all paths (08), restart (09), history reopen (10), remote device (11), v1/v2 environments (12), models/variants (13), compaction (14), native subagents vs child chats (15); extras 16–18.

## 6. Product decisions

| # | Decision | Date | Sources | OpenCode consequence |
| --- | --- | --- | --- | --- |
| PD-1 | **Dedicated Plan/Build UI cancelled**; no mode switches, no proposal/implement actions, no replacement `/plan` command; research/plans stay in ordinary chat (plan files allowed) | 2026-09-30 (user) | `cx/README.md:18,70,193-195`; `cx/luna-coverage-audit.md:26`; `cc/README.md:67`; `cc/implementation-plan.md:9`; `cc/capability-inventory.md:97`; `cc/HANDOVER.md:9`; `00-baseline.md:89` | Inventory OpenCode's native `plan` agent and any plan-enter/exit tools; classify any LUNA mode picker/plan workflow as `excluded-by-product-decision`; imported/unexpected plan-agent sessions must be handled safely (no hang, no approval bypass) — `cc/tool-and-event-matrix.md:26`, `cc/README.md:67` |
| PD-2 | Steering must keep native semantics; never implement steering by interrupting and restarting the turn; follow-ups default to Queue; Send-now/Stop are distinct interrupting actions | 2026-09-30 | `cx/README.md:13,134-137`; `docs/reference/message-queue.md:33-36` | Advertise mid-turn steering for OpenCode only if v2 has native in-turn delivery (e.g. inbox `delivery:"steer"`, §4.1); else steering `unavailable` and queue holds |
| PD-3 | Use installed integrations; installation/management of plugins/marketplaces stays out of the picker | 2026-09-30 | `cx/README.md:15,150,264` | Don't propose an OpenCode plugin/MCP manager; read-only catalogs only |
| PD-4 | Prioritise everyday capabilities; internal maintenance, terminal themes, unrelated experimental endpoints don't define the backlog | 2026-09-30 | `cx/README.md:17` | OpenCode TUI themes/keybinds etc. → `n/a` (CLIENT) |
| PD-5 | No worktree creation by research; automatic worktree creation stays disabled; observe native workspace transitions only | 2026-09-30 / 10-01 | `cc/README.md:69`; `cc/capability-inventory.md:69`; `cc/tool-and-event-matrix.md:27` | OpenCode worktree/workspace features: observe and revalidate authority; don't enable auto-creation |
| PD-6 | Native subagents ≠ LUNA-created persistent child chats | 2026-10-01 | `cc/current-gap-assessment.md:56`; `cc/reference-integrations.md:93`; `00-baseline.md:89` | OpenCode child sessions map to native-subagent transcripts, never to LUNA lineage |
| PD-7 | Child placement is presentation-only (no reparenting); children are created by the parent agent tool with one-use approval, users don't create children directly; cross-provider children start fresh without inheriting provider-specific model/options/account | 2026-10-03 (Iulian) | `docs/reference/chat-placement-and-child-models.md:5-24` | OpenCode as child destination needs catalog/ceiling; as parent needs verified tool delivery |
| PD-8 | Ordinary in-app coordination (create/read/message/manage) asks no separate approval card; provider work follows Ask/Auto/Full; children capped at parent access; Full never parks on LUNA cards | 2026-10-05 | `docs/design/agent-workspace-controls/README.md:50-56`; `approval-failures.md:7-8,23-28` | OpenCode mode mapping must not add cards in Auto/Full; must not persist "always" rules |
| PD-9 | Provider defaults for new chats are per-provider; an unavailable saved model blocks sending — no silent alternative | 2026-10-05 | `docs/design/agent-workspace-controls/README.md:19-23` | OpenCode model/variant defaults validated against live catalog |
| PD-10 | Tools are advertised only when the adapter actually delivers them; instructions alone don't make a tool callable | 2026-10-01 / 10-05 | `docs/pending-tasks/agent-created-child-threads.md:186,261`; `docs/design/agent-workspace-controls/README.md:9-11`; `docs/design/agent-delegation-ux/tool-discovery.md:45-46` | OpenCode currently `RuntimeDelivery::Unsupported` (E2); must stay so until verified |
| PD-11 | Reuse shared surfaces (composer, queue, output viewer, approval/form wizard, child-chat system, history, goal/review); don't duplicate | 2026-10-01 | `cc/current-gap-assessment.md:3`; `cc/HANDOVER.md:9`; `00-baseline.md:89` | See §2.12 |
| PD-12 | No orchestration resumption, no credential changes, no cache cleanup as part of parity research | 2026-10-01 | `cc/HANDOVER.md:9` | Same constraints for OpenCode (also: no credential-store reads; contrast BB §4.2) |
| PD-13 | Hosted/external services are separate lanes, certified individually; unavailable access is a blocker, not a pass | 2026-09-30 | `cc/README.md:47`; `cc/implementation-plan.md:192`; certification ledger `:7-18` | OpenCode share/web/GitHub/console rows get their own ledger rows |
| PD-14 | (Scope, not product exclusion) OpenCode was outside the harness-evolution acceptance scope; "do not propose 'add OpenCode 2' as a blank feature" | 2026-10-04/05 | `docs/design/harness-evolution/README.md:46`; `docs/pending-tasks/orchestrator-v2-and-bb-2026-10-04/luna-integration-plan.md:27` | Treat existing dual-wire adapter as the baseline; parity work is gap closure |
| PD-15 | (Historical) Token-usage display dropped in the native rewrite | pre-2026-09 | `docs/PARITY.md:96` | Superseded in practice by context ring/Usage features (`docs/context-usage.md:16` has an OpenCode row); flag as stale, not binding |

## 7. Unresolved questions

1. **Checklist gap in `docs/PARITY.md` §4:** no OpenCode harness row exists despite a shipped adapter (E1). Should the OpenCode docs add/replace a row there, or is PARITY.md frozen historical?
2. **Generation probe endpoint:** LUNA uses `/api/health` vs `/global/health`; T3 uses `/api/info` `{version,pid}` for 2.x. Which is stable across 2.0.22/2.0.24/v2-head? (B)
3. **Native mid-turn steering in 2.0.22:** does `delivery:"steer"` and `session.inbox.*` exist at v2.0.22 (T3 targets client 2.0.18)? Determines PD-2 steering advertisement. (A/B)
4. **Subagent tool name and child events:** T3's 2.0.18 adapter calls it `subagent`; v1 was `task`. Confirm for 2.0.22 and whether background subagents trigger provider-initiated parent "wake" executions. (A/B)
5. **Revert semantics:** v2 `revert.stage` has `files:false` (T3). Does LUNA's adapter use it, and is unrevert needed? Does a staged revert auto-commit on the next prompt in 2.0.22? (B/C)
6. **MCP runtime registration:** can LUNA attach its `luna` endpoint via `mcp.add` per directory without writing the user's config, and does it persist across server restarts or leak into the user's TUI sessions on the same storage? (B/C)
7. **Durable instructions channel:** is `session.instructions.entry.put` available in 2.0.22, and does it survive compaction/fork? (B)
8. **Question/form support:** what field types does the 2.0.22 question tool emit, and does the shared question panel cover them? BB disables it; T3 maps text and multi-select only. (A)
9. **OpenCode QA isolation:** which env vars isolate OpenCode config/data/auth (XDG_*, `OPENCODE_CONFIG_DIR`, others) for a QA bundle, and should `LUNA_QA_MCP_ISOLATION` cover OpenCode MCP servers? `qa-infrastructure.md` is silent. (C + coordinator)
10. **Model-call authorization for T3-tier evidence:** real or free-model runs (`opencode/big-pickle`, Zen anonymous tier) are model calls; who authorizes them, and which account/storage do they use?
11. **Background/daemon resolver:** which PATH does the headless daemon / remote host engine use for OpenCode resolution (baseline item deferred to C)?
12. **Manual child creation inconsistency:** the Claude reassessment (2026-10-01) describes a shared *manual* child operation, while the 2026-10-03 placement contract says "users do not create children directly". Which is current? (`cc/current-gap-assessment.md:56` vs `docs/reference/chat-placement-and-child-models.md:19-21`)
13. **Stale doc:** `docs/pending-tasks/agent-created-child-threads.md:139` still says Claude MCP transport is unverified, contradicted by `ClaudeMcpTools` and the 2026-10-05 acceptance. Note for doc hygiene only.
14. **Historical permission auto-approve on 2.0.3** (`67c960f4`): confirm the follow-up scoping removed blanket auto-approval for non-owned sessions at HEAD (C).
15. **Shared storage hazard:** with v1 (Homebrew) and v2 (`~/.opencode/bin`) both installed, do they share `~/.local/share/opencode`? The user's `opencode1` wrapper isolates XDG for v1 only when invoked as `opencode1`; plain `/opt/homebrew/bin/opencode` (the launchd/login-shell fallback) does not. (C)
