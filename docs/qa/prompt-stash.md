# Prompt stash acceptance

The file-backed E2E uses a disposable engine profile and sends real upload,
draft, and stash RPCs through the in-memory client. The native pass uses the
isolated `LUNA QA Prompt Stash` app and its own data directory. Neither pass
uses a provider account or a user's files.

## Failure cases to exercise before accepting the implementation

1. A saved prompt must survive a process restart after its source draft is
   cleared. Clearing or retiring that draft must not make the uploaded file
   eligible for removal while the stash owns it.
2. A malformed, missing, changed, or out-of-root attachment must fail the
   stash write, or appear as needing reattachment on a later read. The source
   draft must remain available after a failed write. Quoted or terminal context
   supplied through RPC must be rejected until restore can replay it; malformed
   review-comment records must also be rejected.
3. Reusing a stash ID with different content must fail; retrying the exact
   entry must be idempotent. More than 50 entries or more than 1 MiB of
   metadata in one entry must be rejected without clearing the composer.
4. Restoring into a nonempty or unsaved editor must leave both the editor and
   the stash untouched. A successful restore must create an active draft,
   verify its durable readback, then remove the stash entry.
5. Deleting a stash must remove only its ownership. A blob shared with another
   active draft remains protected; after the final owner is removed, the
   profile's guarded unowned-removal path may delete it.
6. Up/Down recall must start only from an empty editor, skip attachment-only
   sends, and return to an empty editor after the newest sent prompt. With a
   typed draft, attachment, selection, edit, or ordinary multiline caret
   movement, Up/Down must retain the editor's normal behavior.

Run the focused file-backed test with `scripts/cargo.sh test --locked -p
luna-engine --test prompt_stash`. Retain its command log and a JSON acceptance
record with the run time, git SHA, fixture profile path, RPC outcomes,
attachment-owner transitions, and artifact paths. The native pass records
screenshots of the saved-prompts list and restored composer, plus the exact
bundle process, IPC listener, signature, and refresh duration.
