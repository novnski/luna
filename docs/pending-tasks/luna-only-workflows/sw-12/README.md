# SW-12 shipped Board/automation replay (E15)

Source: `8ff34b56`, 12 October 2026. Live status belongs to
`pending_tasks.py`. The [failure criteria](failures.md) were written before the
replay ran.

**Result:** every delivered behaviour of the
[Board/automation fix package](../../../design/board-automation-ticket-fixes/verification.md)
(LUN-1, LUN-2, LUN-11, LUN-12, LUN-13 and the related-tickets follow-up) still
holds on the current build. Nothing failed, so no ticket or correction was
opened and no feature was rebuilt. The original tickets and their history are
untouched.

## Replay against the criteria

| ID | Behaviour | Current proof (all exit 0) |
| --- | --- | --- |
| R1 | LUN-1 automation editor | `chat-workbench-fixture` w20m/w20n: the grouped create/edit form, readable cadence and next runs, date/time refusals. Switching to a valid interval retires the earlier "Enter the time as HH:MM." refusal. The list is captured in light and dark (`w20m-lun1-list-{light,dark}.png`), and the narrow window plus rendered contrast are checked. |
| R2 | LUN-2 status freshness | `chat-workbench-fixture`: a row deleted while the page was hidden is not shown on reveal, which revalidates within 2 frames. A visible delete leaves the list in 181 ms. An inline card moves from revision 1 Finished to revision 2 Deleted ([reveal](evidence/w20m-lun2-reveal.json), [card](evidence/w20n-lun2-card.json)). `automations_e2e::ticket_fix_automation_watch_observes_commits_and_scheduler` and the revision-fence tests: 30 passed. |
| R3 | LUN-11 bounded ranks | `board_store_e2e::ticket_fix_bounded_ranks_preserve_legacy_order_and_neighbors` and `update_archive_rank_and_watch_share_one_canonical_state`; `board_lifecycle_e2e` (14 passed). |
| R4 | LUN-12 safe deletion | `board_store_e2e::ticket_fix_delete_and_archive_remove_related_badges_without_losing_history` (reserved number and audit kept). `board_review_e2e::main_ticket_codex_reviewed_by_claude_repairs_seeded_defect_then_cleans_up` refuses deleting a Done ticket while later work is live, then deletes it once settled. `board-detail-fixture`: `running-ticket-not-deletable`, `delete-asks-before-sending`, `delete-removes-the-ticket`. |
| R5 | Related tickets | `board-chat-ui-fixture` (31 checks): `no-related-tickets-in-conversation`, `related-ticket-in-right-sidebar`, `related-row-opens-ticket`, `lun12-archive-leaves-sidebar`, `lun12-unarchive-returns`, `lun12-delete-leaves-sidebar`. `related-chats-fixture` passed. |
| R6 | LUN-13 same-chat images | `board_tools_e2e::ticket_fix_images_and_delete_cross_real_claude_mcp` and `queued_attachments`. `board-detail-fixture`: `engine-saved-the-brief-image-and-mention`, `first-start-delivers-the-brief-image`, `brief-images-replaced-through-the-engine`, lightbox. |

Totals:
- **Engine E2E:** 86 passed, 0 failed, 1 ignored. The ignored one is the live
  GitHub PR test ([counts](evidence/engine-e2e.json)).
- **UI fixtures:** `board-detail-fixture` 55/55 steps, `board-chat-ui-fixture`
  31/31, plus the `chat-workbench-fixture` and `related-chats-fixture` pass
  sentinels.
- **Changes since 10 October:** `board-chat-ui-fixture` has grown from 23 to 31
  checks. `board-detail-fixture` still runs the same 55.

## Original tickets in the primary app (read-only)

A read-only IPC readback against **LUNA Dev** (PID 17924, port 59777, signature
verified) compared the live records with the 10 October handoff. The script
([native-readback.mjs](evidence/native-readback.mjs)) allowlists only read
methods ([result](evidence/native-verification.json)). It found:
- LUN-1, LUN-2, LUN-11, LUN-12 and LUN-13 are active, and LUN-3 to LUN-10 are
  still archived.
- All five active tickets are still related to the source chat.
- LUN-12 is still at revision 2, with the same two audit events and the same
  source link.
- Its attached screenshot reads back over IPC with the original SHA-256
  `721ec3c7…8c1a3e`.

## Evidence

- Commands and exit codes: [commands.json](evidence/commands.json). Hashes of
  every retained artifact (202 files, including the 149 PNG captures):
  [sha256.json](evidence/sha256.json).
- Committed here: the four pass sentinels, the two step manifests, the
  workbench automation readbacks and the native result.
- Retained locally in `target/qa/sw-12/`: the full capture directories
  (`detail`, `chat`, `workbench`, `related`), the logs and the native ticket
  dumps.
- **App handling:** LUNA Dev was quit gracefully before the fixtures (it exited
  in 2 s) and run one at a time. It was then restored with
  `scripts/run-macos-dev.sh --refresh` and verified: PID 17924, IPC 59777,
  `codesign --verify --deep --strict` exit 0. No runtime code changed, so the
  refresh reused the existing build.

## Limitations

- **Not replayed:**
  - The live GitHub PR watch test. It is ignored because it needs an
    authenticated `gh` and a live PR target.
  - Multi-device Board replication, which is out of scope under PB-0.
- **No disposable native project:** E15's deletion, image and automation
  journeys ran through the production UI fixtures, which drive the real Shell
  with real input against an in-process engine, and the engine E2E suites. This
  follows the gate's primary owners. No adversarial native transport case was
  added because current coverage already owns ordering and stale responses
  (R2).
- **The primary app** was only read: no ticket, automation or attachment was
  changed.
