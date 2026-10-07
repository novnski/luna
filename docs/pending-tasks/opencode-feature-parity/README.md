# OpenCode v2 feature parity for LUNA

**Status (2026-10-08):** OC-00 to OC-15 and OC-17 are merged into `main` (the `oc/*` branches, integrated at `3275226b`), and three live E2E rounds (S1–S9, `oc/e2e-base-r3`) fixed what they found. What remains:
- **OC-16** remote-host parity: OpenCode binary resolution, capabilities and models on luna-headless hosts (OCE-11). The Go-limits part is settled by PD-17.
- **OC-18** certification: create `docs/reference/opencode-advanced-certification.md`, fill the Verification column of [capability-inventory.md](capability-inventory.md) from retained artifacts, add the OpenCode row to `docs/PARITY.md` (Q-4), and update the shared contracts' OpenCode statements.
- PD-16 file-restore button, deferred by decision.

The research below describes the pre-implementation baseline; its "today" findings (G-0 and so on) are fixed.

Research date: **2026-10-06**, finished 2026-10-07, refined for provider parity on 2026-10-07. This is research and documentation only: no implementation, tests, builds, app refreshes, model calls, commits or pushes.

**Objective, as refined (PD-14).** Make OpenCode usable in LUNA **the same way Codex and Claude Code are**. It should go through the same LUNA mechanisms (harness hooks, events, engine gates, UI surfaces) with the same behaviour wherever OpenCode has a native equivalent: everyday use, native subagents, side chats, child chats and LUNA tools. OpenCode-only features without a LUNA counterpart are recorded but **not pursued**. **[provider-parity-contract.md](provider-parity-contract.md) is the scope document.**

The original research inventoried every OpenCode v2 capability, OpenCode-only ones included. That inventory is kept as the record, and each row now says whether it is in parity scope. The work keeps three things apart:
- native OpenCode functionality
- LUNA-owned functionality
- externally gated services

"The provider executes it" is never treated as proof of integration.

## Start here

- **[provider-parity-contract.md](provider-parity-contract.md):** **read first.** Shared provider hooks and events compared across Codex, Claude and OpenCode; subagents, side chats, child chats and LUNA tools specified against what Codex and Claude do; the OpenCode-only features not pursued.
- **[HANDOVER.md](HANDOVER.md):** a self-contained prompt for the implementation agent.
- **[capability-inventory.md](capability-inventory.md):** the main coverage matrix. 178 feature rows (14 marked *not pursued* under PD-14), each rated on Execution, Data fidelity, Product connection and Verification, with LUNA route/gap, affected files, work package and E2E scenario.
- **[tool-and-event-matrix.md](tool-and-event-matrix.md):** transports, the endpoint contract with required fixes, inputs, the v2 tool vocabulary, all 94 v2 event types with today's and target handling, controls, and the lifecycle contract.
- **[luna-coverage-audit.md](luna-coverage-audit.md):** end-to-end source traces through LUNA, ranked gaps G-0..G-21, provider gates, silent drops, caps, resolver behaviour, test coverage, and corrections to earlier claims.
- **[implementation-plan.md](implementation-plan.md):** work packages OC-00..OC-18 with dependencies, modules, reuse points, acceptance and risks.
- **[validation-plan.md](validation-plan.md):** failure cases written before implementation (OCF-01..168), the evidence artifact contract, tiers T0–T4, and E2E scenarios OCE-00..24, with a comparative Claude-vs-OpenCode parity-proof rule.
- **[coverage-review.md](coverage-review.md):** coverage of every docs page, API family and event type; reconciliation with the Codex and Claude parity inventories; product decisions; the separate omissions review; and the unresolved-questions ledger.
- **[research-baseline.json](research-baseline.json):** commits, branches, dirty state, executables and hashes, docs retrieval timestamps, entry points, and what was and wasn't verified.
- **[notes/](notes/):** workstream evidence:
  - [00 baseline](notes/00-baseline.md)
  - [A native inventory](notes/A-native-inventory.md)
  - [B protocol and lifecycle](notes/B-protocol-lifecycle.md)
  - [C LUNA integration](notes/C-luna-integration.md)
  - [D reconciliation and verification](notes/D-reconciliation-verification.md)
  - [E omissions review](notes/E-omissions-review.md)

## Baseline

