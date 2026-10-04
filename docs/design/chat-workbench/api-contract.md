# Chat workbench backend contract

This is the shared implementation contract. Codex owns `crates/proto`,
`crates/rpc`, engine services and their tests; Claude owns `crates/ui` callers.
The canonical Rust types live in `luna_proto::chat_workbench` and are
re-exported at the crate root. JSON uses camelCase. Layout is device/profile
local presentation state, not synced lineage or provider state.

## Identity and state

`ChatWorkbenchTab { chat_id: String, side_parent_id: Option<String> }` identifies
one existing conversation. Normal main/child/alongside chats have no
`side_parent_id`; a side chat includes its actual source parent. `chat_id` is
the tab identity, so one conversation appears at most once in a workbench.
Titles, provider icons, run/approval status and ancestry come from live chat
metadata; do not persist a stale duplicate of them in the layout.

`ChatWorkbenchSnapshot`:

- `version: u32` (1), `main_chat_id: String`, `revision: u64`.
- `root: ChatWorkbenchNode`, `focused_pane_id: String`.
- `closed_tabs: Vec<ClosedWorkbenchTab>`; newest first, bounded history.

`ChatWorkbenchNode` is a tagged enum (`kind`):

- `Pane { id, tabs: Vec<ChatWorkbenchTab>, active_chat_id: Option<String> }`.
- `Split { id, axis: WorkbenchAxis, ratio: f64, first: Box<Node>, second: Box<Node> }`.

`WorkbenchAxis::Horizontal` means left/right children;
`Vertical` means top/bottom children. Ratio is the first child's fraction.
An intentional empty pane has no active chat. Every nonempty pane has one
active chat present in its tabs. Node IDs remain stable across rendering and
resize. The initial pane ID is `main-pane` and contains Main only.

`ClosedWorkbenchTab { tab, pane_id, index }` stores the last group and position.
If that group no longer exists, reopening uses the currently focused group.

The pure shared reducer `ChatWorkbenchSnapshot::apply(&ChatWorkbenchCommand)`
returns a changed/not-changed result or a typed validation error. It is atomic:
failed operations leave the original snapshot untouched. UI can use it for
optimistic visual feedback; the engine persists the authoritative result.
`validate()`, `pane(id)`, `pane_for_chat(chat_id)`, `focused_chat_id()` and
`contains_chat(chat_id)` are shared helpers. Revision is advanced by persistence,
not by a local preview reducer.

## Commands

`ChatWorkbenchCommand` is tagged by `op`:

| Operation | Fields | Semantics |
| --- | --- | --- |
| `openTab` | `tab`, optional `destination` | Open a closed/not-yet-open view; if already open, focus its existing location. |
| `focusTab` | `chatId` | Select that tab and its pane. |
| `focusPane` | `paneId` | Focus an existing group, including an empty group, without changing its selected tab. |
| `moveTab` | `chatId`, `destination` | Deliberately move/reorder the existing view; no duplicate. |
| `closeTab` | `chatId` | Hide and remember a non-main tab; refuse Main. |
| `closeOtherTabs` | `paneId`, `keepChatId` | Close other tabs in the group while also preserving Main. |
| `closePane` | `paneId` | Close its non-main views and collapse it; refuse a group containing Main. |
| `splitPane` | `nodeId`, `edge`, `newPaneId`, `splitId` | Add an empty neighboring pane around the target node. |
| `resizeSplit` | `splitId`, `ratio` | Persist the final finite ratio, constrained to 0.1–0.9. |
| `balancePanes` | none | Balance the entire saved tree by leaf-pane count in one mutation. Preserve axes, IDs, ordering, selection, focus and history. |
| `reopenClosed` | optional `chatId` | Reopen the named closed tab, or the newest eligible one. |

`WorkbenchDestination` is tagged by `kind`:

- `Pane { pane_id, index: Option<usize> }`: insertion index in the resulting
  tab order; `None` appends. Same-position drops are no-ops.
- `Split { node_id, edge, new_pane_id, split_id }`: edge is `left`, `right`,
  `top` or `bottom`; the moved/opened view becomes the new pane's active tab.
  A node may be a pane or the whole root tree (outside-edge drop).

