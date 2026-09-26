# Saved project commands

Configure a small action list in Settings → Projects → Project commands. Saved
commands are user-owned and **device-local**. Luna does not discover or import
repository hook files. A project may name Build, Test, Start app and Dev server
commands, with optional HTTP(S) preview URLs. Open a task's Terminal → Commands
to run one in that task's exact checkout, inspect output, stop or retry. Preview
uses the existing in-app browser; it does not imply the server is ready.

Commands use the host's `/bin/sh -c` and inherited engine environment. They get a
shell intentionally so pipelines, redirections and environment expansion work.
Checkout paths are passed as process cwd, never interpolated into shell text.
Interactive login profiles are not sourced. POSIX hosts are supported; Windows
reports an explicit unsupported-host error. Use foreground dev servers: detached
processes that escape their process group cannot be controlled by Stop.

Saving requires the original action list as an `expectedActions` snapshot.
The engine compares it while holding the configuration gate; stale editors get
a conflict instead of replacing newer work. The settings card preserves the
unsaved draft and lets the user reload current commands before saving again.

Configuration and immutable command snapshots live in separate tables in the
existing `docs.sqlite3` (schema13). Device admission protects mutations. The
existing backup VACUUM snapshot includes both tables; terminal logs keep their
existing bounded local retention and backup exclusion. Restored foreign-device
configuration cannot execute. Existing project cleanup removes both tables'
project rows. Task cleanup removes ordinary action receipts and terminal output;
checkout setup receipts remain until project deletion to prevent duplicate setup.
Unknown future database versions are rejected before changing the database.
This guard cannot retrofit older binaries that were released without it.

One action may run automatically on **new worktree creation**. The existing host
materialization path first reserves setup, then publishes the new checkout and
launches it. Reusing/importing an existing checkout or editing a command never
starts setup. A checkout generation includes Git's canonical admin-directory
identity, filesystem creation time and inode. Missing creation identity fails
closed. A replacement checkout at the same path gets a new generation.

Setup is Pending → Running → Succeeded, Failed or Canceled. When “Wait for setup”
is enabled, the existing durable agent Run waits; failure/cancellation resolves
its normal command receipt with an actionable error. Retry setup in Terminal →
Commands, then retry the task. Disabling the wait explicitly permits independent
work while setup runs. Status and output survive navigation; receipts survive
restart. Historical output can expire under terminal retention.

Each logical run has a stable ID, captured action digest and attempt number.
Retry preserves the captured command and checkout, increments the attempt and
retains the previous terminal ID. Completion updates are attempt-fenced. A PTY
waits for a random input handshake before user code starts; the engine sends it
only after committing the terminal ID and process group. Startup preserves known
terminal exits and cancels interrupted/pending attempts without replay. Retry is
blocked while the previous process group exists, conservatively including a
possibly reused ID. Luna never signals a recorded PID after restart. A retry of
an action with an unsatisfied setup dependency still respects its gate.

Source references inspected: Luna baseline d1c71c3a31d6ef7c895adc9084c27293940fce39;
T3 checkout e5a46d6c5d00b89afba5274a94d42428c8d79763 and fetched origin/main
2a9832b8019039f812706639c11ccc24ce4c4e05 (`projectScriptEditor.tsx`, checkout-aware
ChatView execution, `ProjectSetupScriptRunner.ts`). The fetched setup change
closes clean idle shells while keeping history. Luna instead observes terminal
process exit directly. BB checkout9c8112e546f042b923f1a0c080f6b427c2fa60f6 and
origin/main9c9bae7f36a237c7e1b96de3d4c2186d13967686 were inspected without changing
those checkouts; its dispatch path was unchanged between those refs.

Verification: `scripts/verify-project-commands.sh [artifact-directory]` runs real
disposable Git checkouts and PTYs with harmless commands. It writes receipts,
output and test logs. See `docs/qa/project-commands-failure-cases.md` for the
failure-first acceptance matrix and the native QA checklist.
