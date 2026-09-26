# Combined UUID WorktreeSpec and project setup: failure-first acceptance

Written before composing the Projects setup hook with Workspace's atomic
materialization. This is one disposable engine integration case. It must use
UUID task/space/command/message IDs and the real memory-RPC
EXECUTE_TASK_COMMAND `session → run` with WorktreeSpec. Direct DocHost queueing,
SetCwd, manual task-identity mutation and non-UUID shortcuts are excluded.
No production code or existing test assertion changes belong in this patch.

Fixture: one private Git repository, a saved user-owned setup action
`printf 'setup-attempt\n' | tee -a qa-setup-attempts; while [ ! -f qa-release-setup ]; do sleep 0.02; done; test -f qa-allow-setup`, a
recording Mock harness, target UUID chat and untouched sibling UUID chat in the
same project. The first worktree root is process-scoped under a temporary
LUNA_WORKTREES_DIR; Git hooks, GPG signing, remote access and actual provider
credentials are disabled/unused. Both tasks begin unbound at the repository
root. The action is configured in Luna's project-command store, not discovered
from a repository file. Preserve a source hash and JSON before assertions.

1. Submit target's first UUID Run with WorktreeSpec and fresh task-command
   revision/generation. Setup prints its marker and then waits for the fixture's
   `qa-release-setup` file. While it is paused, read Chat cwd, managed identity,
   sibling, pending Run and Git roots to JSON; assert committed metadata points
   at the physical checkout and Mock has not run. Then release the shell. Missing
   `qa-allow-setup` makes setup attempt1 Failed.
   The terminal output says setup-attempt, its receipt retains the captured
   action/cwd/exit, and the task Run receipt has an actionable terminal
   non-Applied outcome. No Mock provider dispatch occurs. One linked checkout
   exists; its canonical path equals target Chat.cwd and managed workspace
   currentPath. The sibling still points at the original root. Managed
   metadata commit happens before setup's shell can execute.
2. Read every failure fact to JSON BEFORE assertions, including the held-shell
   metadata snapshot and source hash: create/run receipts,
   original and current identity, sibling, chat cwd/branch, setup receipt and
   replayed terminal bytes, harness cwd observations, marker content and Git
   worktree inventory. A physical worktree alone cannot make Run Applied.
3. Shutdown/reopen the same isolated engine profile. The failed setup/output
   remains inspectable, no setup/provider work starts automatically, and no
   second checkout appears. Redeliver the exact original command ID/payload
   as a lost-ack retry; idempotence returns existing terminal state without
   rerunning setup or replacing the chosen workspace.
4. Create `qa-allow-setup` in that worktree and explicitly Retry the setup
   receipt. Attempt2 succeeds; the marker contains exactly two lines. Cwd,
   checkout generation and workspace identity remain stable. This does not
   resume the previously unsuccessful provider submission.
5. Submit a **fresh** ordinary UUID Run in the same chat with `worktree=None`
   and current worktree cwd. Mock records exactly that cwd. Its applied receipt,
   Chat.cwd and managed workspace currentPath agree; the first binding's
   workspaceId matches the task workspaceId. The sibling remains at the repo
   root. The provider acceptance advances the normal task revision once from
   the post-setup snapshot, without a workspace repair operation. The setup
   marker stays at two lines and Git still lists one linked checkout.

All commands are local and harmless. Old setup's captured command remains the
retry source if the project action was edited meanwhile. A still-running old
process group may block retry until it exits. The test must retain every
preassert artifact if a pending-Run bridge/metadata/provider fault occurs;
report that exact blocker rather than switching to a simpler task ID, changing
cwd by hand or treating a physical checkout as success.

Composition rule the case protects: reserve setup after physical Git creation;
Workspace's managed UUID path then atomically commits identity, Chat cwd/branch
and pending Run receipt. Only after that durable commit may setup start and gate
provider dispatch. Managed paths do not stamp cwd/branch a second time. Legacy
fresh checkouts retain the direct stamp and flush. An accepted exact-root reuse
must not launch setup again; bound tasks' unsupported reassociation may still be
refused by Workspace provenance checks. The task-command receipt cannot claim
Applied if setup or metadata commit failed.

Test source path: `crates/engine/tests/project_commands_worktree_setup.rs`.
Run only after the coordinator has composed Projects with the reviewed
Workspace/Lifecycle repair and granted the shared build slot:

```sh
LUNA_COMMAND_QA_ARTIFACTS="$PWD/target/qa/commands-workspace-setup" \
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test project_commands_worktree_setup -- --nocapture --test-threads=1
```

This matrix and test source are prepared in0ace; no result is claimed until the
composed runtime is built and its saved JSON/logs are read back.

Pre-run test-source SHA-256 (raw saved `.rs` bytes after rustfmt):
`d94ccce470fe531289a4b104928ad6efdf4ea77a8ad83f1ffe2f30fba80ea6ff`.
The test also includes its compile-time source SHA-256 in each JSON packet, so
the executed source can be compared with this reviewed checkpoint after a
composed branch adopts it.