Generate pane/split IDs once when accepting a gesture, not during render. A
self split of a sole tab is a no-op. Dragging an already-open Related row should
use `moveTab`; clicking its Open action should use `openTab` and focus it.
Closing/moving the last tab out of a pane collapses that source branch.

## RPC

Balancing weights panes rather than tabs. Each leaf, including an empty pane,
has weight one; each split's first-child ratio becomes that child's leaf count
divided by the total, constrained to the existing 0.1–0.9 bounds. This yields
equal pane areas in mixed horizontal/vertical trees except when an extreme
tree reaches a ratio bound (pixel seams and minimum rendered sizes can also
limit exact physical equality). Two stacked panes beside a single pane use
2/3 for the outer split and 1/2 for the stack. An already balanced tree or a
single pane is a no-op. The viewport can scroll to honor minimum pane sizes;
balancing does not rearrange or hide groups.

Capability: `chat-workbench-v1`. Do not call the new methods against older
engines; preserve the existing single-chat experience with a clear unavailable
message for new layout actions.

`GetChatWorkbench { mainChatId }` returns `ChatWorkbenchSnapshot`.
`WatchChatWorkbench { mainChatId }` streams the current snapshot and later
committed revisions. Neither method starts or resumes a provider.

`MutateChatWorkbench { mainChatId, profileId, expectedRevision, commandId, command }`
returns `ChatWorkbenchMutationResult { snapshot, applied, conflict }`.

- `profileId` must match the current `EngineInfo.profileId`.
- A stale revision returns `conflict: true` plus authoritative current state.
  Do not silently replace it with an old optimistic snapshot.
- Retry a transport-ambiguous action with the same `commandId` and exact
  request. A persisted bounded command ledger prevents duplicate close/split.
  Reusing a command ID with different content is an error.
- Unknown tabs, stale/deleted targets, invalid structures and missing Main
  fail without changing the persisted layout.
- Serialize the UI's workbench mutation queue. Guard every completion by engine
  generation, profile ID and main chat ID. Prefer one save at drag/resize commit,
  not a mutation per pointer move. Cross-window changes arrive through Watch.

The layout store is a separate atomic, bounded profile-local store and follows
the device write-admission lifetime. It accepts IDs, never caller file paths.
It has no transcript, draft, run, provider, branch or permission mutation.

## Fresh child from `+`

`CreateChildTab { parentTaskId, idempotencyKey, title?, provider?, model?, reasoning?, modelOptions? }`
returns `ChildThreadReceipt`. Use the workbench's immutable `main_chat_id` as
`parentTaskId`, never whichever pane is focused. This endpoint creates a genuine
blank child, marked `provisioning.createdBy: user`, in the main chat's current
checkout. It does not create a worktree, widen permissions, copy history, bind a
native session, or queue a synthetic prompt. It returns
`startupState: awaiting-first-message`. Its blank initial prompt remains a
manual-start marker; the ordinary queue owns all later user sends.

Default provider/model/reasoning/access follow the main chat's compatible
settings. Explicit model choices are validated against the actual account's
catalog. Parent permission and sandbox ceilings remain enforced. User-created
draft children have `reportResults: false` and `continueParent: false`; merely
opening a tab never triggers another chat. Parent-agent `create_thread` remains
its existing approval-gated, prompt-driven path.

The child creation and tab opening are intentionally separate durable effects:
after a successful receipt, open `ChatWorkbenchTab` using its `taskId`. If the
layout write fails, the child still appears in Related chats and can be reopened;
retry the same creation key rather than creating another child. A UI retry after
an unknown result keeps the exact key and request. Existing per-chat Composer
and queue methods send the first message once the user is ready.

## Runtime/view boundary

Reuse `PaneComposers` and fixed `ChatBinding`s. Key visible entities and drafts
by chat ID; never reconstruct a Composer on tab order/ratio changes. The Main
chat remains the workspace owner/left navigation selection. Pane focus is a
separate value. Each pane routes tool approvals, Send, Stop, queue and model
changes to its own binding. At most one visible view per chat in this workbench
keeps the existing owner-token watch contract valid. Close/unmount releases a
view watch only; it does not unload/end a side chat or terminate any session.
