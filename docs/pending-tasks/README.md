# Pending tasks

- [Useful Zeron changes: snapshots, registry growth, checklist, file navigation and workspace search](zeron-upstream-improvements/README.md): five pending desktop improvements from the 2026-10-11 upstream review; excludes shipped capabilities and work already owned by other plans.

- [T3 remote/mobile: source research and LUNA gap plan](t3-remote-mobile/README.md): ten-agent audit of connections, native mobile features and current LUNA; detailed gaps, implementation tasks and future E2E, supplementing Control this Mac without changing direct-only V1.

- [LUNA-only workflows: prioritized migration plan](luna-only-workflows/README.md): pending coordination and implementation plan based on retained Codex/Claude usage; reuses existing HTML/MCP Apps/search work and shipped Board/automation fixes, with explicit task dependencies and E2E switching gates.
- [Control this Mac from a paired phone](control-this-mac/README.md): plan only; no part of M0–M4 is implemented.
- [Complete a two-version macOS GitHub Release updater trial](github-release-updater-two-version-trial.md): blocked while the repository is private and has no published Releases.
- [Agent workspace ideas from T3, Conductor, super.engineering and BB](agent-workspace-app-research/README.md): research; most of its fifteen opportunities are not started.
- [Interactive HTML replies, MCP Apps, and conversation search](interactive-chat-surfaces/README.md): implementation plan only; HTML replies must work through Codex, Claude Code and OpenCode, with a shared isolated renderer, capability-based MCP Apps support, and find within the selected conversation.
- [Project board, ticket chats, and unified Side chats](devin-project-board/README.md): PB-0–PB-3 and PB-1R are implemented (Board below Automations, ticket chats, Side chats, reviewer side chats, Discuss with AI, GitHub issues); only PB-4 (feature groups, more trackers, dependency automation) remains.

Completed tasks were removed on 2026-10-08: Codex feature parity, agent-created child threads, Claude Code parity research and the orchestrator V2/BB research. Recover any of them with `git show be17717e:docs/pending-tasks/<path>`. OpenCode feature parity was removed on 2026-10-09: its adapter and live E2E rounds are merged, its OC-18 certification is not pursued, and remote use of OpenCode moves to Control this Mac (`git show 2abe004b:docs/pending-tasks/opencode-feature-parity/<path>`).

Claude's [two-writer resume protection](../reference/claude-code-advanced-certification.md) passed live verification in the 2026-10-08 audit. Its native workflow lanes remain intentionally deferred; LUNA will build its own workflows later.
