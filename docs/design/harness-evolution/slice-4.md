# Durable effect boundaries

## Failure matrix, specified before implementation

1. A missing local provider account fails before provider I/O, but a queued
   message becomes `deliveryUnknown` and cannot be safely retried.
2. Send now interrupts an unrelated running turn before the selected provider's
   account, input and permission policy checks have passed.
3. A known local rejection is retried automatically after a document update,
   account repair or restart. It must remain Held until an explicit send.
4. A native protocol failure after entering the provider is misclassified as a
   local rejection. Its acceptance must remain Unknown and must not replay.
5. A rejection receipt commits separately from the held queue projection, or a
   failed transaction permits delivery. The two must commit atomically; an
   unresolved claim remains non-replayable.
6. Retrying the same command sends twice, changes the queue revision, drops
   attachments or rewrites the canonical user input.
7. A read-only preflight passes, but local configuration changes before dispatch.
   Dispatch must repeat the same checks under its existing serialization.
8. New classification accidentally relaxes existing queue, steering, recovery,
   task-command, workspace-authority or provider-acceptance boundaries.
9. Canceling or saving a queue edit clears the local-rejection hold after an
   account repair, silently resending the message without Send now.

## Narrow repair

Use a typed pre-provider rejection only for explicit read-only input, account,
registry and execution-policy checks. Run these before an explicit interruption
and repeat account/policy resolution at the actual dispatch boundary. Do not
infer acceptance from error strings or reclassify arbitrary harness errors.

Persist `executionUnavailable` on the exact claimed row together with a Rejected
command receipt and specific diagnostic. Automatic drain leaves this hold alone;
an explicit send rechecks current configuration. Editing or canceling an edit
preserves this hold. Existing unknown acceptance and
native acknowledgement paths keep their conservative behavior.

Verification uses full engine/RPC/document/SQLite lifecycle scenarios with a
controlled provider boundary, persisted readback across engine restart, and
repeatable JSON evidence. Existing queue, task-command and recovery integration
coverage remains the broader failure matrix. No new unit tests are added.
