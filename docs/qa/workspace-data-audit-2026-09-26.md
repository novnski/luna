# Workspace and data safety audit — 26 September 2026

Base: `7b141044c10650cc2ec476128763384cf5084cfe`. Isolated branch:
`codex/audit-workspace-data`. The completed backend audit at this base is not
repeated. No provider adapters, queue/identity internals, styling or real user
project data are changed.

## Failure modes recorded before implementation

1. Upload publication uses an overwriting rename at a destination derived from
   eight ID characters and a sanitized filename. Different IDs/names can alias;
   a changed replay also replaces an attachment used by a durable draft/history.
   Concurrent publishers must never both succeed with different bytes. A failed
   collision must preserve both the old attachment and retryable staged chunks.
   Identical replayed bytes may succeed, but a symlink destination must fail.
   Generated-image materialization has the same replacement problem when a stable
   event key is replayed after the provider's source image has changed.
2. Checkpoint capture/current-tree verification seeds a private index from HEAD.
   An ignored file newly tracked with `git add -f` is absent from HEAD, so capture
   omits its worktree bytes despite preserving its staged blob. Diffs omit changes,
   stale preview tokens can pass, and restore can delete that file. Capture must
   use real index membership without changing the user's staged/unstaged split.
3. Worktree deletion discards Git's refusal and recursively removes the supplied
   path anyway, then may delete its branch. Unregistered folders, the primary
   checkout, and locked worktrees must survive a refused deletion with an error;
   ordinary registered worktree deletion must retain its existing behavior.
4. Connected coverage exposed a process-aborting stack overflow in
   `checkout_file_diff_text_rpc_fits_the_default_worker_stack`. It also aborts
   against the original source, before any diff-capture fix. Crash frames locate
   the failure in `diff_sync::capture_diff`: `capture_git` carries an inline 64 KiB
   buffer through nested futures. Move this buffer to the heap and verify the
   existing real RPC regression on its unchanged default worker stack.

`crates/engine/tests/workspace_data_safety.rs` was written before implementation.
It exercises actual disposable files, simultaneous publication, subprocess Git,
managed worktree capture, optimistic preview rejection and verified restoration.

## Source changes and attribution

- Upload commits and generated-image intake share no-clobber publication. An
  existing regular file is accepted only after bounded byte comparison and
  revision readback. Changed bytes and symlink destinations fail; failed commits
  retain staged chunks. Existing durable paths remain compatible.
- Checkpoint capture and restore-token inspection seed their private indexes from
  the real index tree, preserving force-added ignored files and the original
  staged/unstaged split. Neither HEAD nor the user's index is capture output.
- Worktree removal propagates Git refusal before pruning or branch deletion.
  There is no recursive filesystem fallback.
- Diff capture allocates its await-spanning 64 KiB buffer on the heap. The existing
  RPC integration regression now completes on the unchanged default Tokio worker
  stack; no global stack override or shared RPC handler edit was required.

Checkpoint membership traces to `noviulian`'s `dc0e21b1`; upload publication to
`aedbd88f`. The shortened upload names, worktree recursive fallback (`0d8aca862`),
generated-image replacement (`9601a2440`), and inline diff buffer (`0d8aca862`)
also contain inherited upstream
behavior. They are included because they directly affect the authored durable
attachment, checkpoint, and workspace lifecycle flows; they are not all
attributed to the user's original changes.

## Verification

Run `scripts/qa-workspace-data.sh` for the disposable-root regressions and
connected file RPC, repository/diff, upload, backup, terminal, preview and Git
action suites. It retains commands, logs, statuses and cache reports beneath
`target/qa/workspace-data.*`. The nine new cases were written before source fixes.

The original implementations from `7b141044` were temporarily installed beneath
the prewritten integration suite, retaining the approved infrastructure wrapper.
All nine tests failed for their expected behavioral assertions (60 seconds,
including compilation). The fixed sources were restored and the exact same nine
tests passed (32 seconds, including compilation; test runtime 1.28 seconds).

Evidence is retained at `target/qa/workspace-data-2026-09-26/`:

- `baseline/test.log`, `baseline/result.txt`, `baseline/command.txt`, the exact
  integration source, and saved corrected source files.
- `fixed/test.log`, `fixed/result.txt`, `fixed/command.txt` and both cache reports.
- `focused/results.tsv` and command/log/cache files for directly connected tests.
- `connected-baseline/test.log` and
  `connected-baseline/crash/original-source-default-worker.ips` preserve the
  original-source process abort. The crash artifact SHA-256 is
  `d4bd24b736b3036bfa71e579d66c98e1608d7ec28f4dcf0b8972c4a33b9861b7`.
- `final/default-stack.{command,log,result}` verifies the corrected default-stack
  RPC directly, and `final/suite/results.tsv` records the complete successful run.

## Final source verification

**101 targeted tests passed, zero failures or ignored tests.** The dedicated
default-stack RPC rerun also passed (35 seconds including compilation; 0.32 seconds
test runtime), and it passed again within the full repository suite.

