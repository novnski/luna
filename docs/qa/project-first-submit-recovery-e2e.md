# First-submit setup recovery: failure-first native E2E

This test exercises the real macOS composer, task commands, worktree creation,
durable project-command receipts, and draft recovery. Run it only against a
signed, isolated `LUNA QA Defaults` (port 59788) or `LUNA QA Projects
Integration` (port 59791) bundle. The read-only verifier is
`scripts/qa-project-first-submit-recovery.mjs`; it validates the exact bundle,
process, IPC port, data directory and codesign before reading state. Save a
fresh evidence directory under that checkout's `target/qa/` for each case.
The verifier never sends a prompt, runs a provider, edits a project, retries a
command, or deletes a worktree.

## Failure matrix

| Trigger | Required native result | Machine readback |
| --- | --- | --- |
| A. A fresh `New worktree` task has one `Wait for setup` action that exits 7 before a readiness marker exists. | Keep the minted task selected and in the sidebar. Terminal → Commands shows the failed captured setup command, output, and Retry. Show the Run receipt's exact `recoveryAction`: “Worktree setup failed. Open Terminal → Commands to inspect output and retry setup, then retry the task.” Keep the prompt recoverable on **that task**. No provider starts. | Exactly one new A task and Git worktree; `ProjectCommandStatus` has one setup run, attempt 1, failed, exit 7, same task/checkout; one rejected Run and no DeleteCommand; task-scoped `GetComposerDraft` has the recoverable prompt; no running session. |
| B. Send blocks before `CreateChat` (unavailable pinned model or equivalent preflight). | Keep the prompt on the new-task canvas with an actionable validation message; create no task. | No new chat, worktree or provider session; canvas draft retains prompt. |
| C. `CreateChat` applies, then Run rejects before a worktree or setup run is materialized (for example, a stale selected ref in a disposable Git fixture). | Delete the empty minted task and return the prompt to the canvas. Do not leave a phantom sidebar row. | Rejected Run receipt; no new visible chat, worktree or provider session; canvas draft retains prompt. If UI validation blocks before `CreateChat`, this is case B and must not be misreported as C. |
| D. Run receipt is pending or delivery is unknown. | Do not delete or resubmit before terminal receipt readback. | At most one Run command ID; final disposition follows the same A/C rules. |
| E. An existing task's Run rejects. | Never delete the existing task; keep its own draft and command history. | Same task ID remains; no DeleteCommand. |

The baseline on clean integration `6fce1828` demonstrated the failure in A:
`CreateChat` applied for `472ac23e-3591-4624-a563-6d17b315c151`, setup
`setup-238dd28a…` failed at attempt 1 with exit 7, Run
`c308461b…` rejected with the recovery action above, then DeleteCommand
`c80fa356…` applied. The UI displayed a generic rejection, removed the task,
and returned the prompt to the canvas. The signed app and complete read-only
receipt packet are preserved under
`target/qa/projects-native/6fce1828-retest/` in the integration checkout.

## Reproduce A and verify the fix

1. In a new, isolated QA data directory, run the existing
   `scripts/qa-project-defaults-live.mjs seed` fixture with all four matching
   `LUNA_DEV_*` overrides and `LUNA_PROJECT_DEFAULTS_QA_IDENTITY`. In Settings
   → Projects, set Defaults A to Codex `gpt-6-luna`, Approve for me, New
   worktree. Save exactly one worktree setup action with Wait enabled:

   ```sh
   printf 'SETUP_CWD=%s\n' "$PWD"
   if [ ! -f .qa-setup-allow ]; then
     printf 'PROJECT_SETUP_BLOCKED\n'
     exit 7
   fi
   printf 'PROJECT_SETUP_READY\n'
   ```

2. Set `qa_evidence` to a fresh absolute directory below this checkout's
   `target/qa/`. With the exact four `LUNA_DEV_*` values exported, run
   `node scripts/qa-project-first-submit-recovery.mjs baseline "$qa_evidence"`.
   Its `baseline.json` records the app identity, A config, chats, sessions and
   Git worktrees. A repeated case uses a **new** directory.
3. Use the native `+` to make a fresh Defaults A draft. Before Send, capture
   the project, `GPT-6-Luna`, Auto, New worktree, base ref and exact prompt:
   `Reply with exactly PROJECT_DEFAULTS_NATIVE_OK. Do not use tools or change files.`
   Send it **once**. Inspect the current task and Terminal → Commands; capture
   one screenshot showing the retained task, failed receipt and Retry, and one
   showing the actionable failure and recoverable prompt. Do not click Retry
   or submit a second provider prompt during this failure-first checkpoint.
4. Run `node scripts/qa-project-first-submit-recovery.mjs assert-setup
   "$qa_evidence" /absolute/path/to/current-task-screenshot.png`. The verifier
   writes `assert-setup.json` even when an assertion fails and exits nonzero.
   A human reviewer must confirm that the screenshot visibly shows the
   retained task, failed setup output, Retry, exact recovery text and draft
   recovery; the verifier checks durable IPC/Git state and references that
   screenshot. Do not infer UI visibility from IPC alone.

For B, take a separate baseline before choosing an unavailable pinned model
for the same synthetic A project; capture the blocked draft and run
`assert-precreate`. For C, use a **separate disposable project** and invalidate
only its selected Git base ref after the draft has loaded, before one Send.
If the UI blocks before `CreateChat`, record B instead. If a Run rejection
occurs, read its command ID from the QA task-command receipt trace, export it
as `LUNA_QA_REJECTED_RUN_COMMAND_ID`, capture the canvas and run `assert-empty`.
Never alter a real project or provider checkout to create these controls.

After A passes and the coordinator grants provider use, create/save only
`.qa-setup-allow` in the materialized synthetic worktree through Luna Files.
Inspect the **captured command text in the native receipt** before clicking
that receipt's Retry. Verify attempt 2 succeeds in the same checkout, then
explicitly retry the task once and check its configured model is still
`gpt-6-luna` before the provider starts. The expected reply is exactly
`PROJECT_DEFAULTS_NATIVE_OK`. No command or provider should replay merely
from task navigation or app restart.