| Item | Value |
| --- | --- |
| LUNA | `main` @ `9b3e9a2e` for the first delivery; the parity refinement was read at `36cd741a` (adapter unchanged; other engine files moved; see the baseline JSON) |
| OpenCode **installed and released** | **v2.0.22**, `~/.opencode/bin/opencode` (sha256 `af29b0b1…`), tag commit `527f0b93`, on the upstream `v2` line |
| Later OpenCode | v2.0.23 `0fd7e282`, **v2.0.24 `e7a34f09`** (latest release, not installed), v2-head `4a0f273f` |
| Version the LUNA adapter claims | **v2.0.3** `d44b52ca`, the last release before the v2.0.4 API audit |
| Local OpenCode checkout | `dev` @ `4ac0d9c3` is the **v1 line (1.18.34)**, not v2. v2 evidence was read from immutable commits without touching the checkout. |
| v1 | Homebrew 1.18.34 (`/opt/homebrew/bin/opencode`; the `opencode1` wrapper isolates its storage) |
| Docs | 63 pages at https://opencode.ai/v2/docs/ retrieved 2026-10-06 20:31–20:32Z. The docs track the dev line (they describe 2.0.23/24/head features), and the Intro links 2.0.6 binaries. |
| Reference apps | T3 `4ee6bfd5` (two native OpenCode adapters, v2 via `@opencode/client` 2.0.18); BB `a2bbfaad` (OpenCode over ACP) |

Plain `opencode` resolves to v2 in interactive and profile shells, and to **v1** in login-non-interactive and minimal shells. GUI LUNA (launchd) has no opencode on its process PATH and relies on its login-shell probe. That probe normally finds v2 (`zsh -l -i -c`, 0.10 s) but **silently falls back to v1** if the interactive attempt fails.

## Executive findings

1. **OpenCode v2 cannot run in LUNA today (G-0, S0).** LUNA detects readiness and the wire generation only via `GET /api/health`, which every release since **v2.0.4** removed. At v2.0.22, `/api/health` returns 404 and `/global/health` returns the web UI's HTML, so each run and discovery probe waits 300 s and fails. This is E2 source evidence, independently re-checked against the OpenAPI spec of every tag. It has not been observed live (OCE-00 reproduces it first).
2. **Behind G-0 there are two more contract breakers from v2.0.4:**
   - **Approvals** send `{reply}`; v2 requires `{decision}`. The 400 is swallowed and the turn hangs (S0).
   - **Slash commands** send `{command}`; v2 requires `{name}`. The 400 is ignored and the turn stalls (S1).

   Fork (`before`), compaction (object body) and session permission rules (`permissions` with `{action, resource, effect}`) are also wrong on v2.
3. **Questions, MCP elicitation and the websearch provider picker hang invisibly on v2.** They arrive as `form.*` events without a top-level `sessionID`, and LUNA drops them (S1).
4. **LUNA's Ask/Full policy is never applied on v2,** so OpenCode's defaults govern. In LUNA's spawn mode an agent's shell commands can read the server password and could approve their own permission requests (S1, inferred).
5. **v2 renamed tools and LUNA kept v1 names.** `shell` renders as a generic card with its command stripped. `subagent` children are never bound, so **all v2 native subagent transcripts are dropped**.
6. **Tool output is cut to 4 KiB before LUNA's journal.** Diffs, live output, metadata and image content are dropped. 75 of 94 v2 event types, including retries, finish reasons, compaction, inbox delivery, revert and session-state changes, are dropped silently.
7. **Turns can resume unseen.** LUNA terminates its server mid-turn without `interrupt`, so the restart claim survives, and the user's own OpenCode background service (same database) may later resume that turn.
8. **LUNA delivers no runtime contract and no tools to OpenCode** (`RuntimeDelivery::Unsupported`). v2 has native building blocks for both: runtime `mcp.add` and session instruction entries.
9. **Subagents, side chats and child chats don't yet behave like Codex and Claude** ([provider-parity-contract.md](provider-parity-contract.md) §2–§4):
   - **Subagents:** v2 subagents are dropped entirely. OpenCode emits none of the shared child status, progress or background-task events, and lacks the per-child Stop hook.
   - **Side chats:** native-fork side chats are offered for OpenCode but **fail on v2** (G-20), and snapshot side chats put the parent context in the user's first message (G-21).
   - **Child chats:** OpenCode is **already offered as a child-chat destination** (`CHILD_PROVIDERS`) but its turns fail on v2. It can't be a parent until LUNA tools are delivered the way Claude gets them (inline config, root-only evidence fence).
10. **What's already sound and reusable:**
   - the HTTP+SSE architecture
   - per-run password auth
   - resume semantics
   - text and reasoning streaming
   - typed mapping for most file tools
   - the engine's nested-subagent docs
   - LUNA's shared composer, queue, output journal, approval and question panel, MCP form contract, rewind contract and child-chat system

   v2's native inbox (`delivery: steer|queue`) can back LUNA's existing StepBoundary steering, so OpenCode steers mid-turn like Codex and Claude. Its forms back the shared question panel. Instruction entries and `OPENCODE_CONFIG_CONTENT` back LUNA tool delivery, the way Claude uses `--mcp-config`.

