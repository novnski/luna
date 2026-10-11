# Failure, media and helper-control contracts

These cases are recorded before any SW implementation. Each starts from a
healthy allowed operation, then asserts the intended refusal or state and zero
forbidden downstream effects. "Observed" marks a case this baseline already saw
on the live engine. The rest are expected and must be reproduced by the owning
ticket before code changes.

## Expected failures

| Category | Case | Required behaviour | Owner |
| --- | --- | --- | --- |
| Wrong identity/scope | A call or result reaches another chat, account, project or host. A pasted `plugin://`/`app://` link mints a selection. A foreign-chat or prose-only image path is treated as authorized. | Refused by the existing fences: Claude's root `tool_use` id per turn, Codex's thread/turn fence, cross-task ancestry, own-chat attachment identity. Nothing goes upstream. | SW-01, SW-02, SW-09, SW-12 |
| Unavailable native capability | **Observed:** Executor is not loaded in Claude Ask/Auto. **Observed:** Drive and Spotify need auth. **Observed:** Codex MCP health is unknown. Claude's out-of-turn compact is unsupported. | A precise reason and supported remediation appear before sending. Never a silent switch to Full, and never "connected" presented as "callable". | SW-01, SW-02 |
| Denied action | The user declines a staged write, or Auto review escalates. | Zero effects on the independent service log. The decline stays visible. | SW-02, SW-04 |
| Unknown delivery | Transport is lost after the service commits but before acknowledgement. A command is queued but not accepted. A stop is accepted but not confirmed. | The state stays unknown until reconciled, with no silent resend. A `Requested` stop receipt is not reported as `Stopped`. | SW-04, SW-06, SW-10 |
| Stale result | **Observed:** all 16 acknowledged runtime contracts are `restartRequired`. The session MCP observation is outdated after a mode, account or config change. A question answer targets an older revision. | Staleness is shown with the generation it applies to. A stale answer cannot approve a new action. | SW-01, SW-06 |
| Restart | **Observed:** Claude session attachment is gone after engine start (memory-only). Resumed-session tool delivery is unproven. | After restart, readback reports "unknown until the session reports". Resume re-attaches the `luna` endpoint, verified live. | SW-01, SW-06 |
| Incomplete history | Claude transcripts are retained only from 30 September. Older or cloud-only Codex history is missing. | Shown as unavailable, never filled in, and grants no tools. | SW-11 |
| Inaccessible media | **Observed by source:** computer-use image bytes are replaced by byte counts (`harness/src/claude/tool_results.rs`), and `FetchToolBlob` returns text only. | A truthful "unavailable" fallback. A positive image case needs real authorized bytes. | SW-09 |
| Provider drift | **Observed:** Codex is 0.160.0 against a 0.154.0 validation, and Claude is 2.1.293 against the 2.1.286 record. | Tool lists and method names match ([README](README.md#providers-accounts-and-modes)). Each live ticket records the CLI version it certified, and a mismatch is a re-verification trigger. | SW-01, SW-02 |

## Shared media and artifact references (IC-0 boundary)

The IC-0 `shared` criterion owns the storage and reference contract. CORE-14
versions it. SW-09 consumes that contract and adds no reference type, blob store
or transport of its own. Until IC-1 lands, these are the only authorized host
byte routes:

- **Own-chat attachments:** `UploadChunk`, `UploadCommit`, `ReadAttachmentChunk`.
- **Workspace files:** `ReadWorkspaceImage`.
- **Private tool output:** `FetchToolBlob`, addressed by `<chat>/<part>`, text only.

Rules for SW-09 and IC:

1. A returned screenshot or image becomes viewable only through IC-1's typed
   lazy reference, with owner authorization and retention/deletion rules. Bytes
   are never inlined into synced text or journal metadata.
2. The computer-use omission policy stays. A capture that must be retained uses
   an explicit host-owned asset under the same IC-1 lifecycle.
3. If a provider's native result has no recoverable bytes, SW-09 shows that
   limitation and uses a verified capture route. It never claims an image from
   metadata.
4. SW-09 cannot start before IC-0 and CORE-14. The queue already encodes this
   dependency.

This records SW's side of the boundary. When IC-0 runs, it confirms or revises
the reference format and notes the result in its own package.

## Helper control

SW-10 attaches the existing catalog name **`interrupt_thread`**. It is currently
omitted with the reason "no verified cross-task command adapter is attached".
It is backed by the existing exact-target stop path in `engine/src/rpc/task_work.rs`:

- `PreviewStopTaskWork` returns a `preview_id` with the exact `items` and the
  `excluded` items.
- `StopTaskWork` takes `task_id`, the exact `work_ids`, that `preview_id` and
  a `request_id`. The receipt is stored durably with a request hash, so a retry
  is idempotent. It returns a `TaskWorkStopReceipt` with, per work item,
  `Requested` | `Stopped` | `Rejected` | `Unknown`. `TaskWorkKind` already covers
  `ChildChat` and `NativeChild`.

These receipt states are E12's accepted-stop, confirmed-stop, completion-race and
offline cases. No new control name, receipt type or generic destructive control
is added. `archive_thread`, `open_thread`, `fork_thread` and `move_thread` stay
omitted.

Authority:
- Existing user grant, ancestry and permission-ceiling checks apply.
- A child's report is data. It cannot authorize stopping a sibling or an
  unrelated task.
