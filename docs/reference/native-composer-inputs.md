# Native composer inputs

The composer keeps native selections separate from their readable projection in text. `luna_proto::ComposerInput` is a tagged `mention` (`name`, exact `plugin://<id>` or `app://<id>` URI) or `skill` (`name`, absolute host-native skill path). `inputItems` is optional on the wire and defaults to an empty vector. It persists through drafts, submitted snapshots, saved prompts, queued rows, queue editing, run requests and steering. A missing queue-edit vector preserves existing selections; `[]` clears them. Picker reconciliation can remove previously selected chips after edits; pasted text never creates native identity.

Images retain the existing `attachments` upload/staging transport. `pending://` refs wait for uploaded bytes, then resolve to absolute paths on the execution host. Codex receives deduplicated `localImage` items for initial turns, active steering and correlated-rejection fallback. Empty text with attached images is valid. Native inputs require readable regular host files. Admission follows what the standard Codex 0.159 CLI on the execution host decodes: PNG, JPEG, GIF and WebP everywhere, plus classic TIFF on macOS and BMP on Windows (the TUI's `arboard` dependency enables them in the single unified `image` crate). The original staged bytes are sent unchanged; Codex decodes and re-encodes them. SVG, and TIFF/BMP on hosts without that decoder, are rejected explicitly as `RejectedBeforeAcceptance`; the whole message, images and selections stay recoverable. Header validation does not fully decode the image. Claude retains its existing image handling. OpenCode receives each distinct staged image as a native `files[{uri, name}]` entry (a percent-encoded `file://` URL its server reads on the execution host) on every route: initial turn, boundary steer, warm follow-up, queued row, slash command, retry and restart. Admission (engine and adapter) accepts PNG, JPEG, GIF and WebP up to OpenCode's 20 MiB limit after a bounded decode, rejects SVG explicitly, and refuses images for a model whose catalog `capabilities.input` lacks `image` (OpenCode would drop them silently); all as `RejectedBeforeAcceptance`. Natively sent paths leave LUNA's path trailer, since OpenCode labels each file itself. OpenCode typed skills are sent like Claude's: skill selections resolve against `GET /api/skill` and ride `skills[{id}]`; plugin and app mentions stay Codex-only.

Native selections require Codex. They are verified again against the loaded runtime's account, workspace and thread policy immediately before initial dispatch, steering and fallback. Selected-input startup waits for this validation before returning the stream; catalog failure or timeout returns `RejectedBeforeAcceptance`. Unless recovery schedules an automatic resend (which confirms the same correlation when it starts), the engine then rejects only the exact uncommitted pending Run receipt for that correlation (workspace-repair and binding-generation fences) and marks the same-device frozen submission recoverable in the same transaction. Receipts already accepted, marked delivery-unknown, terminal or fenced out stay unchanged, and recovery keeps that stronger acceptance so the input is never replayed automatically. Discarding a recoverable submission also clears the matching draft's selections; a newer draft with different selections is kept. A cancellation during validation starts no user turn. Ordinary messages with no native selections incur no catalog reads. The request/queue/draft envelopes remain frozen during validation and retain exact identity for recovery. Existing accepted/unknown steering replay rules still apply.

## Repeatable offline proof

After combining the native-inputs and integrations-picker branches in the primary checkout, the coordinator runs:

```sh
scripts/qa-native-inputs.sh /absolute/path/to/evidence/native-inputs/central-validation
```

The adapter scenarios record actual subprocess JSON-RPC requests for image-only startup, attached steering, rejection before provider spawn, catalog rejection before `turn/start`, and attached correlated-steer fallback. The engine scenario covers upload staging, SQLite draft restart, queue lease/edit preservation, and dispatch through the real Codex adapter. `SHA256SUMS`, exit-code files, logs and JSON/JSONL payloads are retained. The fixture is deterministic and does not prove a real model or native GUI behavior. It creates no Git worktrees.

## Native/provider verification

The coordinator refreshes the primary app after compilation and regression checks, then verifies:

1. Drop/paste a representative PNG or JPEG, send it with empty text, and have Codex report a visible property of that image. Keep the transcript and a screenshot.
2. Send another image with text, queue an attached follow-up, and choose Steer while an actual turn is active. Confirm native correlation, transcript placement and model incorporation separately.
3. Select two native integrations with distinct IDs, change chats and restart the app, then reopen the draft and queued edit. Confirm the selected identities survive. Remove a chip, undo/redo, and confirm the picker reconciliation matches the visible selection.
4. Make a selected integration unavailable or switch execution account before dispatch. Confirm the selected input remains recoverable and no unvalidated provider turn starts.
5. Submit an image from a remote device, confirm the owning host's resolved path appears in `localImage`, and verify the bytes/content, not just HTTP success.
6. Try an invalid/missing image path and a stale queue edit. Confirm a recoverable failure. Submit ordinary Claude/OpenCode text and image requests to check their existing behavior.
7. On macOS, attach little- and big-endian TIFF images and confirm Codex describes their content. Attach an SVG with text and a selection; confirm the explicit rejection, a terminal Rejected receipt, and that the full input is restorable after restart.

Retain host-side payload evidence, app identity/IPC/signature checks, transcript/screen readback and hashes outside disposable checkouts. Do not label the offline fixture as native or real-provider proof.
