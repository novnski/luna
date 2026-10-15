//! How a task's latest turn ended when it streamed nothing (v22). Such a turn
//! leaves no transcript row, so this host-local row is its only terminal
//! evidence. The engine owns the status vocabulary.
use super::*;

impl DocsStore {
    pub fn record_unwritten_turn_end(
        &self,
        task_id: &str,
        message_id: &str,
        status: &str,
        now: i64,
        permit: Option<&WritePermit>,
    ) -> Result<(), StoreError> {
        self.command_writer(permit)?.execute(
            "INSERT INTO task_unwritten_turn_ends(task_id,message_id,status,updated_at)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(task_id) DO UPDATE SET message_id=excluded.message_id,
               status=excluded.status,updated_at=excluded.updated_at",
            params![task_id, message_id, status, now],
        )?;
        Ok(())
    }

    /// `(message_id, status)` of the task's latest unwritten turn end.
    pub fn unwritten_turn_end(
        &self,
        task_id: &str,
    ) -> Result<Option<(String, String)>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT message_id,status FROM task_unwritten_turn_ends WHERE task_id=?1",
                [task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }
}
