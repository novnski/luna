# OpenCode v2 parity: implementation plan

Research date 2026-10-06/07. This plan turns the findings in [capability-inventory.md](capability-inventory.md), [tool-and-event-matrix.md](tool-and-event-matrix.md) and [luna-coverage-audit.md](luna-coverage-audit.md) into stable work packages, **OC-00 onward**. Each package is a unit of ownership and acceptance, not permission to work in parallel: `crates/harness/src/opencode/mod.rs` and the engine's shared session code should have one owner at a time (the CC plan used the same rule).

**Parity scope (PD-14, 2026-10-07).** The goal is for OpenCode to behave like Codex and Claude Code inside LUNA's existing shared surfaces, not to expose OpenCode-only features. Every package below implements an **existing** LUNA mechanism (a `Harness` hook, `AgentEvent`, engine gate or UI surface) with OpenCode-native backing. Where Codex and Claude differ, follow the provider-neutral hook (usually Claude's trait-method path). OpenCode-only features are listed as *not pursued* in [provider-parity-contract.md](provider-parity-contract.md) §7 and get no package work. Subagents, side chats and child chats are specified in that document's §2–§4.

**Ground rules for the implementation phase.** These come from the repository and user instructions; see [validation-plan.md](validation-plan.md) §1.
1. Refresh the pins first (OC-00). The baseline here is OpenCode v2.0.22 (`527f0b93`) and LUNA `9b3e9a2e`.
2. Write each package's failure cases **before** its code ([validation-plan.md](validation-plan.md) §3). Do not add unit tests after implementation; prove behaviour with repeatable E2E artifacts.
3. Use `scripts/cargo.sh` with `--locked`. Run `python3 scripts/build-budget.py --status` before builds. Respect the 170 GiB aggregate budget and the exit-75 coordination rule.
   - **Parallel branches:** never point two worktrees at one `CARGO_TARGET_DIR`. Cargo maps path crates from every worktree to the same unit dirs, so a check silently reuses another branch's compiled crates (observed 2026-10-07).
   - Give each branch its own subdir, e.g. `CARGO_TARGET_DIR=/Users/iulian/workspace/code/personal/luna-worktrees/opencode-build/target/<branch>` with `CARGO_INCREMENTAL=0`.
   - **The integrator builds in a fresh `.../target/oc-integration` subdir** and distrusts results from the shared root target.
4. Run one LUNA GUI app at a time. QA bundles need all four `LUNA_DEV_*` overrides plus a dedicated OpenCode QA identity (OC-01). Quit gracefully; never force-quit to free the slot.
5. After each meaningful change and its checks, refresh **LUNA Dev** with `scripts/run-macos-dev.sh --refresh`. Verify the process, port and `codesign`. Then follow the commit-and-push skill (`.codex/skills/commit-and-push/SKILL.md`) unless told to stop.
6. Preserve user data, credentials and OpenCode storage. QA runs use isolated OpenCode XDG directories. Never read or copy credential stores for new features.
7. Product decisions:
   - **PD-1:** no Plan/Build UI, mode switch or `/plan`.
   - **PD-2:** native steering only, never interrupt-restart.
   - **PD-3:** no plugin or MCP manager.
   - **PD-5:** no automatic worktree creation.
   - **PD-6:** native subagents are not LUNA child chats.
   - **PD-8:** never persist OpenCode "always" rules for LUNA modes.
   - **PD-10:** advertise tools only after verified delivery.
   - **PD-11:** reuse shared surfaces.
   - **PD-14:** parity means the same shared mechanisms as Codex and Claude; OpenCode-only features are not pursued.
   - **PD-15:** OpenCode v1 (1.18) support is dropped; LUNA runs OpenCode 2 only.
   - **PD-16:** the OpenCode file-restore button comes later; normal rewind first.
   - **PD-17:** keep the OpenCode Go usage-limits reader as is.
   - **PD-18:** real-AI testing is authorized for every scenario, including Claude comparisons.

## Sequence

