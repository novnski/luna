# PB-4A (AW-13): feature groups and related-repository acceptance

Baseline: `f2e2c848` (13 October 2026). Live status belongs to
`pending_tasks.py`. This file is the decision record, the contract, the
failure cases (written before the code), the criteria mapping, the exact
commands and the native QA steps. Sources: [Board proposal](../proposal.md)
§4 ("child completion … does not satisfy unmet parent acceptance criteria
automatically"), §6 ("Space already means a host-owned project folder … use a
distinct feature/group concept"), §7 (PB-4 row) and "Required lifecycle
scope"; [research](../research.md) ("add a feature-group level only when
multiple tasks need shared acceptance criteria"); [agent workspace
opportunities](../../agent-workspace-app-research/luna-opportunities.md) row 13
and "Cross-repository feature bundles … never silently create worktrees or
mutate another repository because it was included in a feature label";
[product findings](../../agent-workspace-app-research/product-findings.md)
finding 5 (shared context groups related repositories; documented, not
tested). The [automatic review](../automatic-review.md) and
[chat model](../chat-model.md) specifications are unchanged by this ticket.

## Decision (approve or overrule)

**A feature group is a local Board record owned by one home Space on its
owning host. It names existing tickets as members; a member may be a ticket
of another Space on the same host (a related repository), but only when the
user explicitly links that ticket. The group has its own acceptance: shared
criteria text, optional named integration gates whose results the user
records, and one explicit Accept. Children reaching Done never accept the
group. The group only reads its members: it never edits a member ticket,
creates a chat, worktree, launch or review, moves a member, or grants anyone
access to another project. Each member keeps its own Space, checkout,
branch, worker and reviewer, and the group shows them as their own owners
report them. No new Space identity, no migration: groups live in one
host-owned Board record (the existing keyed Board store), served by two new
owner-local `Board*` RPC methods. Agents get no group tools in this ticket.**

To overrule, pick one of the rejected alternatives below; the shape of the
read model (members, gates, unmet reasons) stays usable for each.

## Rebased against current main

| Need | Current main | Decision |
| --- | --- | --- |
| Group several tickets under shared acceptance | Gap: no grouping record in `crates/proto/src/board/`, `crates/engine/src/board/`, the Board store or UI | **Implemented here** as `FeatureGroup` |
| Child completion must not accept the parent | Gap (no parent) | Acceptance is a separate, explicit, basis-bound record |
| Dependencies | Shipped (PB-2): `BoardDependency`, acyclic, **within one project**, never dispatches | **Reused, unchanged.** The group aggregates each member's open blockers; cross-repository ordering is expressed only as group gates |
| Review results | Shipped (PB-1R): `ReviewRun` per ticket, Done only after a passing current review | **Reused, read-only.** The group shows each member's latest run as its owner reports it |
| Per-repository checkout/branch | Shipped: `LaunchSnapshot.resolved` (cwd, checkout id, branch), the working chat's checkout, `Space.path` | **Reused, read-only**, plus a read-only `git branch --show-current` of the member's checkout |
| Space identity | `Space` = host-owned project folder (`crates/proto/src/entities.rs`) | **Preserved.** A group is never a Space and never renames one |
| All-projects aggregation | `BoardScope::AllProjects` keeps per-item project ownership | Same rule for groups: every member names its own project |
| Automatic dependency dispatch, more trackers, templates | PB-4B/C/D | Out of scope |

## Alternatives considered

