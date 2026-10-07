# OpenCode feature parity: remaining work

**Goal (PD-14).** OpenCode should work in LUNA the same way Codex and Claude Code do. It goes through the same harness hooks, events, engine gates and UI surfaces wherever OpenCode has a native equivalent. OpenCode-only features without a LUNA counterpart are not pursued.

## Status on 2026-10-08

OC-00 to OC-15 and OC-17 are merged into `main`, integrated at `3275226b`. LUNA runs OpenCode 2 (v2.0.22) over HTTP+SSE with `serve --stdio`, a boot compatibility check, native steering, approvals and forms, full tool output, native subagents, history operations, LUNA tools, discovery, review, usage and updates. Three live E2E rounds (`oc/e2e-base`, `-r2`, `-r3`) fixed what they found, most recently the lost approval and question answers in `c7e3d5be`.

What remains:
1. **OC-16 remote-host parity:** OpenCode on luna-headless hosts, plus the hosted Console, Zen and Go certification lanes.
2. **OC-18 certification:** the OpenCode certification ledger in `docs/reference/`, a certified status for every in-scope checklist row, an OpenCode row in `docs/PARITY.md`, and a check of the shared contracts' OpenCode statements.
3. **PD-16:** the OpenCode file-restore button, deferred by decision.

## Files

- [implementation-plan.md](implementation-plan.md): ground rules, product decisions, OC-16 and OC-18.
- [capability-inventory.md](capability-inventory.md): the certification checklist, with each row's native behaviour, scope and certifying scenarios, and the list of OpenCode-only features that are not pursued.
- [validation-plan.md](validation-plan.md): failure cases, evidence contract, tiers and the OCE scenarios.
- [QA guide](../../reference/qa-infrastructure.md): the OpenCode QA identity, storage isolation, mock provider, live driver and in-app scenario runner.

The original research, including the gap audit, event matrix, coverage review, handover and workstream notes, is at `c7622350`. Its pre-implementation findings, such as OpenCode 2 failing to boot in LUNA, no longer apply.

## Product decisions

- The Plan/Build UI and replacement `/plan` stay cancelled.
- Native steering only.
- No plugin or MCP manager.
- No automatic worktree creation.
- Native subagents are not LUNA child chats.
- Never persist OpenCode "always" rules for LUNA modes.
- Tools are advertised only after verified delivery.
- Reuse shared surfaces.
- **PD-14:** parity means the same shared mechanisms as Codex and Claude; OpenCode-only features are not pursued.
- **PD-15:** OpenCode v1 (1.18) support is dropped; LUNA runs OpenCode 2 only.
- **PD-16:** the OpenCode file-restore button comes later.
- **PD-17:** keep the OpenCode Go usage-limits reader as is.
- **PD-18:** real-AI testing is authorized for every scenario, including Claude comparisons.
