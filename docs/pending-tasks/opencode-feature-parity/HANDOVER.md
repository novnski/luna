# Handover: implement OpenCode v2 parity in LUNA

Use this as a self-contained starting prompt for the implementation agent.

---

You are bringing OpenCode v2 to parity with Codex and Claude Code in LUNA. The repository is `/Users/iulian/workspace/code/personal/luna`.

**What parity means (PD-14, user decision 2026-10-07).** An OpenCode chat must work through the **same LUNA mechanisms** Codex and Claude use: the same `Harness` hooks, `AgentEvent` variants, engine gates and UI surfaces, with the same behaviour wherever OpenCode has a native equivalent. Native subagents, side chats and child chats must behave as they do for the other providers. Use OpenCode-native APIs only to *back* an existing LUNA surface. Do **not** build OpenCode-only features. Where Codex and Claude differ, follow the provider-neutral hook (usually Claude's trait-method path). Follow `AGENTS.md` (repository, workspace and personal), the commit-and-push skill (`.codex/skills/commit-and-push/SKILL.md`), and the rules below.

## Read first, in order

1. `docs/pending-tasks/opencode-feature-parity/provider-parity-contract.md`: **the scope.** Shared hooks and events compared across Codex, Claude and OpenCode; subagents (§2), side chats (§3), child chats (§4), LUNA tools (§5), everything else (§6), not pursued (§7).
2. `README.md`: baseline and findings.
3. `luna-coverage-audit.md` §2: ranked gaps G-0..G-21.
4. `implementation-plan.md`: packages OC-00..OC-18, their order, dependencies and acceptance.
5. `validation-plan.md`: failure cases first (§3), artifact contract (§2), scenarios (§4).
6. `tool-and-event-matrix.md`: exact endpoint and body fixes (§2), event targets (§5), lifecycle (§7).
7. Use `capability-inventory.md` as the row-level checklist (rows marked *not pursued* get no work), and keep its Verification column honest.

The workstream notes in `notes/` hold the full citations. Research pins:
- OpenCode **v2.0.22** = commit `527f0b931d1f9b3ebd34e106c51b31ce5db5b075`, upstream `anomalyco/opencode`, `v2` line.
- LUNA `9b3e9a2e` (first delivery) and `36cd741a` (parity refinement).

## Current state in one paragraph

LUNA's OpenCode adapter (`crates/harness/src/opencode/mod.rs`) speaks native HTTP+SSE, with a 1.x wire and a 2.x wire "verified against 2.0.3".
- **Every release from v2.0.4 onward breaks it.** Readiness probes the removed `GET /api/health`, so every v2 run and discovery probe times out after 300 s (G-0). This is E2 source evidence; not yet observed live.
- **Behind that:**
  - Permission replies send `{reply}` instead of `{decision}`, causing a 400 and a hang.
  - Slash commands send `{command}` instead of `{name}`.
  - Fork sends `messageID` instead of `before`.
  - Compaction posts a `null` body.
  - Session rules go under `permission` (v1 shape) instead of `permissions[{action, resource, effect}]`.
  - `form.*` events (questions, MCP elicitation) are dropped, so turns hang.
  - v2 tool names `shell` and `subagent` are unmapped, so child transcripts are dropped.
  - Tool output is capped at 4 KiB before the journal.
  - 75 of 94 event types are dropped.
  - The server password is visible to agent tools in `default` serve mode.
  - SIGTERM without `interrupt` leaves restart claims that the user's managed OpenCode service may resume.
  - LUNA delivers no runtime contract or tools (`RuntimeDelivery::Unsupported`).
- **Against the Codex/Claude contract:**
  - OpenCode lacks Claude's shared hooks (`can_reconfigure_live`, `native_history_turns`/`rewind_native_conversation`, `commands_in`, `models_live`, `native_extensions`, `stop_native_child`, `run_title`).
  - It emits none of `ChildStatus`/`ChildProgress`/`NativeTaskChanged`/`ProviderTurnStarted`/`ToolActivity`/`ProviderAccounting`/`ContextCompaction`/`ProviderNotice`.
  - It steers `TurnBoundary` while Codex and Claude are `StepBoundary`.
  - Native-fork side chats are offered but fail on v2.
  - OpenCode is already a child-chat destination but can't be a parent.
- A GUI-launched LUNA normally resolves v2 (`~/.opencode/bin/opencode`) via its `zsh -l -i -c` probe, but silently falls back to Homebrew v1.18.34 if that probe fails. Both generations share `~/.local/share/opencode`.

## Decisions already made (do not reopen)

- **Transport:** keep native HTTP+SSE. Launch `opencode serve --stdio --port 0` with `OPENCODE_PASSWORD` and stdin as the ownership lease. Detect readiness and generation with `GET /api/info`. Add a boot-time compatibility contract: OpenAPI subset fingerprint, pinned event vocabulary, known-good/known-bad/untested classes. No ACP, SDK sidecar or `run --format json`.
- **Product:**
  - No Plan/Build UI, mode switch or replacement `/plan`. OpenCode's `plan` agent is shown read-only at most.
  - Steering must be native (v2 `delivery:"steer"` with inbox evidence), never interrupt-restart.
  - No plugin or MCP manager.
  - No automatic worktree creation. Deny `opencode_session_move` so the chat's workspace stays LUNA-owned.
  - LUNA tools are root-only: refuse calls from native subagents, as Codex and Claude do.
  - Attach LUNA's shared `luna` MCP endpoint through `OPENCODE_CONFIG_CONTENT` (inline, nothing on disk), like Claude's `--mcp-config`. Carry the runtime contract and side-chat seed in session instruction entries.
  - Review routes to OpenCode's `/review` command line, as Claude's goes to `/code-review`. Goals stay unavailable (no native goal engine).
  - Not pursued: everything in `provider-parity-contract.md` §7 (`/btw`, background control, `!cmd`, references, agent picker or mentions, export/import, stats, saved approvals, external server attach, account management, inbox UI).
  - Native subagents are not LUNA child chats.
  - Never write OpenCode "always" rules for LUNA modes.
  - Advertise LUNA tools only after verified delivery.
  - Reuse the shared composer, queue, tool-output journal and viewer, approval and question panel, MCP form contract, rewind contract and child-chat system. Don't build duplicates.
- **Auto** permission mode stays unavailable for OpenCode (no native automatic reviewer).
- **Real-AI testing is authorized for every scenario (PD-18),** including the Claude comparison runs. Every scenario needs a real-model pass. Use OpenCode's free models in the isolated QA profile; if a paid OpenCode model is needed, ask the user to sign in to that profile (never copy keys). Claude runs use the user's normal login. Report usage after each batch. Keep fake-AI runs for forced-error tests.
- **File-restore button: later (PD-16).** Ship normal rewind first.
- **Keep the Go usage-limits reader as is (PD-17).**
- **OpenCode v1 (1.18) support is dropped (PD-15).** Run OpenCode 2 only. The resolver skips v1 binaries; an only-v1 install shows "OpenCode 2 required" with install instructions. Remove the v1 wire and v1 tool names once v2 boots.

## Ask the user before these (open decisions)

- Q-4: an OpenCode row in `docs/PARITY.md`.
- Q-21: manual child-creation doc conflict.

The full ledger is in `coverage-review.md` §9. Q-1 (agent picker) and Q-28 (subagent LUNA tools) are closed by PD-14.

## How to work

1. **Start with OC-00, then OC-01.** Refresh the pins (newer OpenCode releases may exist; re-run the OpenAPI checks in `tool-and-event-matrix.md` §2 against them). Build the verification infrastructure: OpenCode QA identity, isolated OpenCode XDG storage, raw SSE/HTTP capture, a T2 rig (real binary plus a mock OpenAI-compatible provider), and a live driver `scripts/qa-opencode-*.mjs`. Then run **OCE-00** on the unmodified build to reproduce or refute G-0, and keep the artifact.
2. Then OC-02 → OC-03 (P0 gate). After each package, run its OCE scenarios at T2 and keep artifacts per `validation-plan.md` §2. Then the P0/P1 packages in the order of `implementation-plan.md`. One owner per shared file (`opencode/mod.rs`, engine `sessions.rs`/`rpc.rs`, harness `lib.rs`). Other sessions edit engine files concurrently, so re-read before editing and re-resolve the cited line numbers (they are at `9b3e9a2e`).
3. **Prove parity, not just function.** For shared-surface scenarios, run the same steps against Claude (or Codex) and OpenCode, and diff the normalized LUNA artifacts (`validation-plan.md` §1 item 8).
4. **For each package:** write its failure-case expectations first (`validation-plan.md` §3), then code, then E2E evidence. **Do not write unit tests after the code;** prove behaviour with repeatable E2E artifacts. Existing fixture tests may need updating where you change contracts (v2 fixtures currently assume 2.0.3 shapes).
5. **Builds:** `scripts/cargo.sh check --locked -p luna-harness` for fast feedback; run `python3 scripts/build-budget.py --status` before builds; `CARGO_INCREMENTAL=0` for one-off validation; respect exit 75.
6. **App:** after each meaningful change, run `scripts/run-macos-dev.sh --refresh` yourself. Confirm the bundle process, the IPC port and `codesign --verify --deep --strict 'target/luna-macos-dev/LUNA Dev.app'`. One GUI app at a time. QA bundles need all four `LUNA_DEV_*` overrides. Quit apps gracefully; never force-quit.
7. **Data:** never touch the user's OpenCode storage, config or credentials during QA; use isolated `XDG_*`. Never copy secrets into evidence or code. Real-model runs are authorized (PD-18); follow the account rules above.
8. **Status updates:** update `capability-inventory.md` rows only from retained evidence, and finish with OC-18 (an OpenCode certification ledger modelled on `docs/reference/claude-code-advanced-certification.md`).

## Definition of done

Every in-scope matrix row is either:
- **certified**, with retained artifacts (manifest including the binary-resolution and compatibility records, raw frames, readback, normalized events, `SHA256SUMS`), or
- explicitly **unsupported**, **gated**, **unavailable** or **excluded** with evidence and a user-visible truthful state.

On top of that:
- All OCE scenarios (OCE-00..24) pass at their stated tier, with the Claude comparison recorded for the shared-surface scenarios.
- No T0/T1 result is presented as real-provider proof.
- The shared contracts' OpenCode statements are updated.
- The adapter header records the certified version range.
