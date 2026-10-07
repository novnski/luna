# OpenCode parity: remaining work packages

OC-00 to OC-15 and OC-17 are merged into `main`. Their plans were removed from this file on 2026-10-08; the full original plan is at `c7622350` (`git show c7622350:docs/pending-tasks/opencode-feature-parity/implementation-plan.md`). Two packages remain: OC-16 and OC-18. The OpenCode file-restore button stays deferred (PD-16).

**Parity scope (PD-14, 2026-10-07).** OpenCode should behave like Codex and Claude Code inside LUNA's existing shared surfaces. OpenCode-only features are not pursued; see the last section of [capability-inventory.md](capability-inventory.md).

## Ground rules

These come from the repository and user instructions; see [validation-plan.md](validation-plan.md) §1.
1. Record the OpenCode version you certify against. The research baseline is v2.0.22 (`527f0b93`), which is still the installed version on 2026-10-08.
2. Write each package's failure cases **before** its code ([validation-plan.md](validation-plan.md) §3). Do not add unit tests after implementation; prove behaviour with repeatable E2E artifacts.
3. Use `scripts/cargo.sh` with `--locked`. Run `python3 scripts/build-budget.py --status` before builds. Respect the 170 GiB aggregate budget and the exit-75 coordination rule. Never point two worktrees at one `CARGO_TARGET_DIR`; give each branch its own target subdir.
4. Run one LUNA GUI app at a time. QA uses the `LUNA QA OpenCode` identity from [the QA guide](../../reference/qa-infrastructure.md). Quit gracefully; never force-quit to free the slot.
5. After each meaningful change and its checks, refresh **LUNA Dev** with `scripts/run-macos-dev.sh --refresh`. Verify the process, port and `codesign`. Then follow the commit-and-push skill (`.codex/skills/commit-and-push/SKILL.md`) unless told to stop earlier.
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
   - **PD-16:** the OpenCode file-restore button comes later.
   - **PD-17:** keep the OpenCode Go usage-limits reader as is.
   - **PD-18:** real-AI testing is authorized for every scenario, including Claude comparisons.

## OC-16 — Remote-host parity (P2 / gated)

**What changes.**
1. **luna-headless hosts:** record each host's OpenCode binary resolution and compatibility. Capabilities and models come from the host; attachments resolve on the host. This is the same remote model Codex and Claude use. Find out which PATH and binary luna-headless sees on a remote host before relying on it.
2. **Hosted Console, Zen and Go:** separate certification lanes. Unavailable access is a blocker, not a pass.

The Go-limits credential question is settled by PD-17: LUNA keeps reading the saved key, doesn't copy the pattern elsewhere, and switches only if OpenCode adds an official route.

**Not pursued (PD-14):** attaching to external OpenCode servers or the user's managed service; OpenCode multi-account management.

**Rows:** F-WS-09, F-WS-11 and F-HOST-02 in [capability-inventory.md](capability-inventory.md). **Acceptance:** OCE-11 (remote device) and the remote environment of OCE-12. Failure cases OCF-08, 83, 130–133.

## OC-18 — Certification ledger and matrix close-out (required)

**What changes.**
1. Create `docs/reference/opencode-advanced-certification.md`, modelled on `docs/reference/claude-code-advanced-certification.md`. One ledger row per in-scope checklist row, with status `Certified`, `Supported · live QA pending`, `Unsupported in LUNA`, `Externally gated` or `Native OpenCode only`, plus evidence paths and hashes. List the rows not pursued under PD-14 once as `Native OpenCode only`.
2. Certify every in-scope row of [capability-inventory.md](capability-inventory.md) from retained artifacts only. The in-app scenario runner's S1–S8 and live extras already map to OCE IDs ([QA guide](../../reference/qa-infrastructure.md)); reuse their evidence where it covers a row.
3. Add an OpenCode row to `docs/PARITY.md` §4.
4. Check the shared contracts' OpenCode statements and update them where needed: `native-composer-inputs`, `message-queue`, `tool-output`, `mcp-elicitation`, `qa-infrastructure` and `chat-placement-and-child-models` under `docs/reference/`.
5. Refresh the adapter's "verified against …" header on certification.

**Acceptance:** every scenario has a real-model (T3) pass (PD-18). Every in-scope row is certified, or explicitly unsupported or gated with evidence; all OCE scenarios have retained artifacts with `SHA256SUMS`; T0/T1 results are never labelled as real-provider proof.

## Risks

| Risk | Mitigation |
| --- | --- |
| Upstream v2 keeps changing | Certify against a recorded version; the boot compatibility check classifies later versions |
| Shared OpenCode database with the user's own TUI and background service | QA storage isolation (`scripts/qa-opencode-env.sh`); no automatic data changes |
| Real-model test cost and quota | Authorized for every scenario (PD-18); prefer OpenCode free models; report usage after each batch |
