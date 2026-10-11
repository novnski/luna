# SW-12 replay: what counts as a failure

These criteria were written before the replay ran. SW-12 replays the shipped
[Board/automation fix package](../../../design/board-automation-ticket-fixes/verification.md)
(LUN-1/2/11/12/13 and the related-tickets follow-up) on the current build.

A failure means the current build breaks one of the delivered behaviours below.
Only a failure reproduced on the current build opens a scoped correction under
the existing owner. A fixture that drifted from the current UI is repaired in
the fixture, and the behaviour is then re-verified.

| ID | Behaviour (ticket) | Owner proof replayed |
| --- | --- | --- |
| R1 | Automation editor: grouped form, readable cadence, stale validation clears; light/dark/narrow layout (LUN-1) | `chat-workbench-fixture`; `automations_e2e` |
| R2 | Automation status freshness: a delete or update is visible at once, survives reopening a hidden page, and a stale response can't resurrect it (LUN-2) | `chat-workbench-fixture`; `automations_e2e` |
| R3 | Board ranks: bounded keys, exact order across restart, content edits keep concurrent rank changes (LUN-11) | `board_store_e2e`, `board_lifecycle_e2e` |
| R4 | Safe ticket deletion: a settled ticket deletes, keeps its reserved number and audit; active, queued, uncertain or reviewing work refuses, including later live work on a Done ticket (LUN-12) | `board_store_e2e`, `board_review_e2e`, `board-detail-fixture` |
| R5 | Related tickets: listed only in the right sidebar's Related surface; archive/delete remove rows and counts without reopening the chat; rows open the exact ticket (follow-up) | `board-chat-ui-fixture`, `related-chats-fixture`, `board_store_e2e` |
| R6 | Same-chat image reuse: real bound-chat uploads create/update a ticket and reach main and side workers in the chosen worktree; foreign or prose-only references are refused (LUN-13) | `board_tools_e2e`, `queued_attachments`, `board-detail-fixture` |

Not replayed here: the live GitHub PR test (ignored; it needs an authenticated
`gh` and a live PR target), and multi-device Board replication, which is out of
scope under PB-0.
