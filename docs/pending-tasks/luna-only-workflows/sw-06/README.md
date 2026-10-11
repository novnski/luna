# SW-06 long-session steering and recovery

Source: `1139e752` plus this change, 11 October 2026. Providers: Codex
`0.160.0` (`gpt-6.1-sol`) and Claude Code `2.1.293` (`claude-opus-5-5`).
Everything ran in the disposable signed QA app `LUNA QA Steering`
(`com.novnski.luna.qa.steering`, port 59852, isolated data, MCP isolation).
The [failure cases](failures.md) were written before the runs.

**Result:** one real defect reproduced and was fixed at its owner. Every other
E07/E08 contract passed live on both primary providers.

## Defect: a steer Claude queued past the turn's result was lost

**Live symptom (Claude S9S `completion-race`):**
- A Steer row was queued while a short turn ran.
- Claude 2.1.293 reported the input `queued` (`msg_lifecycle_v1`) instead of
  folding it in. It answered the running prompt, then started the steer as its
  own next turn.
- Claude's own session file shows both replies.
- LUNA retired the steer as `deliveryUnknown` at the first `result` and
  ignored the follow-on turn's `Steered` as stale.
- The user row and the reply never reached the transcript. The row stayed
  stuck in the queue, and Send now was refused.

Codex passed the same race.

**Wire:** captured with the real CLI
([frames](evidence/claude-2.1.293-race-frames.json),
[capture script](evidence/claude-race-capture.py)). In order:

1. B `queued` (0.812 s)
2. A's `result`, echoing A only (2.110 s)
3. A `completed`
4. B `started` (2.112 s)
5. a new `system/init`
6. B replay, B reply
7. B `result`, echoing B, then B `completed`

**Fix:** `crates/harness/src/claude/turns.rs` and `mod.rs`.
- The CC-02 turn ledger now records explicit steers Claude reports `queued`
  but not yet `started`.
- An **own** result that leaves such a steer unconsumed withholds its `Done`
  (never after an interrupt). Claude's replay of the steer then reaches the
  engine as the ordinary in-turn `Steered`: the row settles, the user row is
  inserted and the segment rotates. The steer's own result ends the LUNA turn.
- If Claude abandons the steer (cancelled, discarded or refused), the withheld
  `Done` is released. If the process exits first, the loop emits that real
  `Done` instead of a crash `Done`.
- CLIs without `msg_lifecycle_v1` keep the previous behaviour.

No engine, protocol or UI code changed.

**Proof:**
- Regression `claude_steer_queued_past_the_result_settles_once_with_its_reply`
  (E11b) in `crates/engine/tests/claude_cc06_e2e.rs`. It runs the production
  engine queue and RPC with the real `ClaudeHarness` against the new
  `steer-race` scenario in `fake-claude-cc06.py`, which plays the captured
  frame order.
- Before the fix it failed for the live reason
  ([log](evidence/e11b-before-fix.txt)): the queue row stayed `DeliveryUnknown`
  (SteerNow), with no steer user row and no reply.
- After the fix, the whole CC-06 suite passes (6). So do all Claude harness
  suites plus the lib (224 passed, 6 live-gated ignored) and all Claude engine
  suites plus `message_queue`, `native_inputs`, `queued_attachments`,
  `restart_resume` and `timeline_order_e2e` (81 passed, 1 live-gated ignored).
- Claude S9S passed twice on the fixed QA build. In both runs Claude's record
  shows the steer running as its own next turn, and LUNA delivered it once.

## Live coverage

| Contract | Codex | Claude | Runner step |
| --- | --- | --- | --- |
| Q1–Q3 nonblocking question: keep working, late answer as a new turn, amendment, idempotent repeat, answer steered into a running turn | pass (25) | pass (25) | S9Q |
| Q4 stale answer after Stop has no effect | — | pass | S1 `stale-answer-after-stop`: receipt `rejected`, no effect |
| Q5 queued image follow-up delivered once, with its image | — | pass | S7 `image-queued` (blue consumed) |
| Q6 live steer of an image Steer row into a running turn | pass | pass | S9S `steer-applied`: one working period, row once, image used |
| Q6 steer racing turn completion | pass | **fixed**, then pass ×2 | S9S `completion-race` |
| R1/R2 restart with a pending approval and an unaccepted queued row | — | pass | S4 pre/post: sends once, one unresolved approval, queued row kept and not delivered |
| R3 same task continues after restart; fork binding across restart | pass | pass | S4 `continue-after-restart`; `qa-related-chats-live.mjs` |
| R4/R5 fork, edit, rewind, reopen | pass | pass (39) | S5; Codex `qa-same-chat-rewind.mjs` (see note) |
| R6 deliberate fresh handoff | pass | pass | `qa-context-transfer-live.mjs`: native fork merge, active parent, Codex → Claude → Codex recall |

**Codex rewind** passed all eight checks: same binding and session, exact
retained prefix, a duplicate rewind is not applied twice, and the unsent draft,
file hashes and checkpoints all survive. The real same-session continuation
contains none of the rewound markers.

The first attempt never reached the rewind step: the turn settled, but the
model added a period to the exact marker the runner matches. The rerun
passed. Both attempts are kept.

S9S is new. It was added to `scripts/qa-opencode-scenarios.mjs` and, like
S9Q, is enabled for Codex at T3. The driver gained a `followUpBehavior` option
and a `sendQueued` helper.

**Other observation (not a defect):** S7's `image-steer` step asks for a steer
on a row queued with the normal "queue" behaviour. The UI never makes that
request: its Steer now action is only for Steer-behaviour rows without
attachments. The engine correctly keeps the row and delivers it once as the
next turn.

The refusal reason it reports is wrong, though. The hold says "another steer
remains unconfirmed" and the receipt says "being edited, reviewed, or
delivered", but the row simply isn't a steer row. This is recorded as a
follow-up.

## Repeat

[commands.json](evidence/commands.json) has every command and exit code.
[sha256.json](evidence/sha256.json) hashes the committed evidence, the local
raw run (`target/qa/luna-only-workflows/sw06-20261011T164424Z/`) and the
native transcripts, which stay local.

```sh
node scripts/qa-opencode-scenarios.mjs run S9S --provider claude-code --tier T3
node scripts/qa-opencode-scenarios.mjs run S9S --provider codex --tier T3 --model gpt-6.1-sol
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test claude_cc06_e2e
```

**Limitations:**
- Full mode for S5, S7, S9Q and S9S; Ask for S1 and S4.
- S1, S4, S5 and S7 have no Codex mode in the runner. Codex rewind, fork
  across restart and handoff are covered by their dedicated runners.
- The completion race is timing-dependent. Both live outcomes are accepted only
  when the row is delivered once, and the deterministic E11b fixes the Claude
  shape.
- A steer written within milliseconds of a result, before Claude reports
  `queued`, still follows the earlier path.
