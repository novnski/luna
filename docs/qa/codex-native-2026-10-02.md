# Native QA corrections after composer parity

`claude/composer-parity` (`5496894c`) was merged into `main` as `94d02b7d` after the earlier full-pane redesign and QA fixes. One native tester used one LUNA Dev app, with real Sonnet 5.5 Low, Haiku 4.5 and GPT-6.1 Sol Low turns in disposable projects. Read-only reviewers used GPT-6.1 Sol Extra High. Provider configuration and unrelated pending-task documentation were preserved.

## Corrected behavior

- SQLite contention no longer strands a provider-executed queue item merely because acknowledgment initially returns BUSY/LOCKED. The write reservation and bounded recording retries never redispatch the prompt.
- Long side titles shrink while Close stays visible. Send to main appends to the existing parent draft.
- Native agent inspectors read through their active side owner without granting derived-document mutation. Inspector links, file editors and file-tree navigation retain the originating checkout. Same-named parent/child files use separate cache keys; stable editor ownership survives rename collisions.
- Claude's opening agent prompt is seeded once and reconciled with only its first plain exact native echo. Later identical instructions, attributed input and legacy seed-only producers remain intact.
- Codex rewind accepts settled completed/interrupted/failed history while rejecting duplicate/live/unknown turns. Known Codex terminal checkpoint ids are retained after Stop, including empty terminal rows. Only checkpointed Aborted Codex entries are kept in history prefixes; unknown boundaries still fail closed.

## Verification

Native red/green captures and repeatable artifacts are in `luna-feature-checkouts/2026-10-02-pane-chat/evidence/codex-native/`. The consolidated requested report is `luna-feature-checkouts/2026-10-01-goal-child-ui/reports/codex-qa-final.md`; detailed chronology is the pane-chat checkout's `reports/codex-pane-qa.md`. Reports distinguish real native checks from deterministic E2E and direct provider probes.

Latest native code validation built in 20s, IPC 59777 and strict signature verified. SHA256: `d3f843f3acfe2baa483afd12c8dc42af5322d116d9e9dbc871fbbc77e7dc164f`. Native tests include separate child worktree/branch/model/access, correct inspector file ownership, Stop→rewind→same-session continuation, Claude fork/edit/review, and both providers' Standard/Fast labels/bolts.

Repeatable focused checks:

```sh
scripts/cargo.sh test --locked -p luna-engine --test task_commands --test claude_history
scripts/cargo.sh test --locked -p luna-engine --test secret_input
scripts/cargo.sh test --locked -p luna-harness --test codex
scripts/cargo.sh clippy --locked -p luna-engine -p luna-harness -p luna-ui --lib
scripts/cargo.sh fmt --all -- --check
```

Results: 37 affected command/history cases, 2 secret cases, 74 Codex adapter cases passed;6 optional Codex cases ignored. Earlier pane/side 23, native-agent/task 3 and coordination 6 checks pass. Existing stale-session UI check 1/1 and headless Codex prompt masking/MCP fixture pass. The new native process regression refuses live/missing/unknown/duplicate history and proves exact terminal prefix after revert. The engine terminal regression was authored before the correction and first failed with the same missing checkpoint as native QA.

## Limits

The old QA chat damaged by a pre-fix restart is still refused when checkpoint ids are missing; ids are not guessed. Native queue drag was inconclusive because the automation gesture may omit GPUI's required second move; backend reorder checks pass. External native MCP persistent-scope/secret producers, extra Auto/WebFetch session variants, long-tail/diff Copy layouts and optional loop/peer/resource cases remain explicitly unverified. Deterministic masking/restart/MCP checks pass. Conversation rewind keeps files; file checkpoint restore remains a separate reviewed operation.