**Transport and version conclusions.**
- **Keep native HTTP+SSE.** ACP (`opencode acp`) is a lossy translation that runs its own private server. `@opencode/client` and `@opencode/sdk` would need a JS sidecar and add no capability.
- **Launch with `serve --stdio --port 0`.** This removes the port race and the password leak, and gives an ownership lease.
- **Detect with `GET /api/info`.**
- **Add a boot-time compatibility contract:** an OpenAPI subset fingerprint plus a pinned event vocabulary, classifying each version as known-good, known-bad or untested.
- **Version drift:** all breakers date from v2.0.4. 2.0.22 → 2.0.24 adds only `vcs.init` and additive fields; 2.0.24 → head changes no API.

## Priorities

| Stage | Packages | Outcome |
| --- | --- | --- |
| A Gate (P0) | OC-00 baseline and binary truth; OC-01 verification infrastructure; OC-02 boot, ownership and compatibility contract; OC-03 request-contract repair | v2 runs in LUNA, deterministically, with evidence |
| B Everyday (P0/P1) | OC-04 permissions; OC-05 questions and MCP elicitation; OC-06 tools and full output; OC-07 receipts and StepBoundary steering like Codex/Claude; OC-08 reconnect and restart; OC-09 images and skills like Claude; OC-10 models, live change, usage and compaction | Everyday use behaves like Codex and Claude |
| C Agents and history (P1) | OC-11 native subagents and background tasks (Claude/Codex contract); OC-12 fork, native-fork side chats, rewind and edit through Claude's hooks; OC-13 LUNA tools and runtime contract like Claude (child-chat parent, side-chat seed) | Subagents, side chats, child chats and history behave like Codex and Claude |
| D Ongoing (P2) | OC-14 discovery parity (`commands_in`, `run_title`, skills, inventory); OC-15 Review like Claude (goals stay unavailable); OC-16 remote-host parity; OC-17 updates and compatibility | Remaining shared surfaces |
| E Certify | OC-18 OpenCode certification ledger | Verification column filled from retained artifacts |

## Product decisions respected

- The Plan/Build UI and replacement `/plan` stay cancelled. OpenCode's native `plan` agent is inventoried and any LUNA surface over it is excluded.
- Native steering only.
- No plugin or MCP manager.
- No automatic worktree creation.
- Native subagents are not LUNA child chats.
- Never persist OpenCode "always" rules for LUNA modes.
- Tools are advertised only after verified delivery.
- Reuse shared surfaces.
- **PD-14 (2026-10-07):** parity means the same shared mechanisms as Codex and Claude; OpenCode-only features are not pursued.
- **PD-15 (2026-10-07):** OpenCode v1 (1.18) support is dropped; LUNA runs OpenCode 2 only.
- **PD-16:** the OpenCode file-restore button comes later.
- **PD-17:** keep the OpenCode Go usage-limits reader as is.
- **PD-18:** real-AI testing is authorized for every scenario, including Claude comparisons.

Sources: [coverage-review.md](coverage-review.md) §5.

## What was and wasn't verified

**Verified:**
- Installed versions and shell-dependent resolution (E1).
- Release pins via read-only `git ls-remote`.
- Per-tag OpenAPI checks for health, readiness and request bodies (E2).
- The v2.0.22 Form schema, restart-claim ownership and `mcp.add` semantics (E2).
- All 63 docs pages (E3).
- `--help` for every v2.0.22 subcommand (E1).
- Source traces through LUNA (E2).

**Not verified:**
- Any live OpenCode session, prompt or model call. A live `opencode serve` metadata probe was skipped because v2 can install npm packages at boot with no flag to disable it.
- Any LUNA build, test or E2E run.
- Any GUI interaction.

Every verifiable matrix row's Verification is `unverified` (the rest are `n/a`). The only historical live 2.x evidence is a harness-only probe against 2.0.3 (LUNA commit `67c960f4`). There are 22 open questions in [coverage-review.md](coverage-review.md) §9; Q-1 and Q-28 were closed by PD-14; Q-2, Q-13, Q-15 and Q-10 by PD-15..PD-18. The most consequential:
- Q-3: does v2 ignore or reject excess request keys?
- Q-5: live confirmation of G-0.
- Q-4: whether `docs/PARITY.md` gets an OpenCode row.
- Q-21: a doc conflict about manual child-chat creation.
- Already decided: drop v1 (PD-15), file-restore later (PD-16), keep the Go-limits reader (PD-17), real-AI testing for every scenario (PD-18).

## Preservation status

This folder is **untracked** (not committed), like the sibling `agent-workspace-app-research/`. It is **not git-ignored**. It is linked from `docs/pending-tasks/README.md`. `SHA256SUMS` covers the deliverables and notes. The v2.0.22 source snapshot used for reading lives in a temporary session scratchpad; the commit `527f0b93` is the durable pin.
