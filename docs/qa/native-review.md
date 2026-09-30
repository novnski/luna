# Native code review acceptance

The Codex task context menu's **Review code…** action offers working changes, base branch comparison, a commit, or custom instructions. Branch suggestions come from the task's execution host. Results can stay in the task or open in a separate ordinary LUNA review chat using the same project, host, checkout, account and settings. Separate review is rejected when the source task is hosted on a different device than its project, because ordinary chat creation resolves the host from the project.

Every review is a native `review/start` request. A dedicated `nativeReview` task-command admission kind makes older hosts reject this action instead of silently treating it as an ordinary turn. Current hosts normalize it into the existing durable Run payload after parsing. A separate review first uses the existing LUNA chat creation operation, then `thread/start` and `review/start` with inline delivery on its own provider thread. It does not create a worktree, copy conversation history, or replace the source task binding. Codex 0.159.0's release schema supports native detached delivery but explicitly deprecates it and recommends this separate-thread plus inline-review approach. See [official review API documentation](https://learn.chatgpt.com/docs/app-server) and the [0.159.0 review schema](https://github.com/openai/codex/blob/rust-v0.159.0/codex-rs/app-server-protocol/src/protocol/v2/review.rs).

Lifecycle items become an existing transcript tool chip and authoritative Markdown findings. Stable findings part IDs absorb repeated completion notifications. Errors, missing reviewer output and interruption remain visible; an interrupted review cannot retain a running chip. Review intent is preserved for an exact retry and cleared for ordinary follow-ups, answers, title generation and accepted-session continuation.

## Failure scenarios

- Native review must never become ordinary `turn/start` or text steering, including on a parked runtime.
- Reject empty targets, a non-Codex harness, active turns, changed account/checkout, attachments, worktree creation, pending context handoffs and stale revision/binding snapshots.
- A foreign `reviewThreadId`, missing turn/thread identity, RPC failure, interruption or absent reviewer output must not claim a successful review or rebind the task.
- Repeated exit items must not duplicate findings. Durable findings must survive engine restart.
- A profile change must not update/select a prior-profile task. Separate-chat failure must show the error and preserve any already-created review chat.

## Coordinator validation

All compilation, execution, model runs and app operations in this implementation wave are coordinator-owned. Run these after integration in the primary checkout, reusing its build cache:

```sh
scripts/cargo.sh check --locked -p luna-proto -p luna-doc -p luna-harness -p luna-engine -p luna-ui
scripts/cargo.sh test --locked -p luna-harness --test codex
scripts/cargo.sh clippy --locked -p luna-proto -p luna-doc -p luna-harness -p luna-engine -p luna-ui -- -D warnings
scripts/cargo.sh build --locked -p luna
LUNA_REVIEW_QA_EVIDENCE=/Users/iulian/workspace/code/personal/luna-feature-checkouts/2026-09-30-codex-parity/evidence/native-review node scripts/qa-native-review.mjs
```

The acceptance script uses a disposable ordinary Git repository and isolated engine data/Codex home. It drives the actual LUNA engine, durable command path, adapter subprocess and document persistence. It captures all four `review/start` target payloads, repeated findings completion, parked-runtime resume, separate binding isolation, invalid target rejection, provider rejection, missing output, active-turn conflict, native Stop, unexpected thread identity and restart persistence. Each run writes `result.json`, the protocol capture and engine log with SHA-256 hashes. This is deterministic protocol/engine/document evidence, **not real-provider or native UI proof**. No Git worktrees are created.

## Live provider and native UI scenarios

Use the primary LUNA Dev identity under coordinator control. Follow the project handoff rules, including one GUI app, graceful quit, process/IPC/signature readback and preserved data/cache. Capture screenshots and exported transcript/command/binding snapshots outside disposable checkouts, then hash the evidence.

1. Open an idle Codex task's context menu. Choose Review code, inspect all four targets and host-loaded branch suggestions, and verify the checkout shown.
2. Review an ordinary fixture repository containing a clear deliberate bug. Verify `review/start` for working changes, base branch and commit, then custom instructions with a specific review scope. Verify visible entered/exited lifecycle, actual findings and file/line links when the provider includes them.
3. Repeat in the same task after the provider parks. Verify the same account/cwd/provider binding is resumed and the new request remains a native review.
4. Choose Separate review chat. Verify a new LUNA/provider task opens, with the source project/host/cwd/account/settings, independent findings, and unchanged source binding/transcript.
5. Use an invalid branch/commit, stop an active review, and try reviewing during an active normal turn. Verify visible failure/interruption and no replacement of the active/source binding.
6. Reload/restart the app and verify the findings persist exactly once. Switch profiles while a picker is open and verify it cannot start/update/select a task in the prior profile.

Record real-provider coverage, CLI/model/account used, screenshots, transcript/binding evidence, outcomes and limitations separately from deterministic fixture results. A diff panel or prose-only review is not acceptance evidence.