| Alternative | Why rejected |
| --- | --- |
| **Group as a `WorkItem`** (a parent ticket with children) | A ticket can be dragged to Ready, batch-started, reviewed and completed; every lifecycle, review and agent-tool path would need a "not for groups" guard, and a missed guard would launch a chat for the group. Its column would also pretend to be a workflow state. |
| **Group as a new Space kind** / multi-root Space | Changes Space identity, which the proposal forbids ("do not rename it"), and every chat/project surface keys on Space. |
| **Related-repository scope with materialized checkouts** (shared context that clones or creates worktrees for each repository, as in product finding 5) | Would create worktrees or touch repositories implicitly and need cross-repository execution grants. The specs forbid both. A later ticket can add an explicit "start in related repository" action on top of this read model. |
| **Cross-project dependencies** | Dependencies are deliberately per project (PB-2 cycle checks and holds are per project). Gates express cross-repository integration without changing that contract. |
| **Auto-accept when every child is Done and every gate passed** | The specs require an explicit parent acceptance; a gate verdict recorded before the last child changed would otherwise accept a stale integration. |
| **Dedicated tables (migration v22)** | Gives indexed membership and journaled events, but moves `CURRENT_SCHEMA`, the backup table inventory and the migration-replay tests, which AW-11 is changing now, and makes every older build refuse the profile (`NewerSchema`) for a deferred P2 concept. The keyed record is bounded and atomic (see Storage). If groups need journaled events or cross-host projection, migrate the record into tables then. **No migration number is used by this ticket.** |
| **Agent tools** (`board_group_*`) | Not required by the outcome; adds authority questions (an agent in repo A linking repo B's ticket). The RPC binds the caller like every Board method, so tools can be added later with the same checks. |

## Architecture scope

### Data model (`crates/proto/src/board/groups.rs`, re-exported from `luna_proto`)

- `FeatureGroup { id: "fg-…", project_id (home Space, fixed), owner_host_id,
  title, acceptance (Markdown criteria), gates[], members[], acceptance_record?,
  archived, revision, created/updated at/by, next_gate, history[] }`.
- `FeatureGroupMember { work_item_id, project_id (the member's own Space),
  key, added_by, added_at_ms }`. A ticket's project is fixed at creation, so
  the recorded project is the member's repository identity.
- `FeatureGroupGate { id: "g1"…, label, result? }`; `result = { state:
  passed|failed, note?, evidence[], basis[], recorded_by, recorded_at_ms }`.
- `FeatureGroupAcceptance { basis[], accepted_by, accepted_at_ms, group_revision }`.
- **Basis**: for every member, sorted by id, `{ work_item_id, column?,
  execution_generation }` (`column` absent for a deleted/missing member).
  A gate result or an acceptance counts only while the current basis equals
  the recorded one.

Bounds: 100 groups per host, 30 members and 12 gates per group, title 200
characters, criteria 16 KiB, gate label 120 characters, note 2,000
characters, 8 evidence references, 50 history entries, 200 retained receipts.

### Ownership and storage

- The home project's owning host is the only writer (same as tickets).
  Members must be live tickets of Spaces this host owns.
- One JSON record per host in the profile's `docs.sqlite3` keyed store
  (`board-feature-groups:<deviceId>`, through `board_kv_load`/`board_kv_save`
  and device write admission). Every mutation is read-modify-write of that
  one record under the engine-wide Board gate `feature-groups`, so a
  mutation, its history entry and its idempotency receipt commit together.
  Included in backups with the rest of `docs.sqlite3`; not replicated
  (owner-local, like every Board record).
- Groups never write member tickets or their rows. A member's state is read
  live from the ticket's own owners on every read.

### RPC (CORE-01 catalog rows, both `local`/`localOnly`)

| Method | Id | Kind | Params → reply |
| --- | --- | --- | --- |
| `BoardFeatureGroups` | `luna.core/board-feature-groups` | query | `{scope?, groupId?, workItemId?, includeArchived?}` → `BoardFeatureGroupPage { groups: FeatureGroupView[] }` |
| `BoardFeatureGroupMutate` | `luna.core/board-feature-group-mutate` | command | `{requestId, action}` → `FeatureGroupMutation { receipt, group? }` |

`action.kind`: `create {projectId, title, acceptance?, gates?, members?}`,
`update {groupId, expectedRevision, title?, acceptance?}`, `addMember` /
`removeMember {groupId, expectedRevision, workItemId}`, `addGate {…, label}`,
`removeGate {…, gateId}`, `recordGate {…, gateId, state?: passed|failed,
note?, evidence?}` (no state clears), `accept`, `reopen`, `archive {…,
archived}`, `delete`. Every action except `create` carries the group's
`expectedRevision`.

Scope: a project scope returns groups homed there (`home: true`) and groups
that contain one of its tickets (`home: false`, a related repository's
view); All projects returns every group on this host.

### Read model (`FeatureGroupView`, engine-derived)

Per member: availability (`available|archived|deleted|missing|otherHost`),
its own project name/folder, key, title, column, phase, attention kinds,
source kind and issue, working chat, checkout (`workingChat|launch|project`
source, cwd, checkout id, recorded branch, live branch, requested checkout),
latest review run (id, state, verdict, generation, whether it belongs to the
current generation), open blockers (key, inside the group or not), execution
generation and revision. Then per gate a status (`pending|passed|failed|stale`),
the acceptance state (`notAccepted|accepted|stale`), the **unmet** list and
`canAccept`.

## Contract

1. **Child completion never satisfies the group.** Only `accept` records
   acceptance. `accept` is refused (`invalidInput` on `acceptance`, naming
   every unmet reason, nothing written) unless the unmet list is empty.
2. **Unmet reasons** (in this order): `noMembers`; per member
   `memberUnavailable` (deleted, missing, another host), `memberNotDone`
   (column and phase), `memberBlocked` (its open blockers, inside or outside
   the group); per gate `gatePending`, `gateFailed`, `gateStale`.
   Archived members are still members (archive only hides a card).
3. **Basis binding.** A gate result or acceptance recorded for one basis
   does not count for another: a member reopening, a new execution
   generation, or a membership change makes a passed gate `stale` and an
   acceptance `stale`. Reads never rewrite the record; the stale acceptance
   stays in history until `reopen` or a new `accept`.
4. **Membership changes clear acceptance** (history: `acceptanceCleared`), as
   do gate additions/removals, criteria edits and a gate result leaving
   `passed`. Title edits do not.
5. **Members are explicit and live.** `addMember` requires a live ticket of
   a Space on this host that the caller may read; the group's home needs
   write access. Linking writes nothing to the ticket. `removeMember`
   matches the recorded id or key, so a deleted ticket can still be removed.
6. **No implicit execution or grants.** No group operation creates a chat,
   launch, review, task link, worktree or dependency, changes a ticket's
   revision, column or project, or adds a caller scope. The group view reads
   each member's checkout, branch, worker and review owner and never merges
   them.
7. **Idempotency and concurrency.** The same request id and payload returns
   the stored receipt (no second effect); the same id with another payload
   is `idempotencyConflict`; a stale `expectedRevision` is `invalidInput`
   on `expectedRevision` with the current revision; no-ops return a receipt
   with `changed: false` and no revision bump.
8. **Restart and backup.** Groups, gates, acceptance and history survive an
   engine restart (same record).
9. **Change notification.** A mutation announces its members through the
   Board observation channel, so every open Board re-reads.

## Failure contract (written before the code)

| # | Failure | Required behaviour | Owner test |
| --- | --- | --- | --- |
| F1 | Creating or linking changes a member ticket (revision, column, project, key), adds a chat, launch, task link, review or Space, or renames a Space | Members and the Space list are byte-for-byte unchanged; zero new chats/launches | E2E `feature_group_spans_two_repositories_and_is_accepted_only_explicitly` |
| F2 | Every member reaches Done and the group is accepted automatically, or `canAccept` is true | Acceptance absent; `gatePending` unmet; `canAccept` false | same |
| F3 | `accept` succeeds while a member is not Done, blocked, unavailable, or a gate is pending/failed/stale | Refused with every reason; record unchanged | same, plus deleted-member steps |
| F4 | A member's project/column/phase/source/branch/checkout differs from its own `BoardRead` and repository; two repositories on different branches show one branch; a related member shows the home Space | Each member equals its own owner's readback and its repository's `git branch --show-current` | same |
| F5 | A gate passed or an acceptance recorded before a member reopened still counts | `gateStale`, acceptance `stale`, member `memberNotDone` | same |
| F6 | A blocker outside the group is hidden, or cleared state is kept | `memberBlocked` lists the open blocker, then disappears when it is Done | same |
| F7 | Retried mutation applies twice; same key with other payload accepted; stale revision overwrites | One effect; `idempotencyConflict`; `invalidInput expectedRevision` | same |
| F8 | Restart loses groups, gates, acceptance or history | Identical record after restart | same |
| F9 | A related repository's board misses the group, an unrelated project sees it | Project scope of the related Space lists it with `home: false`; unrelated scope lists nothing | same |
| F10 | Unlinking or deleting the group edits a ticket or another group; a deleted ticket silently disappears or counts as Done | Ticket unchanged; deleted member shown `memberUnavailable` until removed | same |
| F11 | Linking a deleted, missing or other-host ticket half-writes | Structured refusal; record unchanged | same |
| F12 | A member's review state is re-derived or invented | Equals the member's latest `ReviewRun` from its own `BoardRead` | E2E `member_review_runs_are_shown_as_their_owner_reports_them` |
| F13 | New methods are unrouted or fail unstructured | Routed; structured errors | existing `board_store_e2e` routing scenario |
| U1 | The UI creates a group or links a ticket with more than one RPC, or any `BoardMove`/`BoardStart`/`BoardControl`/review call | Exactly one `BoardFeatureGroupMutate` per action; zero execution calls | `board-page-fixture` `feature_groups_pass` |
| U2 | The panel names the wrong project for a related member, offers Accept while unmet, or hides unmet gates | Each member's own project name; Accept disabled; gate pending listed | same |
| U3 | Unlinking from the UI touches the ticket | Ticket revision unchanged | same |

## Criteria mapping

- **decision**: this file (local grouping in a home Space; related
  repositories only by explicit links; no new identity; architecture scope
  above).
- **contract**: Contract 1–9; implemented in `crates/engine/src/board/groups.rs`.
- **verification**: `crates/engine/tests/board_feature_groups_e2e.rs` over
  the real engine, Board store and two disposable Git repositories, writing
  manifests to `target/qa/board/feature-groups/`; the Board page fixture's
  feature-group pass (screenshots and readbacks with SHA-256).
- **handoff**: commands and native QA below (owned by the orchestrator).

## Files

Owned: `crates/proto/src/board/groups.rs` (new),
`crates/engine/src/board/groups.rs` (new),
`crates/engine/tests/board_feature_groups_e2e.rs` (new),
`crates/ui/src/shell/board/groups.rs` (new), `crates/ui/src/shell/board/page.rs`,
`crates/ui/examples/board-page-fixture.rs`, this file.

Shared, additive: `crates/proto/src/board/mod.rs` (module and re-export),
`crates/engine/src/board/mod.rs` (module), `crates/engine/src/rpc/board.rs`
(two arms), `crates/rpc/src/lib.rs` (two names in three lists),
`crates/rpc/src/operations.rs` and `crates/rpc/contract/v1/operations.json`
(two rows), `crates/ui/src/shell/board/mod.rs` (module),
`crates/ui/src/shell/board/api.rs` (two typed calls).

## Commands

```sh
export CARGO_INCREMENTAL=0
scripts/cargo.sh check --locked -p luna-proto -p luna-rpc -p luna-engine
scripts/cargo.sh test --locked -p luna-engine --test board_feature_groups_e2e -- --nocapture --test-threads=1
# Reused owners and catalog consumers:
scripts/cargo.sh test --locked -p luna-engine --test board_store_e2e --test board_lifecycle_e2e --test board_github_e2e
scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
scripts/cargo.sh test --locked -p luna-rpc --lib remote_auth
# UI (headless production Shell, real engine):
scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture --example board-page-fixture
target/debug/examples/board-page-fixture "$PWD/target/qa/board/page"
```

Expected: the E2E writes `target/qa/board/feature-groups/manifest.json` with
every step passed; the fixture writes `board-page.pass` and the
`fg-*.png`/`fg-*.json` snapshots listed in its manifest. If the contract test
reports stale files, regenerate with
`scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract`
(only the two new rows should change).

## Native QA (LUNA Dev)

1. Create two Spaces from two Git repositories on this Mac (for example
   `app` on `feature/login` and `api` on `feature/tokens`), and one ticket in
   each.
2. In `app`'s Board, open the ticket's menu (right-click or Shift+F10) →
   **New feature group with this ticket**. A *Feature groups* section
   appears with the group, *Not accepted*, its member and the gate
   *Integration check · pending*. No chat appears in the sidebar.
3. Switch the sidebar to `api`. Open its ticket's menu → **Add to ‹group›**.
   The section appears here too, marked as homed in `app`; each member shows
   its own project, branch and column.
4. Confirm **Accept** is disabled and the unmet list names both tickets
   ("not Done") and the pending gate.
5. Move both tickets through their normal Board workflow to Done. The group
   still says *Not accepted*; only the gate remains unmet.
6. Record the gate as **Passed**, then **Accept**. The group says
   *Accepted*.
7. Reopen one ticket (Move to Backlog). The group says *Accepted · out of
   date*, the gate shows *stale*, and that ticket is listed as not Done.
8. Remove a member with its **Remove** button. The ticket itself is
   unchanged on its own Board (same key, column and chat).
9. Quit and reopen LUNA Dev: the group, gate results and history are kept.

## Limitations and open questions

- Owner-local only: a group cannot include tickets of Spaces owned by
  another machine, and another machine's Board does not show it (same rule
  as tickets, pb0-contract §2.3).
- No group events in the Board journal; the group keeps its own bounded
  history. The UI re-reads on Board changes and every few seconds while a
  group is shown, so a related repository's change appears without a
  journal event in the home scope.
- The UI edits title/criteria and gates only through the engine API in this
  ticket: groups created from a ticket get the ticket's title and one
  *Integration check* gate. A rename/criteria editor is a follow-up.
- No agent tools; no automatic dispatch of members (PB-4D).
- A Space removed from LUNA keeps its groups in the record; they are hidden
  with the Space and reappear if it is re-added.
