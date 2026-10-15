//! How a turn ended when it streamed nothing (v22). Such a turn leaves no
//! transcript row, so this host-local row, keyed by the message that owned
//! the turn, is its only terminal evidence. The engine owns the status
//! vocabulary.
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
             ON CONFLICT(task_id,message_id) DO UPDATE SET
               status=excluded.status,updated_at=excluded.updated_at",
            params![task_id, message_id, status, now],
        )?;
        Ok(())
    }

    /// The status recorded for the turn `message_id` owned, if it ended
    /// without streaming anything.
    pub fn unwritten_turn_end(
        &self,
        task_id: &str,
        message_id: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT status FROM task_unwritten_turn_ends
                 WHERE task_id=?1 AND message_id=?2",
                params![task_id, message_id],
                |row| row.get(0),
            )
            .optional()?)
    }
}