| Coverage | Passing tests |
| --- | ---: |
| New filesystem/Git regressions | 9 |
| Workspace files, repositories/diffs, accounts/uploads/titles, queued attachments | 37 |
| Backup control/restore, terminal lifecycle, preview routing | 27 |
| Existing checkpoint, upload/generated-image and Git-action checks | 28 |

Locked engine compilation, Clippy, formatting, whitespace, Bash syntax and
ShellCheck pass. Clippy exits successfully with 40 engine warnings in unchanged
code; this is not a claim of a warning-free crate. The repeatable runner stops
immediately on build-guard exit 75 rather than retrying or bypassing the guard.

The first connected batch passed workspace file RPCs (3), queued attachments (1),
and existing checkpoint/upload/Git-action checks (3/11/14). It also reproduced two
baseline failures: the diff-RPC stack overflow above, and the unrelated Claude
account fixture's `warnings.is_empty()` assertion. Original-source comparison is
retained in `connected-baseline/`. The final run imports the exact test-only
Claude-specific warning assertion reviewed in provider commit
`6ee5705a1f079ed9be3663bea1cee79ad85d7bde`; the missing Codex fixture may warn during
an all-provider listing. All 11 account/upload/title tests then pass. No provider
production code is modified here. That identical fixture hunk is the only
expected overlap with the provider branch.

## Native handoff on consolidated audit source

Native QA completed on reviewed combined source
`8169b69219f5d1b598f64189a8e2b1b937aa3d7c`. The guarded refresh built in **145 seconds**
(147 seconds including packaging/launch), with `CARGO_INCREMENTAL=0` and all four
assigned app overrides. `LUNA QA Workspace` runs as PID **8868** from this exact
worktree's `target/luna-macos-dev/LUNA QA Workspace.app/Contents/MacOS/luna`.
Its listener is `127.0.0.1:59783`; the bundle ID is
`com.novnski.luna.qa.workspace`, and its data directory is this worktree's
`target/luna-qa-data/workspace`. Strict deep code-signature verification passes.
Executable SHA-256:
`a9393729223050e51595a441fb4de7c2b8da579abf6e54cfd17a457495f68265`.

The real app passed four IPC scenarios on disposable local fixtures:

1. File read/save succeeds; a stale content hash returns a conflict and preserves
   the newer saved bytes.
2. Working-tree diff and exact old/new file-source readback complete on the native
   worker; the app remains responsive after the formerly aborting endpoint.
3. A colliding upload is rejected without replacing the first blob. ReadAttachment
   returns the original bytes, staged bytes remain retryable with a distinct name,
   and identical-byte replay succeeds.
4. Fixture-seeded checkpoint refs list and diff correctly, including a force-added
   ignored file. Restore preview refuses the shared disposable checkout; HEAD,
   the index tree, and worktree bytes remain unchanged.

The first driver run passed all four checks. A second pass added exact executable
targeting and verified cleanup readback: both disposable task/project cleanup
states are `completed`, uploaded test files and repository roots are removed.
Neither run had a driver/contract error or product failure. Checkpoints were
seeded directly in the disposable Git repository: this verifies native read/diff
and refusal contracts, **not** provider-triggered capture or native isolated
restore. Capture/restoration remain covered by the source integration tests.

Repeat with the separately built Workspace QA app:

```sh
LUNA_IPC_PORT=59783 \
LUNA_NATIVE_BUILD_REVISION=8169b69219f5d1b598f64189a8e2b1b937aa3d7c \
node scripts/qa-workspace-native.mjs
```

Evidence: `target/qa/workspace-data-2026-09-26/native/` contains build/refresh logs,
source revision, plist identity, process/listener readback, signature result,
executable hash and budget snapshots. `ipc-final/events.jsonl` records every
native reply and `ipc-final/result.json` records the four successful scenarios
and fixture removal. `ipc-first/` preserves the initial pass. The primary app's
59777 listener was identical before and after refresh. No primary app was
refreshed, user repository mutated, or provider session/prompt created.

## Handoff limits

The final source-verification target occupies 3.6 GiB (`target/debug`); no release
target or extra build directories were created. Normally terminated fixtures
remove their disposable roots. The aborting baseline may leave anonymous OS
temporary roots; no broad temp cleanup was attempted without ownership proof.
After native packaging, `target/debug` occupies 9.3 GiB. Aggregate budget snapshots
were 123.74 GiB before and 132.54 GiB after, below the 170 GiB operating ceiling;
no guard bypass or manual cache cleanup was used. The build slot was released
immediately after launch/readback while native IPC continued. Retain the target,
isolated app/data and QA artifacts for coordinator review. No push or merge to main
was performed by this worker.

This bounded audit is macOS filesystem/subprocess/RPC coverage. Windows/Linux
behavior, hostile external writers racing filesystem publication, and full
multi-device delivery are not established by this run. Legacy upload names stay
compatible: conflicting shortened names now fail safely instead of replacing
published bytes; no attachment-path migration is introduced.
