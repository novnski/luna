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

Latest native code validation built in 75s, PID47527, IPC59777 and strict signature verified. SHA256: `b136f57b56347a0bc4ea61d5e08f1a16d39c0ce6650af04bce41c0fc86238f49`. It contains saved-output fix `c6e6a14d` and no-Keychain fix `7f86fc1f`. Native tests include separate child worktree/branch/model/access, correct inspector file ownership, Stop→rewind→same-session continuation, Claude fork/edit/review, and both providers’ Standard/Fast labels/bolts.

The resumed pass adds complete saved Bash output/Copy (1,200 lines/85,238 bytes with actual tail and matching hash), native Edit diff/receipt Copy, Agents pane running→completed model/token/progress, WebFetch session reuse, rare parent-model inclusion, Auto→Full grant reset and native MCP inert resource metadata/source. Fresh Providers refresh/Usage and a real Sonnet continuation succeed after removing Keychain access. Resumed evidence is `luna-feature-checkouts/2026-10-02-pane-chat/evidence/qa-completion-20261002/`; earlier evidence and exact prior reports remain preserved.

Repeatable focused checks:

```sh
scripts/cargo.sh test --locked -p luna-engine --test task_commands --test claude_history
scripts/cargo.sh test --locked -p luna-engine --test secret_input
scripts/cargo.sh test --locked -p luna-harness --test codex
scripts/cargo.sh clippy --locked -p luna-engine -p luna-harness -p luna-ui --lib
scripts/cargo.sh test --locked -p luna-harness --test claude_tool_results
scripts/cargo.sh test --locked -p luna-engine --test claude_tool_results --test no_keychain --test account_lifecycle_acceptance
scripts/cargo.sh fmt --all --check
```

Results: 37 affected command/history cases, 2 secret cases, 74 Codex adapter cases passed;6 optional Codex cases ignored. Earlier pane/side 23, native-agent/task 3 and coordination 6 checks pass. Existing stale-session UI check 1/1 and headless Codex prompt masking/MCP fixture pass. The new native process regression refuses live/missing/unknown/duplicate history and proves exact terminal prefix after revert. The engine terminal regression was authored before the correction and first failed with the same missing checkpoint as native QA.

Saved-output checks: harness5 passed/1 optional live replay ignored, including19 refusal cases and result replay after artifact deletion; engine3 passed, including private lazy fetch/restart and failure display. No-Keychain lifecycle5, account-switch1 and existing Claude account4 pass. The short-write subprocess and empty/shared-only credential shape cases first reproduced the new implementation hazards, then passed after corrections. Engine all-target Clippy and formatting pass with existing repository warnings. Two read-only GPT-6.1 Sol Extra High reviews confirm the corrections.

## Limits

The old QA chat damaged by a pre-fix restart is still refused when checkpoint ids are missing; ids are not guessed. Native queue drag was inconclusive because the automation gesture may omit GPUI’s required second move; backend reorder checks pass. Native external MCP persistent-scope/secret and Auto escalation producers were not observed; deterministic paths pass. Fast provider generation was omitted. Conversation rewind keeps files; file checkpoint restore is separate. Native external peer messaging is certified as unsupported. CC14 scheduling/Stop receipts pass3/3 with the current named grant; the pending wake was cancelled, so actual timer firing is unverified. MCP Enable/Disable was excluded because it writes provider global configuration.

LUNA no longer directly reads or writes macOS Keychain. Claude account metadata stays usable without readable credentials; the provider CLI owns authentication. New optional LUNA vaults use a private local key file. An existing vault whose original key is only in Keychain is preserved but unavailable until explicit recovery; no migration or replacement key is attempted. The development data has no such vault. New key/vault remain outside the backup allowlist, and serialized legacy archive labels are unchanged.
