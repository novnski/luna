# SW-06 failure cases

Written before the live runs. The deterministic owners keep their contracts:
`message_queue`, `native_inputs`, `queued_attachments`, `restart_resume`, and
history and import. These cases add what real Codex and Claude sessions in the
signed QA app can break. Each one names the existing runner step that observes it.

## E07 — questions, queued images, steering

| # | Failure | Observable (runner step) |
| --- | --- | --- |
| Q1 | A nonblocking question blocks the agent, or its late answer is lost or applied to the wrong turn | S9Q `ask-and-keep-working`, `late-answer-new-turn` |
| Q2 | An amended answer delivers the old revision, or a repeated answer is delivered twice | S9Q `amend-answer`, `idempotent-repeat` |
| Q3 | An answer steered into a running turn starts a new turn or is duplicated | S9Q `answer-steers-running-turn` |
| Q4 | A stale answer after Stop approves or answers another action | S1 `stale-answer-after-stop`, S4 `stale-or-live-answer` |
| Q5 | A queued image follow-up is delivered twice, without its image, or to the wrong turn | S7 `image-queued` |
| Q6 | A steer into a busy turn duplicates the row, drops the image, or reports success it lacks (completion race) | S7 `image-steer`: the receipt is applied, rejected or conflict, the row appears once, and the image is used |

## E08 — restart, history, handoff

| # | Failure | Observable (runner step) |
| --- | --- | --- |
| R1 | A queued row that was never accepted is treated as completed, or is replayed after a restart | S4 `queued-row`, `after-restart-readback`; driver `verify-restart` (no replayed queue row) |
| R2 | A pending approval survives the restart as actionable on the wrong process, or is silently approved | S4 `after-restart-readback`, `stale-or-live-answer` |
| R3 | The same task cannot continue with the same model, mode and provider binding after a restart | S4 `continue-after-restart`; `qa-related-chats-live.mjs` binding across restart |
| R4 | A fork loses the source checkpoint or lineage, or inherits the source's approvals or live tool bridge | S5 `fork-user-row`, `fork-assistant-row`, `fork-continuation` |
| R5 | Rewind or edit keeps rewound content, or loses preserved history | S5 `edit-earlier-message`, `rewind`, `rewind-continuation`, `reopen`; `qa-same-chat-rewind.mjs` (Codex) |
| R6 | A deliberate fresh-chat handoff drops the source, branch or next-step context, or claims native context it lacks | `qa-context-transfer-live.mjs` |

Proof rules (validation plan §1):
- A model's reply is never itself verification. Delivery is read from receipts,
  the queue and transcript watch frames, and the provider's own record.
- A runner step reported as unsupported on a provider is recorded as a
  limitation. It never counts as a pass.
