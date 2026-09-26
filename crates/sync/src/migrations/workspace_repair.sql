-- v15: device-local execution lineage across workspace association repair.
-- Receipt JSON and historical event payloads are deliberately unchanged.
ALTER TABLE task_command_state ADD COLUMN workspace_repair_revision INTEGER NOT NULL DEFAULT 0 CHECK(workspace_repair_revision >= 0);
ALTER TABLE task_command_receipts ADD COLUMN workspace_repair_revision INTEGER NOT NULL DEFAULT 0 CHECK(workspace_repair_revision >= 0);
ALTER TABLE local_executable_authority ADD COLUMN workspace_repair_revision INTEGER NOT NULL DEFAULT 0 CHECK(workspace_repair_revision >= 0);
CREATE TABLE task_execution_lineage (
    task_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    workspace_repair_revision INTEGER NOT NULL CHECK(workspace_repair_revision >= 0),
    PRIMARY KEY(task_id, message_id)
) STRICT;
