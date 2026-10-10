# Useful Zeron changes for LUNA

**Status: pending; research and implementation specifications only.** Reviewed on **2026-10-11, Europe/Bucharest**. No runtime feature was implemented or certified in this task.

The local Zeron checkout was advanced from `b42fc2b8fbf247dd92796c2917f277535cea91ac` (v0.2.101) to official upstream `40091d469aa8a3f1ab1ec7cb404957b742940278` (v0.2.111). The range contains **78 commits, 70 excluding merges**, and changes 476 files. Its newest commit is dated 2026-10-10. The `novnski/zeron` fork remote still pointed at the earlier baseline; fetching that fork alone would have missed these changes. The clean local `main` was fast-forwarded directly from `zeronsh/zeron`; the fork remote was not pushed or reconfigured.

LUNA was compared at `c85fed541461b7aad62df2bfd495e7059e36d3f1`, verified equal to `origin/main` before documentation changes. Recommendations below come from current source and the active documentation, rather than assuming that everything added upstream is absent in this fork.

## The five additions worth keeping

| Task | Priority | Benefit to our workflow | Existing capability reused |
| --- | --- | --- | --- |
| **ZI-01 — isolate and bound turn snapshots** | P1, first | Prevent agent turns and Latest turn inspection from accumulating unreachable objects in the project's Git database or hanging on large untracked output. | Existing Latest turn diff, checkout identity and engine-owned storage. |
| **ZI-02 — bound workspace registry history** | P1, second | Keep a long-lived local workspace from retaining every rename/status write indefinitely; reduce avoidable loading, serialization and memory costs. | Existing registry rows, snapshots, guarded mutations and profile ownership. |
| **ZI-03 — show the actual current checklist** | P2 | See which investigation/implementation step the agent is working on without searching the transcript; restore missing Codex plan events and preserve active-step status. | Existing `ToolCall::Todo`, transcript fold and composer. |
| **ZI-04 — open a tool's file in the existing Files surface** | P2 | Inspect a file the agent just read or edited directly from its tool badge. | Existing Markdown workspace-file link resolution, owning-chat routing and file tabs. |
| **ZI-05 — search workspace text and open the matching line** | P2 | Find a symbol, error string or configuration value in the owning checkout without another agent turn or a manual terminal search. | Existing workspace read authority, Files search, command palette and editor navigation. |

ZI-01 and ZI-02 solve concrete source-level problems. ZI-03 and ZI-04 are small connections between data LUNA already has and actions we perform while supervising coding or technical investigations. ZI-05 adds the missing source-text lookup alongside existing file-name and conversation searches. None requires another dashboard, provider, hosted service or workspace model.

Read [the research and exclusion ledger](research.md), then [the implementation and E2E gates](implementation.md). [The machine-readable inventory](inventory.json) records every fetched commit, its disposition, pinned source references and the controlled Git mechanism probe.

## Duplicate prevention

The existing [agent workspace research](../agent-workspace-app-research/README.md), [LUNA-only migration plan](../luna-only-workflows/README.md), [interactive chat surfaces](../interactive-chat-surfaces/README.md), [Project Board](../devin-project-board/README.md), [Control this Mac](../control-this-mac/README.md) and [T3 remote/mobile plan](../t3-remote-mobile/README.md) retain their ownership. This package does not recreate their settings, review, supervision, visual-result, upload or remote-transport tasks.

The prior performance reports concern renderer/transcript/layout/presence behavior; the existing diagnostics/storage proposal is a visibility feature. ZI-02 specifically changes how current workspace rows and unacknowledged registry operations are retained. ZI-03 is a read-only projection of a provider checklist, not a Board ticket, a workflow engine or another task hierarchy. ZI-04 adds an entry point to the shipped file surface, not a file browser replacement. ZI-05 searches workspace bytes; L14's recorded-content search and IC-5's planned conversation search operate on transcripts. RM-06 retains ownership of a future phone search surface, consuming the existing desktop/host contract rather than duplicating it.

Recheck source and active plans before implementation. If another change has already closed a gap, mark that task superseded and link its evidence instead of implementing it twice. Follow [AGENTS.md](../../../AGENTS.md), [test-audit](../../../.codex/skills/test-audit/SKILL.md) for future test work, and [commit-and-push](../../../.codex/skills/commit-and-push/SKILL.md) after future implementation and checks.

## Evidence limits

All 78 commits were cataloged; substantive capability families were assessed against LUNA source and plans, with focused production-path and patch review for the selected items. This is not a line-by-line correctness audit of all 101,368 added lines. Zeron was not built or exercised through its native UI. Upstream performance numbers are upstream reports, not measurements of LUNA.

A disposable Git probe repeated the commands used by LUNA's snapshot function: three captures added **six object files** to the fixture checkout; the isolated-store variant added **zero**. Both preserved the real index and the same working-tree status. This establishes the Git mechanism, not an engine E2E result or a production performance claim. All probe repositories and stores were removed. Documentation links, pinned paths/lines, inventory coverage and whitespace are the checks appropriate to this documentation-only delivery; no Rust rebuild or app refresh is required.