| Stage | Package | Priority | Depends on | Ownership area | Rows closed (main) |
| --- | --- | --- | --- | --- | --- |
| A Gate | **OC-00** Baseline, binary and generation truth | P0 | — | harness resolver, engine provider_updates (read), docs | F-WS-12, F-SES-18, F-HOST-08, F-MOD-12 |
| A Gate | **OC-01** OpenCode verification infrastructure | P0 | OC-00 | scripts/qa, harness examples, QA docs | prerequisite for every scenario |
| A Gate | **OC-02** v2 boot, ownership and compatibility contract | P0 | OC-00, OC-01 | `opencode/mod.rs` server layer | F-LIFE-01/02/06/07/08/10, F-PERM-14, F-WS-03 |
| A Gate | **OC-03** v2 request-contract repair | P0 | OC-02 | `opencode/mod.rs` request bodies | F-PERM-03, F-INP-11, F-SES-06 (body), F-MOD-06 (body), F-LIFE-09 |
| B Everyday | **OC-04** Permission policy and approval fidelity | P0 | OC-03 | adapter permission path, engine approvals, shared approval details | F-PERM-01/02/04/05/09/12/13/15 |
| B Everyday | **OC-05** Forms: questions, MCP elicitation, provider pickers | P0 | OC-03 | adapter normalizer, engine pending-input bridge, MCP form contract | F-PERM-07/08, F-TOOL-10, F-TOOL-09 (form), F-EXT-14 |
| B Everyday | **OC-06** Tool vocabulary and full-output fidelity | P0 | OC-02 | adapter tool mapping, tool-output projection | F-TOOL-01..09, 11, 13..15, 17, 18, 20; F-OUT-01..05, F-OUT-10 |
| B Everyday | **OC-07** Turn correlation, delivery receipts, StepBoundary steering (like Codex/Claude), retries, errors | P1 | OC-03 | adapter turn loop, engine queue/steer capability | F-INP-02/03/09, F-TURN-01/02/06/08/10/12, F-MOD-13, F-HOST-01 |
| B Everyday | **OC-08** Reconnect, replay, restart and claim safety | P1 | OC-07 | adapter bus, engine recovery | F-LIFE-04/05/07, F-SES-11/12/13/15, F-TURN-09, F-PERM-13 (resync) |
| B Everyday | **OC-09** Images on every path; typed skills like Claude | P1 | OC-07 | engine `provider_inputs`, queue/steer gates, adapter prompt body | F-INP-05/06/15, F-INP-07 (skills only) |
| B Everyday | **OC-10** Models, variants, usage (live and history, like T3), cost, context, compaction | P1 | OC-03, OC-07 (history: none) | adapter catalog/usage, engine compaction RPC, usage page | F-MOD-01..03/06/07/11/15, F-OUT-07, F-TURN-05/07 |
| C Agents and history | **OC-11** Native subagents and background tasks, Claude/Codex contract | P1 | OC-06, OC-07 | adapter spawn and child binding, `stop_native_child`, engine `native_subagents` | F-AGT-03/06..09/14, F-TOOL-12, F-ADV-03 |
| C Agents and history | **OC-12** History and side chats: fork (user and assistant rows), native-fork side chats, rewind and edit via Claude's hooks, reopen | P1/P2 | OC-03, OC-08 | adapter fork/revert, `native_history_turns`, `rewind_native_conversation`, engine `history_operations` | F-SES-05/06/12/16, F-AGT-02 (safety), F-TOOL-20 |
| C Agents and history | **OC-13** LUNA runtime contract and tool delivery like Claude; child-chat parent; side-chat seed | P1 | OC-02, OC-04 | harness `RuntimeDelivery`, engine runtime contract, adapter inline config and instructions | F-EXT-12, F-INP-12, F-AGT-12/13, F-EXT-09 |
| D Ongoing | **OC-14** Discovery parity: `commands_in`, `run_title`, skills catalog, integration inventory (Claude hooks) | P2 | OC-02, OC-07 | adapter discovery hooks, composer catalogs | F-INP-11, F-EXT-01/02/03/08, F-SES-08 (titles), F-MOD-11/12, F-WS-04 |
| D Ongoing | **OC-15** Review parity (like Claude's command-line review); goals and schedules stay unavailable | P2 | OC-03 | engine review routing | F-ADV-01, F-ADV-04 (truthful unavailable) |
| D Ongoing | **OC-16** Remote-host parity (luna-headless) and the Go-limits credential decision | P2/gated | OC-00, OC-02 | luna-headless resolution, usage | F-WS-09/11, F-MOD-14, F-HOST-02 |
| D Ongoing | **OC-17** Updates and operational compatibility | P2 | OC-00, OC-02 | engine `provider_updates`, compatibility notices | F-HOST-05/08 |
| E Certify | **OC-18** OpenCode certification ledger and matrix close-out | required | all | docs/reference, QA evidence | Verification column for every row |

Lower priority does not mean dropped. Every `missing` or `partial` row **in parity scope** maps to a package. OpenCode-only rows are *not pursued* (PD-14) and are listed with reasons in [provider-parity-contract.md](provider-parity-contract.md) §7: F-SES-02/07/09/10/19, F-INP-04/08/10, F-INP-07 (agent mentions), F-TURN-03/04, F-OUT-04 (turn diff), F-OUT-06, F-PERM-04, F-AGT-05, F-WS-01/02, F-MOD-05. Rows marked `n/a`, `unavailable` or `excluded (PD)` are recorded in [coverage-review.md](coverage-review.md) with reasons.

---

## OC-00 — Baseline, binary and generation truth (P0)

**What changes.**
- Refresh the research pins: OpenCode release tags, the installed version on this machine, LUNA HEAD. Re-run the static checks behind G-0..G-7 against the then-current release, and record the results in a new `research-baseline.json` revision.
- Make the executable choice deterministic and visible:
  - (a) Resolve **once per engine process per environment** and record the path, wrapper chain, SHA-256, `--version`, the resolver source (env override, process PATH, login-shell `-l -i -c`/`-l -c`, install path, node-manager) and the launch context.
  - (b) Use the same result for discovery, runs, forks, compaction and update checks.
  - (c) Pin the generation and version per chat binding, and warn when a resumed chat would run on a different generation or major version.
  - (d) Make a v1 fallback **explicit**: when the interactive probe fails and a v1 binary is chosen while `~/.opencode/bin/opencode` (v2) exists, surface a notice instead of silently switching.
  - (e) Route `OPENCODE_EXECUTABLE` through `validate_native_override`, matching Codex and Claude, and fix the stale doc comment.
- Detect the shared v1/v2 storage hazard (both generations installed with the same XDG data dir) and show a one-time notice. Never migrate or delete data.
- **Decision (PD-15, user, 2026-10-07): drop OpenCode v1 (1.18) support.**
  - LUNA runs OpenCode 2 only.
  - The resolver keeps looking past a v1 binary until it finds a v2 one. It checks `--version` on each candidate, so a Homebrew 1.18 that comes earlier on the PATH no longer wins over `~/.opencode/bin/opencode`.
  - If only v1 is installed, LUNA shows a clear "OpenCode 2 is required (found 1.18.x)" state instead of hanging or silently running the old wire.
  - The v1 wire path, its v1-only tool names (`bash`, `task`, `todowrite`) and v1 fixtures are removed in OC-02/OC-06, once v2 boots. Imported history that contains v1 records still renders through the generic tool card.

**Why.** GUI LUNA silently picks v1 when the login-shell probe fails (F-WS-12). Re-resolving per run lets a chat switch generations on one shared DB (F-SES-04/18). There is no recorded version, so no evidence can be tied to a binary.

**Modules:** `crates/harness/src/opencode/mod.rs:118-156,264-271`; `crates/harness/src/executable.rs`; `crates/harness/src/shell_env.rs` (read-only unless the cache policy changes); `crates/engine/src/provider_updates.rs:19-183` (consume the recorded resolution); task-binding metadata (`crates/engine/src/task_identity.rs`).

**Reuse:** the existing resolver and shell-env cache; `validate_native_override`; the Codex/Claude override precedent (`codex/mod.rs:103,188-193`, `claude/mod.rs:199-204`).

**Acceptance:** OCE-00 (blocker reproduction on the unmodified build) is retained first. Capture the **live** `/openapi.json` from the installed version once as the fingerprint baseline, because the committed artifact lags the source (coverage-review §4). OCE-12 passes in four environments: terminal, launchd GUI, minimal-env daemon, and a remote host via OC-16 when available. The manifest's binary-resolution record matches the one decision used for discovery, run and update check. A forced probe failure shows an explicit fallback. Failure cases OCF-01..09.

**Risks:** users with only Homebrew v1 installed lose OpenCode until they install v2 (PD-15), so the message must say how (`curl -fsSL https://opencode.ai/install | bash`, or `opencode upgrade`). Caching per process must still pick up a binary upgrade, so invalidate on a version change at the next run (OCF-07).

## OC-01 — OpenCode verification infrastructure (P0)

**What changes.**
1. Assign a dedicated **OpenCode QA identity** in `docs/reference/qa-infrastructure.md`: name, bundle suffix, IPC port and data dir, following the assignment table rules.
2. Document and implement **OpenCode storage isolation** for QA runs: `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME` (or `OPENCODE_CONFIG_DIR` + `OPENCODE_DB`), `OPENCODE_DISABLE_PROJECT_CONFIG` where needed, and `OPENCODE_DISABLE_AUTOUPDATE`. Decide whether `LUNA_QA_MCP_ISOLATION` must also strip OpenCode MCP servers.
3. Build an **OpenCode raw-wire capture**: tee of `/api/event` frames with receipt timestamps and connection generation, plus HTTP request/response hashes. Redact secrets.
4. Build a **T2 rig**: real `opencode` v2 binary plus a mock OpenAI-compatible provider in an isolated workspace config, generalizing `crates/harness/examples/opencode_subagent_probe.rs` to the v2 wire. No model cost, deterministic tool calls, scripted failures (401/429/500/overflow).
5. Build a **T1 replay** harness that feeds recorded `native-frames.jsonl` through the real adapter → engine → doc.
6. Add a live app driver `scripts/qa-opencode-*.mjs` modelled on `scripts/qa-task-work-live.mjs`: identity assertion, manifest before each send, never replay unknown sends, `run`/`verify-restart`/`snapshot` modes.
7. Optionally evaluate `OPENCODE_SIMULATE` and `http-recorder` (F-HOST-09) as T1/T2 aids.

**Why.** No existing QA driver accepts OpenCode. No OpenCode artifact directory exists anywhere (D §3, §5.2). Every later package's acceptance depends on these assets.

**Modules:** `scripts/`, `crates/harness/examples/`, `docs/reference/qa-infrastructure.md`.

**Reuse:** the `qa-*.mjs` driver pattern; `scripts/qa-steering-status.sh` manifests; `LUNA_TOOL_OUTPUT_EVIDENCE`; `scripts/e2e-smoke.sh` for the remote case; T3's env-gated live test and HTTP replay testkit as reference only.

**Acceptance:** **OCE-00**, a dry T2 run, produces a complete artifact per [validation-plan.md](validation-plan.md) §2 against v2.0.22 (with the then-current code, expected to fail at G-0). That gives the baseline evidence that the blocker reproduces. OCF-140..143 pass: no user storage touched, no secrets in evidence, tiers labelled.

**Risks:** OpenCode may install npm packages at first boot (provider SDKs, formatters). There is no env flag to disable that (B §4), so the isolated cache must be disposable and the run must be authorized. T3-tier (real model) runs are authorized for every scenario (PD-18); follow the account rules in validation-plan §1 item 6.

## OC-02 — v2 boot, ownership and compatibility contract (P0)

**What changes.**
1. **Launch:** `opencode serve --stdio --port 0` with `OPENCODE_PASSWORD` (keep the legacy name too during the transition), stdin piped as the ownership lease, and stdout's `{"url"}` line as readiness. Drop the free-port probe. Keep SIGTERM → SIGKILL after 3 s.
2. **Generation detection:** `GET /api/info`. Any status with a JSON `version` means 2.x; 200 = ready; 503 + `retry-after` = keep waiting; 500 = failed (show its body). Only then `GET /global/health` for 1.x. Attached mode must detect, never default to v1.
3. **Compatibility contract at boot.** Fetch the authenticated `/openapi.json` and check that the operation ids and request fields LUNA depends on exist with the expected `required` sets. That subset is listed in [tool-and-event-matrix.md](tool-and-event-matrix.md) §7 and B §5:
   - `server.info`, `event.subscribe`
   - `session.create.permissions[{action, resource, effect}]`
   - `session.prompt` (`text`, `files`, `id`, `delivery`)
   - `session.permission.reply.decision`
   - `session.form.reply.answer`
   - `session.command.name`
   - `session.compact` (object), `session.fork.before`
   - `session.interrupt`, `session.switchModel`, `session.active`
   - `session.message.list` (`cursor`, `order`)
   - `model.list`, `command.list`

   Fingerprint that subset and classify the version as **known-good**, **known-bad** (2.0.0–2.0.3 with old code, and any fingerprint mismatch) or **untested** (unknown fingerprint: degraded mode with a visible notice). Pin the expected event-type set (94 at 2.0.22–head) and count first-seen unknown types at runtime with bounded diagnostics.
4. **Shutdown:** `interrupt` any active session before SIGTERM, so no restart claim survives (G-8).
5. **Hygiene:** set `OPENCODE_DISABLE_EMBEDDED_WEB_UI=1` on LUNA-spawned servers (E2 flag, A §4.3; OCF-165), and keep `OPENCODE_CLIENT=luna`.
6. **Process model:** measure one long-lived server per engine serving all directories versus the current one-per-run (B §5.7). Adopt it only if the evidence favours it. Either way, run discovery in the run's location (`x-opencode-directory`).
7. **Transport decision:** record in code docs that HTTP+SSE is kept; ACP, SDK and `run --format json` are rejected (B §5).

**Why.** G-0 (S0), the password leak G-4 (S1), the orphan-claim hazard G-8, and the silent-drift class of failures.

**Modules:** `crates/harness/src/opencode/mod.rs:200-206,252-256,462-602`, `Server::shutdown`, `probe_models`/`probe_commands`; `crates/harness/src/lib.rs:607-631` (shutdown helper, shared; coordinate).

**Reuse:** the official `Standalone` launcher semantics (`oc2:cli/src/services/standalone.ts:18-33`); the existing `crash_message` and stderr tail.

**Acceptance:**
- OCE-12 boot sections and a plain T2 turn reach `Done{Completed}` with text on v2.0.22 and on the v1.18.34 fallback.
- The compatibility record is in the manifest.
- A tampered OpenAPI (field renamed) fails loudly before the first prompt.
- Self-approval probe: an agent shell command cannot read the password (OCE-05 step g).
- No orphan server after a LUNA crash (OCE-09).
- Failure cases OCF-06, 10, 11, 14, 16, 150, 163–165.

**Risks:** `--stdio` changes the stdout handling. Make sure stdout is drained and the `{"url"}` line is parsed robustly. An untested version policy that is too strict blocks users, so prefer degraded mode with a notice over refusal unless the fingerprint proves breakage.

## OC-03 — v2 request-contract repair (P0)

**What changes.** Fix every request body that changed in 2.0.4 and check every response:
- **Permission reply:** `{decision, message?}`. Remove the swallowed error; surface failures and keep the card actionable.
- **Command:** `{name, text, files?}`. Check the status; map `CommandNotFoundError` (404) and `CommandExecutionError` (500) to visible errors; forward attachments.
- **Session create/PATCH:** `permissions` as a v2 ruleset. The content comes from OC-04; OC-03 fixes the key and shape and decides behaviour for excess keys. Answer Q-3 with one T2 POST.
- **Compact:** `{id}` object body. The lifecycle is OC-10.
- **Fork:** `{before}`. The boundary is OC-12.
- **Interrupt:** no body; inspect `interrupted`.
- **Errors:** decode the tagged error envelope `_tag`, `message`, `kind`, `field`. Raise the 300 B clip for diagnostics while keeping it bounded.

**Why.** G-1 (S0), G-5, G-7 and G-13 all come from 2.0.4 body changes.

**Modules:** `crates/harness/src/opencode/mod.rs:793-856,908-933,1161-1183,1208-1222,2145-2262,2418-2461,2986-3006`.

**Acceptance:** at T2, every call in [tool-and-event-matrix.md](tool-and-event-matrix.md) §2 returns 2xx for valid input, and wire captures show the bodies. Covers OCE-16 (approve once and reject), OCE-05 (rules body accepted, no 400 on create), OCE-20 (slash command runs), OCE-14 (compaction admitted), OCE-10 (fork boundary correct). Failure cases OCF-30, 35, 74.

**Risks:** none from v1. The v1 path is removed (PD-15), so the v2 bodies are the only ones.

## OC-04 — Permission policy and approval fidelity (P0)

**What changes.**
1. **Mode compilation.** Compile LUNA modes into v2 **session rules** using v2 action names. Pattern from T3 `OpenCode2AdapterV2.ts:466-485`:
   - **Ask:** `shell`, `edit`, `external_directory`, `webfetch` (and any other mutating actions) → `ask`; reads `allow` except `.env` → `ask`.
   - **Full:** `* allow`, with configured denies still enforced.

   Never use OpenCode `always` for a LUNA mode or "allow for session"; use a LUNA-owned session rule (PATCH) instead (PD-8, OCF-52). Read the effective rules back via `session.permissions`.
2. **Child ceiling.** Native children use their own agent rules (F-PERM-12). Either add rules for subagent agents that cap them at the parent mode, or set `subagent: ask/deny` in Ask mode. Document the choice, and verify that child asks are not wider than the parent's.
3. **Ask projection.** Project v2 `permission.asked` into the shared approval details contract:
   - action kind (`shell` → exec, `edit` → file change, `external_directory` → path, `webfetch` → network)
   - `resources[]` (scanner output)
   - `metadata` (command text, diff)
   - `source{messageID, id}`, linking the card to the tool call
   - `message`
   - child attribution, showing which subagent asked
4. **Replies.** Allow once → `once`. Deny → `reject` with a message so the model continues (OpenCode's "decline with feedback"). Cancel → `reject` without a message, which stops the execution. The timeout policy is explicit and visible, not a silent reject.
5. Consume `permission.replied` to close stale cards, and resync pending asks (`GET /api/permission/request`) after reconnect or restart (with OC-08).
6. Auto mode stays unavailable for OpenCode (no native automatic reviewer). Keep the existing copy.
7. Make the permission capability descriptor (`crates/engine/src/permissions.rs:51-60`) truthful per generation: `supports_workspace_sandbox` and "session-scoped rules" only once the v2 rules are actually applied and read back (G-19).
8. **Contain OpenCode-only state changes (PD-14).** Deny `opencode_session_move` in every LUNA ruleset, so the chat's working directory stays LUNA-owned as it is for Codex and Claude. Native titles from `opencode_session_rename` are ignored (titles come from `run_title`, OC-14).
9. **Subagent tool parity.** Deny `luna_*` for subagent agents (with OC-13), matching the Codex and Claude rule that LUNA tools are root-only.

**Why.** G-1, G-3; F-PERM-02/09/12/13/15.

**Modules:** `crates/harness/src/opencode/mod.rs:2145-2199,2858-3007`; `crates/engine/src/permissions.rs:51-66,200-205`; the shared approval details types; `crates/engine/src/sessions.rs:715-770,4832-4871`.

**Reuse:** the shared approval card, details contract, expiry and one-use cards; `docs/design/agent-workspace-controls/approval-failures.md` cases, re-stated for OpenCode.

**Acceptance:** OCE-05 and OCE-16 at T2: details visible (command, diff, resources); Deny continues and Cancel stops; stale answers rejected; no project-wide rule written (diff of OpenCode storage before and after); Full never parks on a LUNA card; child ceiling holds. Failure cases OCF-41, 44, 50–55, 57, 58.

**Risks:** OpenCode's rule evaluation is last-match-wins with layering (global → agent → session). Verify that session rules actually override the user's global config the intended way, and record conflicts the user's own config causes (e.g. a configured deny stays final).

## OC-05 — Forms: questions, MCP elicitation, provider pickers (P0)

**What changes.**
1. Route `form.created` by `data.form.sessionID`. For the MCP `"global"` sentinel owner, attribute to the chat whose turn is active on that location, else show the form at the location/engine level (Q-11).
2. Map the `Form.Field` union onto the shared question panel and MCP form contract:
   - `string` (format, length, pattern, options plus `custom`)
   - `number` and `integer` (min/max)
   - `boolean`
   - `multiselect` (option descriptions, min/max items, `custom`)
   - `external` → URL-mode elicitation, with the existing URL secrecy rules
   - conditional `when` and `required`/`hidden`
3. Reply with `POST …/form/{id}/reply {answer}`; cancel with `DELETE …/form/{id}`. Handle `409 FormAlreadySettledError` and `form.replied`/`form.cancelled` (answered elsewhere).
4. Fields the panel can't render are cancelled with a message so the model continues. Never hang (OCF-56).
5. The turn-level stall guard treats a pending form as an "awaiting input" state, not a stall. A form that is neither shown nor answered within its lifetime is cancelled visibly.
6. On v1, keep option descriptions (`UserInputDetails.option_descriptions`) and child attribution.
7. Generalize the MCP form contract's provider binding (currently "active native Codex thread and turn") to accept an OpenCode form id and session binding.

**Why.** G-2: invisible hangs for questions, MCP elicitation and the websearch provider picker.

**Modules:** adapter normalizer (`crates/harness/src/opencode/mod.rs:3009-3094,3481-3518,3632-3855`); engine pending-input bridge; `crates/proto/src/mcp_input.rs`; `docs/reference/mcp-elicitation.md` (contract update).

**Reuse:** the shared question panel and MCP form UI; limits (64 KiB, 16 fields, 2-minute lifetime); the pending map; URL secrecy. Do not build a new dialog.

**Acceptance:** OCE-16 question and elicitation sections at T2, using a mock MCP server that elicits each field kind plus URL mode. The websearch provider form shows and answers. Unsupported field kinds decline visibly. Failure cases OCF-56, 57.

**Risks:** global MCP elicitation forms aren't session-attributed upstream (`oc2:schema/src/form.ts` comment). Keep the attribution rule conservative and visible.

## OC-06 — Tool vocabulary and full-output fidelity (P0)

**What changes.**
1. **Map v2 tool names:**
   - `shell` → `Exec{command, workdir, timeout, background}`
   - `subagent` → spawn (bound in OC-11)
   - `patch` → per-file operations and partial status
   - `glob`/`grep` with `path`/`include` separate
   - `skill`, `question`, `execute`, `opencode_*` and MCP resource tools → typed cards
   - MCP `<server>_<tool>` → `ToolCall::Mcp{server, tool}`, disambiguated with `mcp.list` names
   - plugin tools keep their input
   - Keep the v1 names on the v1 path.
2. **Remove the 4,096-byte adapter cap.** Send full output to the host journal per `docs/reference/tool-output.md`; doc previews unchanged.
3. **Add an OpenCode `lunaToolOutput` projection.** It carries ordered `content[]` (text, file and image items → attachment refs with byte counts), `metadata` (exit code, diff, managed full-output file path, child `sessionID`), `resultState`, error with partial content, and empty vs unavailable.
4. **Diffs.** Edit, write and patch `files: FileDiff[]` → `ToolDiff` so LUNA's diff surfaces work.
5. **Live progress.** `session.tool.progress` and input deltas → `ToolActivity`, sharing the call owner, with no repaint of the final output.
6. **Images.** Image content items → `GeneratedImage` or attachment refs.
7. **Diagnostics.** Bounded diagnostics for unknown parts, events and tools (type and field names only).
8. **Text and reasoning.** Accept rewritten snapshots as replacements; keep part boundaries where the doc model allows.

**Why.** G-6 (`shell`), G-9; F-OUT-*, F-TOOL-*.

**Modules:** `crates/harness/src/opencode/mod.rs:3220-3367,3442-3624,3736-3796,3903-3933`; tool-output projection types (`docs/reference/tool-output.md`); `crates/doc/src/parts.rs:990-1060` only if sanitization rules need an OpenCode kind.

**Reuse:** the tool-output journal and lazy fetch (`FETCH_TOOL_BLOB`); the Claude `kind:"native"` envelope as a template; the existing diff UI.

**Acceptance:** OCE-06 at T2: a command printing head, more than 64 KiB of body and tail with mid-run output; full output retrievable after reopen; live tail before completion; edit diffs rendered; MCP tool card shows server and tool; image tool result visible; aborted partial output labelled partial. OCE-22 for MCP, Code Mode and plugin tools, including the web-result step. **OCE-02** (T1 replay with duplicated frames, late deltas, an early terminal, a re-emitted ask and an unknown type) for normalization robustness and bounded diagnostics. Failure cases OCF-60..68, 161, 162.

**Risks:** journal growth from large outputs. The journal is uncapped by design; confirm the disk impact with the tool-output contract owners. If v2 truncates natively first, record the managed file reference.

## OC-07 — Turn correlation, delivery truth, native steering, retries and errors (P1)

**What changes.**
1. **Prompt correlation.** Send a caller `id: "msg_…"` derived from the LUNA queue or user-message id, so retries are idempotent (409 on a duplicate) and history can be cut at it. Keep the response inbox id. Start the turn on `session.inbox.delivered{inboxID}` and end it on the next execution terminal for that session, as the official ACP does. Treat non-refusal prompt errors as **delivery unknown**: never auto-replay (T3 pattern).
2. **Delivery evidence.** Map `inbox.enqueued` / `inbox.delivered` / `inbox.cancelled` / `delivery.changed` onto the message-queue contract's accepted / recorded / unknown evidence (`ProviderEvidence`). Send an explicit `delivery`: `queue` for turn-boundary semantics. The inbox is **receipt evidence only**; LUNA's durable queue stays the only queue UI. No inbox list or reprioritise surface, and no interrupt-with-resume (PD-14).
3. **Native steering (PD-2), so the descriptor matches Codex and Claude (`StepBoundary`, `crates/engine/src/registry.rs:697-770`).** Once (1) and (2) prove acceptance vs incorporation at T2, change `steering_mode()` from `TurnBoundary` to `StepBoundary`. Stop clears pending steers (interrupt without `resume`), as Codex's "interrupt cancels turn, clears pending input" does (CXT-12). A steer becomes a prompt with `delivery:"steer"`, settled by inbox events; cancel via `DELETE …/inbox/{id}`. Update `reg:747-766` and the queue's primary action so it offers Steer instead of Send now. Images on steer are OC-09.
4. **Unrequested executions.** Map executions LUNA didn't request (background-subagent `session.synthetic` wake, revert-clear wake, other clients) to `ProviderTurnStarted`, opening a new segment instead of folding into a finished turn.
5. **Retries.** `session.retry.scheduled` → temporary `ProviderNotice{Retrying}` with attempt and time. Remove the dead v2 retry-abort policy, since v2 caps retries at 10. On v1, also convert retries to `ProviderNotice` instead of permanent error rows.
6. **Finish reasons and errors.** `session.step.ended.finish` `length` and `content-filter` → visible notices. Keep the error type, status and bounded body. An error after content is a terminal error, not Completed. Keep the interrupt reason.
7. **Stall guard.** Cover post-activity blocks (no progress, no pending form or approval for N minutes), with a configurable bound. An awaiting-input state is not a stall.

**Why.** G-10, G-11; PD-2; the message-queue contract.

**Modules:** `crates/harness/src/opencode/mod.rs:334-336,1825-2060,2478-2484,2599-2703,3675-3707`; `crates/engine/src/registry.rs:48-50,747-766`; `crates/engine/src/sessions.rs:4689-4810,5126-5174` (strict resume, client message id and delivery-unknown guard are Codex-only today; generalize them); `crates/ui/src/queue.rs:85-96`.

**Reuse:** message-queue capabilities (`message-queue-v1`, actions, edit lease); `ProviderEvidence`; `ProviderTurnStarted` (goal contract); `ProviderNotice`.

**Acceptance:** OCE-17 (steering and queue truth, including the held-command, parallel-tool, post-turn and pending-steer-stop fixtures; lost prompt response; duplicate text), OCE-03 (interrupt result inspected; Send now after interrupt), OCE-07 (retry visible, distinct from failure; finish reasons), OCE-01 (background wake as a provider turn). Failure cases OCF-23, 27, 28, 30–35, 70–73.

**Risks:** `session.command` and compaction take no client id (T3 note). Record the newest message id before sending them. Steering changes the user-visible queue behaviour, so gate it behind the T2 evidence. There is no v1 fallback any more (PD-15).

## OC-08 — Reconnect, replay, restart and claim safety (P1)

**What changes.**
1. Track `durable.seq` per session and the connection generation (`server.connected`); detect gaps.
2. **On reconnect:** replay from `GET /api/experimental/session/{id}/log?after=<seq>` behind a capability check (experimental), falling back to cursor-paged `…/message?order=asc`. Resync the session record (`outcome`, `time.idle`), `GET /api/permission/request` and `GET /api/form`. Re-emit nothing already shown (dedupe by native ids).
3. Add a read-idle timeout above the 15 s heartbeat.
4. **Server crash or restart mid-turn:** a lost execution ends as interrupted with a visible reason; pending asks are re-shown once; MCP registrations are re-added (OC-13).
5. **App restart:** recovered state equals provider truth. Running native work is shown as running or interrupted per readback, the queue starts paused per contract, and unknown deliveries are never replayed.
6. **Claim safety:** with OC-02, `interrupt` before teardown on every path (Stop, quit, crash recovery, stall). Document the managed-service sweep hazard. When a LUNA chat is resumed, detect that its session was resumed by another server (`execution.started` without a LUNA prompt) and attribute it as a provider turn (OC-07).

**Why.** G-8, G-12; OCF-12, 13, 16, 91.

**Modules:** `crates/harness/src/opencode/mod.rs:81-82,2062-2104,3632-3640,3935-4031`; engine boot recovery (`crates/engine/src/run_journal.rs:5-8`, `sessions.rs` recovery paths).

**Reuse:** the existing reconnect loop and `session_running`; the run-journal recovery stamps.

**Acceptance:** OCE-04 (SSE break mid-text, mid-tool, while an ask is pending, while a background child runs; server restart) and OCE-09 (graceful quit while idle, running, with a pending ask, with queued rows). Wire captures and readback must match LUNA's doc with no duplicates or losses. **OCE-02** replays the same captures with injected duplicates and reordering. Failure cases OCF-12, 13, 24, 58, 91, 157.

**Risks:** the session log endpoint is experimental (Q-7). Keep message paging as the guaranteed fallback.

## OC-09 — Inputs and attachments on every path (P1)

**What changes.**
1. **OpenCode admission policy.** Image MIME types PNG/JPEG/GIF/WebP; 20 MiB per item; PDF, audio and video not sent as attachments; per-model image capability from the catalog (OC-10). Reject before acceptance with `RejectedBeforeAcceptance` and keep the draft recoverable.
2. **Routes.** Deliver native `files[]` on every route: initial, follow-up (warm), queued at idle, steer (`delivery:"steer"` accepts files, after OC-07), retry and restart. Remove the OpenCode exclusions in `pi:38-40`, `ses:972-977,5126-5130`, `doc_host.rs:5660-5668` and `rpc:3434-3437` once the adapter delivers files on steer. Stop posting `attachments: &[]` on adapter steers.
3. **Path text.** Stop duplicating image paths into the prompt text for OpenCode, or keep only a short reference if the model needs it. Decide with OCE-08 T3 evidence.
4. **Typed inputs, like Claude.** A LUNA skill selection → `skills[{id}]` from `skill.list`; extend `provider_inputs::admit` (`pi:26-30`) so OpenCode accepts skill items the way Claude does. Plugin and app mentions stay Codex-only. OpenCode agent mentions are not pursued (PD-14).
5. **Slash routes** forward attachments (OC-03).
6. **Remote:** paths resolve on the owning host (OC-16).

**Why.** G-15; F-INP-05/06/07/15; the native-composer-inputs contract (which today says OpenCode "retains existing image handling").

**Modules:** `crates/engine/src/provider_inputs.rs:14-40`; `crates/engine/src/sessions.rs:972-977,5126-5174`; `crates/engine/src/doc_host.rs:5660-5668`; `crates/engine/src/rpc.rs:3434-3437`; `crates/ui/src/queue.rs:347-352`; `crates/harness/src/opencode/mod.rs:1872-1884,2005-2018,2323-2390`; `docs/reference/native-composer-inputs.md` (update the OpenCode statement).

**Reuse:** shared attachment staging (`pending://`), queue attachment capabilities, the `RejectedBeforeAcceptance` path.

**Acceptance:** OCE-08 on all six paths, plus invalid, oversized and SVG files. OCE-20 for typed skills. T2 wire captures show the same image hash in `files[]`; T3 evidence (authorized) shows the model describing a fixture property. Failure cases OCF-32, 33, 80–87.

**Risks:** model image capability metadata may be missing for custom providers. Fall back to allowing the image with a visible "model may not support images" notice rather than silent dropping.

## OC-10 — Models, variants, usage, cost, context, compaction (P1)

**What changes.**
1. **Catalog.** Keep capabilities (image input, reasoning, tools), cost, limits, provider display names and enabled/status. Map arbitrary variant ids into `ModelOption`, beyond the effort ladder. Discover from the run's location. Invalidate on `model.updated`, `provider.updated`, `integration.updated` and `models-dev.refreshed`.
2. **Effective selection.** Read the model, variant and agent back from `session.model.selected`/`agent.selected`/`step.started`, and show requested vs effective. Support switching model or variant mid-chat via `POST …/model`.
3. **Usage and cost.** Emit `ProviderAccounting` from `session.usage.updated.cost` and `step.ended.cost`/`tokens`. Fix context occupancy: use per-step input tokens or `GET …/context` with the model `limit` as the window, instead of cumulative session totals. Stop synthesizing the `"usage"` message id (also fixes OC-12's checkpoint).
4. **Compaction:**
   - Send `{id}` and keep the inbox id.
   - Map `session.compaction.started`/`ended`/`failed` to `ContextCompaction` and refresh the context ring.
   - Report completion only on `compaction.ended`, never on admission.
   - Run compaction on the chat's live server when one exists, or keep the boot server alive until `ended`/`failed`; use `experimental.session.wait` if useful.
   - Remove the 30 s limit that includes cold boot, or measure it separately.
   - Surface auto-compaction mid-turn as lifecycle, not turn end.
5. **OpenCode usage history on the Usage page, the way T3 does it (F-MOD-15).** Codex and Claude already have history there.
   - Replace the "unsupported" OpenCode source (`crates/engine/src/usage.rs:421-424,1625-1645`) with a read-only scan of OpenCode's local databases (`opencode.db` plus channel DBs, from `OPENCODE_DATA_DIR` or `$XDG_DATA_HOME/opencode`), following `apps/server/src/usage/opencodeUsageReader.ts` in T3 @`4ee6bfd5`.
   - Read `session_message` rows with `type='assistant'` (v2), the v1 `message` table, and legacy JSON. Use `busy_timeout` 100 ms so a busy OpenCode never stalls LUNA.
   - Dedupe by message id, count reasoning in output, and estimate zero-cost subscription rows from the shared price table.
   - Read usage fields only; never store message content.
   - Independent of the v2 chat fixes, so it can ship early. Correct the outdated source note at `usage.rs:1636`.

**Why.** G-13, G-14; F-MOD-*, F-OUT-07.

**Modules:** `crates/harness/src/opencode/mod.rs:161-198,283-303,406-417,935-952,1161-1183,1302-1450,2264-2294,2723-2779,3795-3817`; `crates/engine/src/rpc.rs:4792-4900`; `crates/engine/src/live_usage.rs`; `crates/engine/src/usage.rs`.

**Reuse:** the context ring (`docs/context-usage.md` OpenCode row), the compaction control surface, the picker attribution and retry.

**Acceptance:** OCE-13 (catalog after boot and after sync; variant valid; effective readback; cost row) and OCE-14 (auto and manual compaction visible as lifecycle; ring updates; tool bootstrap survives if OC-13 is done). Failure cases OCF-100–105 and 110–113.

**Risks:** the v2 compaction runs at the next step boundary. With no active turn it may need a wake (`resume`), so verify at T2.

## OC-11 — Native subagents and background tasks, Claude/Codex contract (P1)

Full specification: [provider-parity-contract.md](provider-parity-contract.md) §2. The goal is that an OpenCode subagent looks and behaves like a Claude or Codex subagent: same agent card, same nested transcript doc, same status and progress fields, same per-child Stop, same background rows. There is no OpenCode-specific UI.

**What changes.**
1. **Spawn decode.** v2 `subagent` (v1 `task`) → `ToolCall::Unknown{name: "Agent: <description>"}` (the shared `is_subagent_spawn` gate, `crates/proto/src/agent.rs:703-719`), with the call id as chip id.
2. **Child binding.** Use the tool's `metadata.sessionID` when present, else `session.created{parentID}`. Bind at **any depth** through the engine's nested-spawn index (grandchildren are dropped today). Prefix child tool ids so they can't collide with the parent's.
3. **Child traffic.** Wrap all child-session events in `AgentEvent::Subagent{parent_tool_use_id}`. Emit `ChildStatus` from the child's execution events (Codex pattern) and `ChildProgress{model, total_tokens, tool_uses, last_tool}` from child step, usage and tool events (Claude pattern). Agent cards already render the raw OpenCode model id (`crates/ui/src/transcript/agent_cards.rs:75-100`).
4. **Per-child Stop.** Implement `Harness::stop_native_child` (Claude parity, `crates/harness/src/claude/native_stop.rs`) as `POST /api/session/{child}/interrupt`. Settle on the child's `execution.interrupted`, never on the acknowledgement. The engine path (`crates/engine/src/sessions_work.rs:70-92`) is unchanged. Root Stop interrupts the root; foreground children stop natively, and background children are stopped through the same hook. Show remaining work honestly instead of force-settling children locally.
5. **Background subagents.** The spawn chip stays running. Completion arrives as `session.synthetic` on the parent and starts a parent execution, which is emitted as `ProviderTurnStarted` (a wake), so `deterministic_turn_end` (`crates/harness/src/lib.rs:245-250`) stays honest.
6. **Background shells.** Shell jobs (`shell.created/exited/deleted`, `background:true`) → `NativeTaskChanged{kind: Shell}`, like Claude's `local_bash` (`crates/harness/src/claude/coordination.rs:69`).
7. **Asks and forms from children** route to the root chat's shared approval and question UI with child attribution, as Claude's do (with OC-04 and OC-05).
8. **LUNA tools from children are refused** (OC-13 evidence fence plus the OC-04 step-9 rule), matching Codex (`crates/harness/src/codex/luna_mcp.rs:458-461`) and Claude (`crates/harness/src/claude/luna_tools.rs:1-30`).
9. Keep native subagents strictly separate from LUNA child chats (PD-6).

**Not pursued (PD-14):** `session.background`, continuing a child by `sessionID` from the UI, OpenCode subagent model pickers.

**Why.** G-6; F-AGT-*, F-ADV-03; OCF-20..22, 40, 120, 124, 125, 160, 167.

**Modules:** `crates/harness/src/opencode/mod.rs:2523-2554,2701-2837,3128-3174,3313-3440`; a new `stop_native_child` impl; `crates/engine/src/sessions/native_subagents.rs` (reuse); `crates/engine/src/sessions.rs` subagent routing (reuse).

**Reuse:** per-subagent docs `{chat}--sub--{id}`, the nested-spawn index, agent cards, the native-task rows, the engine child-stop path, the Claude and Codex event shapes.

**Acceptance:** OCE-01 (two children, foreground and background, plus a grandchild; **run the same script with Claude and compare the normalized LUNA artifacts**: same event kinds, doc layout and card fields), OCE-21 (background subagent and background shell rows; completion attributed; wake is a provider turn), OCE-03 (b) (per-child Stop), OCE-15 (a) (a native subagent never becomes a LUNA chat), OCE-18 child step (LUNA tool refused). Failure cases OCF-20–22, 40, 120, 124, 125, 160, 167.

**Risks:** the default depth is 1 (`experimental.subagent_depth`), so deeper tests need config in the isolated QA workspace. Background-subagent completion always starting a parent execution (Q-18) must be confirmed at T2.

## OC-12 — History and side chats: fork, native-fork side chats, rewind and edit via Claude's hooks (P1/P2)

Full side-chat specification: [provider-parity-contract.md](provider-parity-contract.md) §3.

**What changes.**
1. **Fork, like Codex and Claude.**
   - `fork_session_staged` uses `{before: <first message after the kept prefix>}` (exclusive; `oc2:core/src/session/projector.ts:136-138`) and a real `msg_` checkpoint (stop persisting the synthetic `"usage"` id).
   - Read history back with cursor paging (`order=asc`) and v2 item shapes. Adopt or delete an orphan fork on post-fork failure.
   - Offer fork from **user and assistant** rows, as for Codex and Claude (`crates/ui/src/transcript.rs:7697-7706`; engine refusal `crates/engine/src/history_operations.rs:165-170`).
2. **Native-fork side chats.** These work through the same fixed fork path. The chooser already offers them for OpenCode (`crates/ui/src/shell/workbench/chooser.rs:390`), so this fixes a live failure. Read-only parents stay refused, as for Claude (`crates/engine/src/side_chats.rs:426-433`).
3. **Edit earlier message and same-chat rewind, like Claude.** Implement `native_history_turns` (from the cursor-paged message list) and `rewind_native_conversation` (`revert.stage {messageID, files:false}` then `revert.commit`; `DELETE …/revert` on failure; ignore the revert-clear wake). Then widen the UI gates that today list only Codex and Claude (`crates/ui/src/transcript.rs:7733,7756`) to key on the capability. Reuse the same-chat rewind contract and its exact-prefix readback; never replay a revert.
4. **Native file-checkpoint rewind: deferred (PD-16).** Later, like Claude's `rewind_native_files`, backed by snapshot revert with `files:true` behind the existing previewed file-rewind surface. Normal rewind ships first.
5. **Reopen:** diagnostics for divergence between the reopened doc and native messages. No OpenCode-specific backfill.
6. **External delete:** a deleted native session (`session.deleted`) shows a recoverable broken binding.
7. Imported or resumed sessions with the native `plan` agent active show it read-only and stay safe (PD-1).

**Not pursued (PD-14):** native session import (Codex-only, Claude lacks it), session export/import JSON, session stats.

**Why.** G-7, G-18; F-SES-05/06/12/16; the side-chat failure in [provider-parity-contract.md](provider-parity-contract.md) §8 item 2.

**Modules:** `crates/harness/src/opencode/mod.rs:351-353,778-856,976-984,1128-1159,2737-2739`; new `native_history_turns` and `rewind_native_conversation` impls; `crates/engine/src/history_operations.rs:21-36,165-170`; `crates/ui/src/transcript.rs:7697-7760`.

**Reuse:** same-chat rewind contract and `scripts/qa-same-chat-rewind.mjs`; persistent-fork provenance; the receipt-backed native fork path; the side-chat creation flow.

**Acceptance:** OCE-10 (three marker turns with tools and a child; reopen; fork at turn 2 from a user row and from an assistant row; rewind keeps files; plan-agent session safe), **OCE-24** (side chats from an OpenCode parent, compared with a Claude parent), OCE-19 (external delete). Failure cases OCF-90, 92–95, 155.

**Risks:** "a staged revert is committed by the next prompt" (T3, E4), so rewind must commit or clear before any queued delivery.

## OC-13 — LUNA runtime contract and tool delivery, like Claude (P1)

Full specification: [provider-parity-contract.md](provider-parity-contract.md) §4–§5.

**What changes.**
1. **Attach the shared `luna` endpoint the way Claude does.** Use the same per-process loopback `luna` MCP endpoint (`LunaMcpEndpoint`) and one inline config entry, through `OPENCODE_CONFIG_CONTENT` on the spawned server: `{"mcp":{"servers":{"luna":{"type":"remote","url":<endpoint>,"headers":{"Authorization":"Bearer {env:LUNA_MCP_BEARER_TOKEN}"}}}}}`. It is the highest-priority config layer, merged over the user's config, written nowhere (`oc2:core/src/config.ts:206-237`). The token goes in the server environment. `PUT /api/experimental/mcp/{server}` is a fallback only.
2. **Tool allowlist:** session rules allow exactly the attached `luna_<tool>` tools; deny `luna_*` for subagent agents.
3. **Evidence fence, like Claude's.** Accept a call only when `_meta["ai.opencode/sessionID"]` equals the **root** session and a matching root `session.tool.called{name: "luna_<tool>"}` was observed in the current turn. Park briefly for races. Mint turn ids and emit `ProviderTurnStarted` for the engine claim fence. The shared `luna_mcp::dispatch` rules still apply.
4. **Runtime contract and side-chat seed.** Use the stable instruction channel `PUT /api/experimental/session/{id}/instructions/entries/luna {value}`. The bounded bootstrap and the side-chat `contextSnapshot` seed move out of the first user message (today's legacy path, `crates/engine/src/side_chats.rs:772-801`), matching Codex and Claude. Verify it survives compaction and fork (Q-19).
5. **Enablement.** Add `RuntimeDelivery::OpenCodeMcpTools` (`crates/proto/src/runtime_contract.rs`), returned only after catalog readback and one harmless call (`get_workspace`) succeed on fresh, resumed, forked and post-compaction sessions (PD-10). Then OpenCode parents get `create_thread`, `read_thread`, `send_message_to_thread`, `wait_threads`, `get_workspace` and `manage_workspace` exactly as Claude does.
6. **OpenCode as a child destination** already exists (`crates/engine/src/cross_task_tools.rs:33-37`). Make it work by fixing boot, contract and rules (OC-02..04). Serve `list_child_models` from the child's execution scope (run location, not the home-dir probe).
7. Optional probe `crates/harness/examples/opencode_luna_mcp_probe.rs`, mirroring the Codex and Claude probes.

**Why.** G-16; PD-10; the delegation and workspace-control contracts; side-chat seed parity.

**Modules:** `crates/harness/src/lib.rs:232-234` (default), adapter spawn env and session setup; `crates/proto/src/runtime_contract.rs:155-167`; `crates/engine/src/runtime_contract.rs:212-216,299`; `crates/engine/src/sessions.rs` (provider claim fence and contract compile, Codex/Claude-only today); `crates/engine/src/side_chats.rs:772-801`; `crates/engine/src/child_threads.rs`.

**Reuse:** `LunaMcpEndpoint` and `luna_mcp::dispatch`, the bootstrap and topic help, the child-model catalog tool, the Claude attachment and evidence design, the Codex and Claude MCP probes.

**Acceptance:**
- OCE-18: tools attached on four start kinds or `Unsupported` reported; bootstrap in the instructions channel; cross-project call denied; **a native child's `luna` call refused**; nothing written to the user's OpenCode config.
- OCE-15 (b, c): OpenCode as parent and as destination, with Claude as the comparison.
- OCE-24: snapshot side-chat seed via instructions.
- OCE-14: tools survive compaction.
- Failure cases OCF-111, 121–123.

**Risks:** instruction entries are experimental, so gate them on the OC-02 fingerprint and keep `Unsupported` as the fallback. Agent tool processes inherit the server env containing the bearer token; the evidence fence, not secrecy, prevents direct calls (the same posture as Claude on non-Unix).

## OC-14 — Discovery parity: commands, titles, skills catalog, integration inventory (P2)

**What changes.** Implement the Claude-path discovery hooks:
1. **`commands_in(scope)`:** project-scoped slash commands from the run location, refreshed on `command.updated`, with MCP prompts as `/<server>:<prompt>`. This replaces the process-wide home-dir cache (G-5 discovery part).
2. **`run_title`:** a tool-less one-shot title run, as Codex and Claude do. Native OpenCode titles are ignored.
3. **Skills catalog:** `GET /api/skill` → composer skill chips (`SlashCommandKind::Skill`), so OpenCode skills appear like Claude's. Refresh on `skill.updated`.
4. **Integration inventory, like Claude:** read-only `native_extensions` / `session_mcp_observation` from `GET /api/mcp` and `/api/skill`, shown in the existing integration inventory surface (`crates/engine/src/integration_settings.rs:668-681`). No management (PD-3).
5. **Effective configuration:** the effective model and permissions shown in the existing requested-vs-effective record. The effective agent is shown read-only only.

**Not pursued (PD-14):**
- agent picker, agent switching, agent mentions
- reference directories
- observing session move or rename (move is denied in OC-04; titles come from `run_title`)
- worktree management (PD-5)

**Why.** G-5 (discovery), F-INP-11, F-EXT-01/02/03/08, F-SES-08, F-MOD-11/12, F-WS-04.

**Modules:** `crates/harness/src/opencode/mod.rs:220,283-319,399-404,1353-1376`; new hook impls; composer catalogs; `crates/engine/src/integration_settings.rs`.

**Acceptance:** OCE-20 (project commands and skills discovered and executed; refresh without restart), OCE-22 inventory section, OCE-19 (`opencode_session_move` denied; external delete). Compare against a Claude chat in the same fixture repo.

**Risks:** OpenCode discovery walks to the filesystem root (Q-17), so QA fixtures must avoid parent configs.

## OC-15 — Review parity; goals and schedules stay unavailable (P2)

**What changes.**
1. **Review, like Claude.** LUNA's Review action on an OpenCode chat becomes OpenCode's built-in `/review` command line for the chosen target (uncommitted, commit, branch, PR). This mirrors how Claude's review becomes its `/code-review` line (`crates/engine/src/sessions.rs:4280-4297`). It runs through the fixed command route (OC-03). Findings are text, labelled the same way as Claude's text review. No duplicate review UI.
2. **Goals:** stay unavailable. LUNA goals require a native goal engine (Codex `thread/goal`, Claude native `/goal` via `native_goal_control`), and OpenCode has none. The existing gate copy stays truthful.
3. **Schedules and monitors:** unavailable (no native counterpart).

**Not pursued (PD-14):** `/btw` side questions, `session.background`, user shell `!cmd`, the per-turn diff endpoint.

**Why.** F-ADV-01, F-ADV-04.

**Acceptance:** OCE-23 (Review routed to `/review` and labelled; goals and schedules reported unavailable).

## OC-16 — Remote-host parity and the Go-limits credential decision (P2 / gated)

**What changes.**
1. **luna-headless hosts:** record each host's OpenCode binary resolution and compatibility (OC-00/02). Capabilities and models come from the host; attachments resolve on the host. This is the same remote model Codex and Claude use.
2. **OpenCode Go limits: keep as is (PD-17).** LUNA keeps reading the saved key (`crates/engine/src/usage_opencode.rs:30-50`). Don't copy the pattern elsewhere; switch only if OpenCode adds an official route.
3. **Hosted Console, Zen and Go:** separate certification lanes. Unavailable access is a blocker, not a pass.

**Not pursued (PD-14):** attaching to external OpenCode servers or the user's managed service; OpenCode multi-account management (no single provider login maps to LUNA's account surface).

**Acceptance:** OCE-11 (remote device). Failure cases OCF-08, 83, 130–133.

## OC-17 — Updates and operational compatibility (P2)

**What changes.**
1. Make `provider_updates` generation-aware:
   - `~/.opencode/bin` → `opencode upgrade` against the v2 channel (`opencode.ai/update/api/<channel>/…`), or the correct npm dist-tag (Q-9).
   - Homebrew formulas `opencode`, `opencode-v2`, `opencode-beta` recognized correctly.
   - Never offer a v1 upgrade for the binary LUNA doesn't run.
2. Show the compatibility class (known-good, known-bad, untested) from OC-02 in the harness settings.
3. On a version change detected between runs, re-run the compatibility check before the next prompt.
4. Keep docs current: refresh the adapter header ("verified against …") on every certification.

**Acceptance:** OCE-12 update section (the offer matches the binary in use). Failure cases OCF-07, 09.

## OC-18 — Certification ledger and matrix close-out (required)

**What changes.**
1. Create `docs/reference/opencode-advanced-certification.md`, modelled on `docs/reference/claude-code-advanced-certification.md`. One ledger row per in-scope matrix row, with status `Certified`, `Supported · live QA pending`, `Unsupported in LUNA`, `Externally gated` or `Native OpenCode only`, plus evidence paths and hashes. OpenCode-only rows not pursued under PD-14 are listed once as `Native OpenCode only`.
2. Update [capability-inventory.md](capability-inventory.md)'s Verification column from retained artifacts only.
3. Add an OpenCode row to `docs/PARITY.md` §4 (Q-4).
4. Update the shared contracts' OpenCode statements: native-composer-inputs, message-queue, tool-output, mcp-elicitation, qa-infrastructure, chat-placement.

**Acceptance:** every scenario has a real-model (T3) pass (PD-18). Every matrix row is certified, or explicitly unsupported or gated with evidence; all OCE scenarios have retained artifacts with `SHA256SUMS`; T0/T1 results are never labelled as real-provider proof.

---

## Cross-cutting risks

| Risk | Mitigation |
| --- | --- |
| Upstream v2 keeps changing (19 patch releases between 2.0.3 and 2.0.22; experimental endpoints) | OC-02 fingerprint and degraded mode; pin experimental endpoints behind capability checks |
| Shared OpenCode DB with the user's own TUI and background service | OC-02 interrupt-before-teardown; OC-01 isolation for QA; notices, no automatic data changes |
| Users with only v1 installed lose OpenCode (PD-15) | A clear "OpenCode 2 required" state with install or upgrade instructions; OCE-12 covers it |
| Concurrent edits by other sessions in `crates/engine` and `crates/harness/src/lib.rs` (observed during research) | One owner per shared file; coordinate through the build guard; re-read before editing |
| Real-model test cost and quota | Authorized for every scenario (PD-18); prefer OpenCode free models; report usage after each batch |
