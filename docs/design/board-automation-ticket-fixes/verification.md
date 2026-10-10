# Automation and Board ticket fixes — verification

Implemented LUN-1, LUN-2, LUN-11, LUN-12 and LUN-13 with Codex owning the backend
and Claude Desktop owning the frontend. Read all five tickets and the complete
24-message source conversation before implementation. Baseline: `0e9416da`.

## Delivered behavior

| Ticket | Result |
| --- | --- |
| LUN-1 | Shared create/edit form grouped into schedule, destination, model and secondary options; readable cadence and compact list actions; obsolete validation errors disappear when the form changes. |
| LUN-2 | Host commit watch, revalidation on reveal, immediate receipt application and response-order guards for both lists and inline cards. |
| LUN-11 | Board-only 16-character rank keys; transactional legacy-order compaction and gap rebalance; content edits preserve concurrent rank changes. |
| LUN-12 | UI confirmation and agent `board_control.delete`; permanent tombstone with reserved number and retained audit; active, queued, uncertain or reviewing work blocks deletion, including later live work on a Done ticket. Archived/deleted source tickets leave chat banners and counts. |
| LUN-13 | Agent create/update accepts actual bound-chat upload references; foreign/prose-only/workspace paths are refused. Ticket images have thumbnails, full-size viewing and add/remove editing; bytes reach main and side workers in explicitly selected worktrees and survive backup/restore. |

The image E2E also found and fixed a dropped worktree selection in Board worker
creation. Main and side workers now use the chosen checkout. SQLite v20 prevents
older writers from erasing tombstones or allocating incompatible legacy ranks.

Board remains local to its owning host under the existing PB-0 contract. This
change supports existing attachment transfer/import roots and backup; it does not
introduce multi-device Board replication. The optional message-ID extension to
`read_thread` is outside this patch.

## Verification results

86 engine E2E tests passed; one pre-existing live GitHub PR test remains ignored
(it requires an authenticated `gh` and an explicit live PR target).
The covered suites are `automations_e2e` (30), `board_store_e2e` (7),
`board_authoring_e2e` (6), `board_chat_model_e2e` (9), `board_lifecycle_e2e` (14),
`board_review_e2e` (17), `board_tools_e2e` (2) and `queued_attachments` (1).
The Done/live-worker case was added to the existing completed-review scenario.

Rendered production UI fixtures passed:

- `board-chat-ui-fixture`: 23 checks, including archive/unarchive/delete updates.
- `board-detail-fixture`: 55 checks, including image load, full-size view, image
  replacement, delete confirmation, refusal while running and successful deletion.
- `chat-workbench-fixture`: full suite, including hidden-page deletion, visible
  deletion, inline cards, field round-trips, validation and light/dark/narrow views.

The visible automation deletion was observed in 164 ms; reopening after deletion
settled in two captured frames without painting the deleted row. The inline card
also reached Deleted in two frames. Exact adversarial response ordering is guarded
and code-reviewed; the rendered tests do not inject a forced late transport reply.

The affected crates' test/example targets compile. Workspace formatting and
Clippy pass; Clippy's existing warnings include no primary diagnostic on a changed
line. All Cargo verification used `scripts/cargo.sh`, `--locked` where applicable,
the shared target/cache and `CARGO_INCREMENTAL=0`.

## Before/after proof and retained evidence

Evidence root: `target/qa/board-automation-fixes-2026-10-10/`.

- `baseline-board-tests.log`: pre-fix ranks grew to 1,009 characters; archived
  source badges had no archived marker. The 2,000-insertion/restart case now passes
  with keys bounded to 16 characters and exact ordering.
- `engine-full.log`: the new image scenario exposed a worker remaining in the main
  checkout after requesting New worktree. `engine-checkout-repair.log` passes both
  main and side worktree/image routes.
- `delete-done-before.log` / `delete-done-after.log`: deletion was accepted during a
  later live turn on a Done ticket; the same regression now refuses it and succeeds
  after the turn ends while preserving the chat and its later history.
- `ui-before-proof/proof.json`: temporarily removing only the corresponding UI
  guards makes the rendered suites fail at `lun12-archive-leaves-strip` and
  `LUN-2: the reopened page painted …`. Both source files were restored to their
  exact SHA-256 values; no unexpected writer was found. Current fixture binaries
  were rebuilt after restoration.
- `ui-chat/`, `ui-workbench-final/`, `claude-detail-run-2/`: passing sentinels,
  machine-readable readbacks and screenshots. The earlier failing directories
  preserve the drawer fixture targeting diagnosis and stale validation screenshot.
- `target/qa/board/tools/image-flow/`: source PNG, private MCP results, exact main
  and side provider input blocks, and the ticket/launch readback.
- `target/qa/board/{store,review,tools}/`: scenario manifests and hashes.

## Native handoff

Refreshed the primary **LUNA Dev** with `scripts/run-macos-dev.sh --refresh`;
the build took 117 seconds. Verified its exact bundle process, port 59777 and
`codesign --verify --deep --strict`. No other Luna GUI app was running.

