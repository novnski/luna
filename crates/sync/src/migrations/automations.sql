CREATE TABLE automations (
 id TEXT PRIMARY KEY, owner_host_id TEXT NOT NULL, revision INTEGER NOT NULL,
 enabled INTEGER NOT NULL, deleted INTEGER NOT NULL, next_due_ms INTEGER,
 origin_task_id TEXT NOT NULL, record BLOB NOT NULL
) STRICT;
CREATE INDEX automations_due ON automations(owner_host_id,enabled,next_due_ms);
CREATE INDEX automations_origin ON automations(owner_host_id,origin_task_id,id);
CREATE TABLE automation_mutations (
 request_id TEXT PRIMARY KEY, automation_id TEXT NOT NULL, payload_hash TEXT NOT NULL,
 committed_at_ms INTEGER NOT NULL, receipt BLOB NOT NULL
) STRICT;
CREATE INDEX automation_mutations_history ON automation_mutations(automation_id,committed_at_ms);
CREATE TABLE automation_runs (
 id TEXT PRIMARY KEY, automation_id TEXT NOT NULL, owner_host_id TEXT NOT NULL,
 scheduled_at_ms INTEGER NOT NULL, task_id TEXT NOT NULL, message_id TEXT NOT NULL,
 blocks_next INTEGER NOT NULL, state TEXT NOT NULL, record BLOB NOT NULL
) STRICT;
CREATE INDEX automation_runs_history ON automation_runs(automation_id,id DESC);
CREATE INDEX automation_runs_active ON automation_runs(owner_host_id,blocks_next,id);
CREATE INDEX automation_runs_message ON automation_runs(task_id,message_id);
