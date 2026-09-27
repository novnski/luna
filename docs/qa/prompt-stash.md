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
7. Cmd+S while the message composer owns focus must stash that prompt. When a
   file editor or search control owns focus, the existing Save File behavior
   must remain in charge. The signed QA app at source `6bf780fe` is the native
   RED: Alpha saved by the visible button (stash count 1); one Cmd+S on a
   focused Beta draft left Beta in the editor and the count at 1. Recheck the
   same path after the focus-routing fix without starting a real provider.
8. The resting composer must show no Save prompt or Saved prompts controls and
   keep its original spacing. Cmd+S with a nonempty message input saves one
   entry; Cmd+S with an empty input opens the transient restore/delete list.
   Closing that list restores the normal composer layout.

Run the focused file-backed test with `scripts/cargo.sh test --locked -p
luna-engine --test prompt_stash`. Retain its command log and a JSON acceptance
record with the run time, git SHA, fixture profile path, RPC outcomes,
attachment-owner transitions, and artifact paths. The native pass records
screenshots of the saved-prompts list and restored composer, plus the exact
bundle process, IPC listener, signature, and refresh duration.

## Native recall and shortcut follow-up, 27 September 2026

An isolated signed Mock task exposed a recall traversal failure on baseline
`a6ac8781`: an empty composer recalled the newest sent prompt with Up, but
another Up moved the caret instead of recalling the older prompt. The
programmatic replacement emitted `Edited`, which cleared the active history
browsing state. The composer now recognizes that one replacement event while
still ending recall on a later user edit or caret move.

With two completed Mock turns, native Up/Up produced BETA then ALPHA;
Down/Down returned through BETA to an empty composer. A typed draft and a
modified recalled prompt both resisted further history replacement. Focused
file-editor Cmd+S saved an edited synthetic file to disk and created no stash
entry. The armed project-skill Remove control exposed the accessibility label
`Confirm remove skill and files` while the synthetic skill remained on disk
after the first click. The previous skill pass had already checked the second
click, so this follow-up stopped at the confirmation state.

The repeatable Mock seeder, failure and fixed JSON records, build/check/Clippy
logs, and SHA-256 manifest are in
`target/qa/stash-followup-2026-09-27/`. CUA supplied native visual and
accessibility readback in the coordinator chat; no local screenshot file was
saved in this follow-up. The QA app used its own bundle, data and port 59795,
passed strict codesigning and exact process/IPC checks, then quit gracefully.
Its bundle was removed after evidence was preserved; the isolated profile and
build cache were retained.