The original conversation now shows LUN-1, LUN-2 and LUN-11 plus two more related
tickets. All eight archived examples remain archived. A revision-guarded update
attached the original screenshot to LUN-12, revision 2; its bytes read back with
SHA-256 `721ec3c786e78dc75135586e05bcff5dacfde5afd15203ce1a3731df158c1a3e`.
Verified the thumbnail, full-size viewer and new automation form in the actual
app. The blank form was closed without saving; the app was left on the source chat.

`native-verification-latest.json` records the live readbacks, exact process and
evidence directory. `native-source-badges.png`, `native-lun12-attachment.png`,
`native-lun12-full-image.png` and `native-automation-editor.png` capture the handoff.
The full-image capture shows the historical screenshot inside the updated viewer.

The five reports remain in Backlog: implementation and verification ran outside
the Board's own worker/reviewer workflow. No Board execution/completion was invented
to change their status.

## Repeatable checks

```sh
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test automations_e2e --test board_store_e2e --test board_authoring_e2e \
  --test board_chat_model_e2e --test board_lifecycle_e2e --test board_review_e2e \
  --test board_tools_e2e --test queued_attachments
CARGO_INCREMENTAL=0 scripts/cargo.sh check --locked \
  -p luna-engine -p luna-ui -p luna-proto -p luna-rpc -p luna-sync \
  --tests --examples --features luna-ui/appshots-fixture
CARGO_INCREMENTAL=0 scripts/cargo.sh build --locked -p luna-ui \
  --features appshots-fixture --example board-detail-fixture \
  --example board-chat-ui-fixture --example chat-workbench-fixture
```

After gracefully quitting the current Luna app, run the three example binaries
one at a time with distinct absolute evidence directories. Restore the primary
app with `scripts/run-macos-dev.sh --refresh`, then verify its process, port and
signature. The retained `verify-primary.mjs` is read-only by default; its sole
optional mutation is the exact original screenshot attachment already completed.

## Follow-up: related tickets belong in the right sidebar

The user rejected the remaining five-ticket banner after `dfba2f00`. Related
tickets now appear only in the right sidebar's **Related** surface, in a compact
**Tickets** section. Each row opens its exact ticket; truncated titles have full
hover text. Tickets follow the focused main or side chat; a side chat's section
names that conversation, while the related-chat library keeps its existing owner.
The conversation retains only its own working/reviewer identity.
Persistent creation/conversion/hand-off receipts and their unused state were
removed. Creating a ticket keeps the pane and focus unchanged, and archive/delete
updates remove rows and counts without reopening the chat.

Claude Desktop implemented the UI; Codex verified the backend, reviewed the
change, ran checks and refreshed the primary app. The existing `BoardChatBadges`
snapshot and Board event watch supply all related tickets, archived flags and updates;
no backend or storage change was needed for this correction.

Evidence root: `target/qa/related-tickets-sidebar-2026-10-10/`.

- `before/manifest.json`: the new rendered checks failed against the old UI,
  including an actual creation receipt above the conversation and missing sidebar
  rows. The production code was unchanged during this baseline run.
- `side-before/manifest.json`: the focused-side-chat case failed before its fix;
  the ticket existed, but its sidebar row was missing. All 30 other checks passed.
- `side-final/manifest.json` and `side-final/board-chat-ui.pass`: **31 checks passed**,
  covering clear conversation space, five accessible sidebar rows, exact-ticket
  navigation, main/side focus changes, preserved identity and live archive/delete.
  The fixture clears its paint probe before each removal check so old coordinates
  cannot masquerade as rows still on screen.
- `related-chats-final/related-chats.pass`: existing sidebar navigation, drag, pin,
  settle/archive and side-chat behavior passed.
- `backend-e2e.log`: existing archive/delete/idempotency/restart regression passed.
  `backend-live-before.json` and `backend-live-complete.json` are read-only checks of
  the actual five active/eight archived tickets and retained original image.
- UI test targets compile; formatting and Clippy pass. The 356 existing Clippy
  diagnostics include none on changed lines (`clippy-complete-summary.json`).
- Final primary LUNA Dev refresh took **49 seconds**. Exact bundle process **15487**,
  IPC port **59777**, and strict deep code-signature validation passed.
  Native screenshots and AX readbacks confirm the original conversation has no
  banners, the Related sidebar lists LUN-1/2/11/12/13, and its LUN-12 row opens the
  correct ticket. `native-complete-closed` and `native-complete-related` captures
  and AX files verify the final build; the app was left on the source chat with
  the sidebar closed.

Repeat with no other Luna GUI running and separate absolute evidence directories:

```sh
CARGO_INCREMENTAL=0 scripts/cargo.sh build --locked -p luna-ui \
  --features appshots-fixture --example board-chat-ui-fixture \
  --example related-chats-fixture
target/debug/examples/board-chat-ui-fixture "$PWD/target/qa/sidebar-repeat/chat"
target/debug/examples/related-chats-fixture "$PWD/target/qa/sidebar-repeat/related"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test board_store_e2e \
  ticket_fix_delete_and_archive_remove_related_badges_without_losing_history \
  -- --exact
```
