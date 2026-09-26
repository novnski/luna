# Same-chat Commands context: separate failure-first reproduction

Parent runtime checkpoint: a679a411. This is separate from the four authenticated
engine/busy admission failures. No project/cwd scope extension is implemented.
The accepted source tree and in-flight verification candidate remain untouched;
this candidate is assembled through an isolated Git index.

Six GPUI cases:

1. Paint project-A Run, apply the projection of a supported local project move
   from /repo/sub to /repo (both own cwd /repo/sub, same Git checkout), keeping
   engine/device/chat IDs fixed. Both projects have action ID build, with
   different command text. Deliver the real painted mouse callback. It must not
   dispatch an old action into the changed project context.
2. Hold project-A status, apply that move, then release A. Old project/actions
   must not replace the current context; a current read remains possible.
3. Paint Run at the task's pre-materialization cwd, apply the same-chat worktree
   cwd/checkout projection, and deliver real mouse input. The stale painted
   context must be refused, without claiming the backend's new cwd is wrong.
4. Hold pre-materialization status; release after the projection. Old cwd/status
   must not overwrite the current context.
5. Fresh-context controls for both transitions: a fresh status read and freshly
   painted action in the new context dispatch exactly once.
6. Rename/title-only metadata with unchanged project/cwd/checkout: held status
   and fresh action still work. Such metadata must not be treated as a new scope.

The UI tests use real temporary overlapping directory layouts and controlled
registry projections, the validated held-RPC transport, real retained Commands
entity, actual ProjectCommandPanel rendering and Window mouse dispatch. Live task
project/cwd/checkout, cached snapshot/action, engine identity and requests are
recorded separately. No callback or identity predicate is mirrored.

One positive EngineCore/RPC companion separately proves the projections are
supported: actual moveProject between overlapping Git roots preserves task/cwd,
then Run resolves the destination's same action ID/current command. Actual
WorktreeSpec materialization preserves task/project and changes cwd; a fresh Run
executes there. Old receipt Stop/Retry stays protected by existing matching.
Commands are harmless printf/pwd marker writes in disposable fixtures. The
backend using current authoritative task cwd is the expected behavior.

All cases are source-prepared, not executed or validated yet. Review/scheduling
must precede a baseline run. Preserve every existing accepted result independently.

Reviewed candidate commands (do not run without a slot):

```sh
LUNA_COMMAND_QA_ARTIFACTS="$PWD/target/qa/commands-same-chat-baseline" \
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test project_commands_context -- --nocapture --test-threads=1
LUNA_EPOCH_QA_ARTIFACTS="$PWD/target/qa/commands-same-chat-baseline" \
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib \
  commands_same_chat -- --nocapture --test-threads=1
```

The engine positive control should pass, establishing supported transitions and
correct current-target execution. UI stale cases are expected to fail separately;
that expectation is not a measured result. UI fresh/metadata controls must pass
before accepting the reproduction. The source can be reviewed/applied independently
of the currently accepted engine/busy admission fix and its fixed22 packet.
