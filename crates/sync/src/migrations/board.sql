-- Migration v19: project board (docs/design/project-board/pb0-contract.md).
-- Host-owned and additive. Every row keeps its full proto record as JSON so
-- later board milestones add optional fields without another migration; the
-- other columns exist only for uniqueness, ordering and bounded lookups.

-- Per-project ticket keys: prefix fixed at the first ticket, numbers never
-- reused.
CREATE TABLE board_projects (
 project_id TEXT PRIMARY KEY, owner_host_id TEXT NOT NULL, key_prefix TEXT NOT NULL,
 next_number INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX board_projects_prefix ON board_projects(owner_host_id,key_prefix);

CREATE TABLE board_items (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL, owner_host_id TEXT NOT NULL,
 number INTEGER NOT NULL, key TEXT NOT NULL, column_name TEXT NOT NULL,
 column_order INTEGER NOT NULL, archived INTEGER NOT NULL, rank TEXT NOT NULL,
 revision INTEGER NOT NULL, search_text TEXT NOT NULL, created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL, record BLOB NOT NULL
) STRICT;
CREATE UNIQUE INDEX board_items_number ON board_items(project_id,number);
CREATE INDEX board_items_order ON board_items(owner_host_id,archived,project_id,column_order,rank,id);
CREATE INDEX board_items_key ON board_items(owner_host_id,key);

-- Idempotency: one receipt per engine-normalized request id.
CREATE TABLE board_mutations (
 request_id TEXT PRIMARY KEY, work_item_id TEXT, operation TEXT NOT NULL,
 payload_hash TEXT NOT NULL, committed_at_ms INTEGER NOT NULL, receipt BLOB NOT NULL
) STRICT;
CREATE INDEX board_mutations_item ON board_mutations(work_item_id,committed_at_ms);

-- One canonical event per accepted mutation, committed with it.
CREATE TABLE board_events (
 seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL,
 work_item_id TEXT NOT NULL, project_id TEXT NOT NULL, revision INTEGER NOT NULL,
 kind TEXT NOT NULL, activity INTEGER NOT NULL, at_ms INTEGER NOT NULL, record BLOB NOT NULL
) STRICT;
CREATE INDEX board_events_item ON board_events(work_item_id,seq);
CREATE INDEX board_events_project ON board_events(project_id,seq);

-- At most one active working chat per ticket and one active ticket per chat.
CREATE TABLE board_task_links (
 id TEXT PRIMARY KEY, work_item_id TEXT NOT NULL, task_id TEXT NOT NULL,
 role TEXT NOT NULL, active INTEGER NOT NULL, record BLOB NOT NULL
) STRICT;
CREATE UNIQUE INDEX board_task_links_one_worker ON board_task_links(work_item_id) WHERE role='working' AND active=1;
CREATE UNIQUE INDEX board_task_links_one_ticket ON board_task_links(task_id) WHERE role='working' AND active=1;
CREATE INDEX board_task_links_task ON board_task_links(task_id,role);
CREATE INDEX board_task_links_item ON board_task_links(work_item_id,role);

-- One active link per external issue per project (deterministic dedupe).
CREATE TABLE board_issue_refs (
 id TEXT PRIMARY KEY, work_item_id TEXT NOT NULL, project_id TEXT NOT NULL,
 dedupe_key TEXT NOT NULL, active INTEGER NOT NULL, record BLOB NOT NULL
) STRICT;
CREATE UNIQUE INDEX board_issue_refs_dedupe ON board_issue_refs(project_id,dedupe_key) WHERE active=1;
CREATE INDEX board_issue_refs_item ON board_issue_refs(work_item_id);

-- Immutable launch snapshots; the launch request id is the no-replay key.
CREATE TABLE board_launches (
 id TEXT PRIMARY KEY, work_item_id TEXT NOT NULL, launch_request_id TEXT UNIQUE NOT NULL,
 execution_generation INTEGER NOT NULL, created_at_ms INTEGER NOT NULL, record BLOB NOT NULL
) STRICT;
CREATE INDEX board_launches_item ON board_launches(work_item_id,created_at_ms);

CREATE TABLE board_outcomes (
 id TEXT PRIMARY KEY, work_item_id TEXT NOT NULL, kind TEXT NOT NULL,
 observed_at_ms INTEGER NOT NULL, record BLOB NOT NULL
) STRICT;
CREATE INDEX board_outcomes_item ON board_outcomes(work_item_id,observed_at_ms);

-- Desired execution state, one row per ticket (CAS on version).
CREATE TABLE board_intents (
 work_item_id TEXT PRIMARY KEY, version INTEGER NOT NULL, intent_revision INTEGER NOT NULL,
 execution_generation INTEGER NOT NULL, desired TEXT NOT NULL, needs_reconcile INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL, record BLOB NOT NULL
) STRICT;
CREATE INDEX board_intents_reconcile ON board_intents(needs_reconcile,updated_at_ms);

CREATE TABLE board_review_runs (
 id TEXT PRIMARY KEY, work_item_id TEXT NOT NULL, reviewer_task_id TEXT, state TEXT NOT NULL,
 execution_generation INTEGER NOT NULL, version INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
 record BLOB NOT NULL
) STRICT;
CREATE INDEX board_review_runs_item ON board_review_runs(work_item_id,updated_at_ms);
CREATE INDEX board_review_runs_reviewer ON board_review_runs(reviewer_task_id);

-- Temporary authoring sessions: Board-only, never chat rows.
CREATE TABLE board_authoring_sessions (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL, owner_host_id TEXT NOT NULL,
 state TEXT NOT NULL, version INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL, record BLOB NOT NULL
) STRICT;
CREATE INDEX board_authoring_sessions_project ON board_authoring_sessions(project_id,state,updated_at_ms);
CREATE TABLE board_authoring_entries (
 session_id TEXT NOT NULL, seq INTEGER NOT NULL, entry TEXT NOT NULL,
 PRIMARY KEY(session_id,seq)
) STRICT;

CREATE TABLE board_dependencies (
 work_item_id TEXT NOT NULL, blocked_by_id TEXT NOT NULL, created_at_ms INTEGER NOT NULL,
 record BLOB NOT NULL, PRIMARY KEY(work_item_id,blocked_by_id)
) STRICT;
CREATE INDEX board_dependencies_blocker ON board_dependencies(blocked_by_id);
