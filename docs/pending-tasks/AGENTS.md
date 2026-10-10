# Pending task agents

Read [README.md](README.md), [WORKFLOW.md](WORKFLOW.md) and the selected ticket's
specifications before editing. The root AGENTS.md and parent workspace rules
remain applicable.

- Pick work with `python3 scripts/pending_tasks.py claim --owner <name> --session
  <actual-chat-or-run-id>` from the repository root. A `next` read is no claim.
- Reuse an existing active claim for your session; never claim a second ticket
  or infer ownership from a Markdown status table. Keep the returned token.
- Inspect previous partial work, Git status, owner/checkout and resume notes.
  Respect dependencies and code-area reservations across feature folders.
- Heartbeat at least every 20 minutes and before long checks. Record evidence
  through criterion `check`, and useful handoff notes through `pause`/`block`.
- Before ending an unfinished run, pause or block it and save a checkpoint.
  If interrupted without a checkpoint, the expired claim stays reserved until
  an operator verifies the prior worker stopped and records recovery evidence.
- Complete only evidenced criteria and required review/delivery. Use `finish`
  after the actual result, then snapshot and deliver the status update.
- Intake future work through the template and `add`; preserve one owner for
  shared behavior. Deferred ideas need their scope decision before activation.
- Do not hand-edit progress.json or the SQLite store, launch agents, broaden
  production authority or recreate shipped functionality as incidental work.
