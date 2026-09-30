# Truthful native steering delivery status

Implementation branch: `codex/parity-steering-status`. Build, deterministic execution, real-provider QA and native UI handoff are coordinator-owned and remain unverified until their saved evidence passes.

## Delivery contract

- `turn/steer` keeps native scheduling and forwards the message ID as `clientUserMessageId`. The ID correlates evidence and is never an idempotency key.
- A matching response `turnId` means **accepted/pending**. The queue row remains visible as “Steer accepted, pending”. It does not create a transcript boundary.
- A native `userMessage.clientId` with the exact expected turn, or exact native history readback, means **recorded in provider history**. This consumes the original queue claim and writes the same user-message ID once. It does not prove model sampling or subsequent tool execution.
- A correlated rejection holds the original row with its reason. Explicit Steer never becomes interrupt or a fresh turn.
- A lost, malformed or delayed acknowledgement is **unknown**. Late native records can still settle it. No automatic duplicate request is sent.
- Native `turn/completed.status` is authoritative for completed, failed and external interruptions.

`AgentEvent::Steered.user_message_id` is optional and serde-defaulted. Legacy adapters retain identity-free boundaries. For Codex, an exact ID with no `next_assistant_message_id` is historical delivery confirmation; it settles the receipt without rotating live output, starting a checkpoint or reopening activity. Reconnect reads only unresolved native queue delivery IDs on the next native resume. It does not launch a provider merely to poll old deliveries.

Native acknowledgements wait at most five seconds and history readback at most two seconds. A failed/absent history read preserves uncertainty. Older providers without a correlated user item remain pending/unknown; matching text is insufficient. Ordinary warm routing retains fresh-turn fallback only after an explicit rejection; ambiguous native delivery is never replayed after runtime exit.

## Repeatable deterministic validation

From the coordinator's primary checkout, after integrating the branch into the candidate being checked:

```bash
LUNA_STEERING_COORDINATOR=1 scripts/qa-steering-status.sh /Users/iulian/workspace/code/personal/luna-feature-checkouts/2026-09-30-codex-parity/evidence/steering-status
```

The script uses the primary build cache and creates a unique `deterministic.*` directory with exact commands, exit codes, elapsed seconds, bounded console output, complete logs, serialized adapter event traces and a SHA-256 manifest. It does not make model calls or open apps. The synthetic transport peer validates current native input/event shapes; it is not real-provider proof. The compile/Clippy steps and all test execution remain pending coordinator execution.

Failure scenarios recorded before implementation:

| Trigger | Required evidence/result |
| --- | --- |
| Ack while active tools run | Queue stays accepted/pending; no early user bubble or boundary. |
| Same text, wrong/missing client ID, foreign thread or stale turn | No settlement from text or unrelated identities. |
| Two accepted inputs, duplicate started/completed records | Exact IDs recorded once, in native order; no duplicate follow-up turn. |
| Lost ack, then native record | Original claim settles from native evidence; no resend. |
| Lost notification, terminal history read | Exact history ID settles before terminal Done. |
| No record before completion/failure/Stop/EOF | Durable unknown; no automatic replay. |
| Native external interruption | Interrupted even when local Stop was never pressed. |
| Resume history has one old ID and omits another | Only the recorded ID settles; no activity for the historical record and no inference from absence. |
| Evidence races unknown/ack projection | Applied/consumed claim cannot be resurrected or reported as a vanished delivery. |
| Timeout or failed read | Bounded wait; uncertainty retained; cancelled RPC waiters cleaned up. |

## Real provider and native UI acceptance

Use one signed LUNA Dev app and an ordinary disposable Git repository. Preserve the app data, build cache and evidence. Do not use a QA harness that creates worktrees. Verify the exact app process, IPC listener and signature before starting.

1. Start a Codex turn asking it to run one uniquely marked command that sleeps 20 seconds and prints the marker, then return a specified final token. While that actual command is active, submit a Steer row asking for a different final token. Capture the queue immediately after acceptance, the unresolved tool chip, then the completed tool output, the same user-message ID in native history and LUNA's document, and the final response. The command runs exactly once and finishes; acceptance alone creates no user bubble.
2. Repeat with two steer rows and two held tools. Capture both input correlation IDs, native order, each tool's single output, and queue consumption. A model response containing the new requirements is stronger evidence than user-message recording.
3. Repeat with Stop during a pending steer, a terminal provider failure, and an external native interruption. Inspect native terminal status and LUNA status/queue. Unknown rows remain available; no background resend or fresh turn is started.
4. For lost acknowledgement and lost notification, use the deterministic subprocess fault cases for transport regression. Real-provider claims require an actual controlled transport fault plus native history and host projection readback. Do not label a synthetic peer as a real Codex/model observation.
5. Restart/resume the provider after an unresolved delivery. Capture queue status before reconnect, native `thread/read(includeTurns:true)`, then the queue/document projection. Only exact native IDs settle; absence remains unknown. Historical confirmation must leave the activity state terminal until new work starts.
6. Exercise Queue, explicit Steer, Send now and Stop independently. Preserve each row's intent and verify that Steer never cancels active tools or starts a fallback turn.

Save provider readback, `WatchQueue`, `WatchDocMessages`, status/receipt snapshots, screenshots or Accessibility readback, exact prompts/markers, command outputs and app verification to the external steering evidence directory. Hash the reviewed milestone before any scoped fixture cleanup. Native QA is complete only when those artifacts establish the claims above.
