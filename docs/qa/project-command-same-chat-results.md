# Commands same-chat context verification and source correction

This is separate from the earlier engine-replacement/busy admission findings.
Coordinator and Commands owner read the raw baseline on combined ff7f5620 under:
`/Users/iulian/.codex/worktrees/9f21/luna/target/qa/project-defaults/independent-baseline-1/`.

The UI suite produced2 passing controls and4 authenticated failures:

- Project move kept engine/device/chat/cwd/checkout fixed while project changed;
  an old rendered Run sent one request although its painted command was the
  source project's command and the current project configured a different one.
- Cwd materialization projection kept engine/device/chat/project fixed while
  cwd/checkout changed; an old rendered Run still sent one request.
- Both held-status cases wrote the old project/cwd snapshot after the live task
  context changed. Premise assertions passed; these were behavior failures.
- Fresh-context and unrelated title/project-label controls passed.

Both Run traces retained input render counts2→2. Live identity and context fields
were recorded separately from cached UI scope, so no scope representation was
frozen by the assertions. Full logs and JSON are under `commands-same-chat/`.

The actual UUID MoveProject/current-target control passed1/1 under `uuid-move/`:
move receipt Applied, task/cwd preserved, current project changed, both commands
Succeeded, the destination command selected for the same action ID, and old
Retry/Stop receipts refused. That is correct backend behavior; it is not a
materialization or backend wrong-cwd defect. The separate UUID materialization
engine companion was excluded and remains unproved/gated on Workspace+Lifecycle.

Only after this evidence, the source correction extends ProjectCommandScope with
the actual selected Chat row's space_id, cwd and checkout_id. Existing scope
invalidation retires the old view waiter/cache/busy ownership and starts a fresh
read when expanded. Existing rendered admission, reply and parent Output guards
all compare the extended scope. No title/label metadata participates. No backend
policy, accepted PTY lifetime, polling cadence, theme, helper or test assertion
was changed. Current accepted backend execution continues independently of the
UI waiter. The source correction still requires authorized fixed verification.
