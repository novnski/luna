//! Device-local, bounded terminal output. This records what was observed, not
//! a process that can be resumed after the owning engine has exited.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use zeron_proto::{TerminalEvent, TerminalInventoryEntry, TerminalProcessState};

const MAX_SAVED_OUTPUT_BYTES: i64 = 1024 * 1024;
const MAX_SAVED_TERMINALS: usize = 256;
const MAX_SAVED_TERMINALS_PER_TASK: usize = 16;

pub(super) struct TerminalHistory {
    db: Mutex<Connection>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn state_name(state: TerminalProcessState) -> &'static str {
    match state {
        TerminalProcessState::Running => "running",
        TerminalProcessState::Exited => "exited",
        TerminalProcessState::Stopped => "stopped",
        TerminalProcessState::Interrupted => "interrupted",
    }
}

fn parse_state(state: &str) -> rusqlite::Result<TerminalProcessState> {
    match state {
        "running" => Ok(TerminalProcessState::Running),
        "exited" => Ok(TerminalProcessState::Exited),
        "stopped" => Ok(TerminalProcessState::Stopped),
        "interrupted" => Ok(TerminalProcessState::Interrupted),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl TerminalHistory {
    pub(super) fn open(root: &Path) -> rusqlite::Result<Self> {
        std::fs::create_dir_all(root)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        }
        let db = Connection::open(root.join("terminals.sqlite"))?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS terminals (
               id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, device_id TEXT NOT NULL,
               cwd TEXT NOT NULL, shell TEXT NOT NULL, state TEXT NOT NULL,
               exit_code INTEGER, last_seq INTEGER NOT NULL DEFAULT 0,
               updated_at_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS terminals_by_chat ON terminals(chat_id, updated_at_ms);
             CREATE TABLE IF NOT EXISTS terminal_events (
               terminal_id TEXT NOT NULL REFERENCES terminals(id) ON DELETE CASCADE,
               seq INTEGER NOT NULL, payload TEXT NOT NULL, bytes INTEGER NOT NULL,
               PRIMARY KEY (terminal_id, seq)
             );",
        )?;
        // A new engine cannot inherit old PTYs. Crash and clean-shutdown paths
        // both resolve any residual running row before the first inventory read.
        db.execute(
            "UPDATE terminals SET state='interrupted', updated_at_ms=?1 WHERE state='running'",
            [now_ms()],
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    pub(super) fn create(&self, entry: &TerminalInventoryEntry) -> rusqlite::Result<()> {
        let mut db = lock(&self.db);
        let tx = db.transaction()?;
        tx.execute(
            "INSERT INTO terminals (id, chat_id, device_id, cwd, shell, state, exit_code, last_seq, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 0, ?7)",
            params![entry.id, entry.chat_id, entry.device_id, entry.cwd, entry.shell,
                state_name(entry.state), entry.updated_at_ms],
        )?;
        let mut old = tx.prepare(
            "SELECT id FROM terminals WHERE state != 'running' ORDER BY updated_at_ms DESC, id DESC LIMIT -1 OFFSET ?1",
        )?;
        let ids = old
            .query_map([MAX_SAVED_TERMINALS as i64], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(old);
        for id in ids {
            tx.execute("DELETE FROM terminals WHERE id=?1", [id])?;
        }
        let mut old_for_task = tx.prepare(
            "SELECT id FROM terminals WHERE chat_id=?1 AND state != 'running'
             ORDER BY updated_at_ms DESC, id DESC LIMIT -1 OFFSET ?2",
        )?;
        let task_ids = old_for_task
            .query_map(
                params![entry.chat_id, MAX_SAVED_TERMINALS_PER_TASK as i64],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(old_for_task);
        for id in task_ids {
            tx.execute("DELETE FROM terminals WHERE id=?1", [id])?;
        }
        tx.commit()
    }

    pub(super) fn append(&self, id: &str, event: &TerminalEvent) -> rusqlite::Result<()> {
        let (seq, state, exit_code) = match event {
            TerminalEvent::Data { seq, .. } => (*seq, None, None),
            TerminalEvent::Exit { seq, exit_code, .. } => {
                (*seq, Some(TerminalProcessState::Exited), Some(*exit_code))
            }
        };
        let payload = serde_json::to_string(event)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let mut db = lock(&self.db);
        let tx = db.transaction()?;
        tx.execute(
            "INSERT INTO terminal_events (terminal_id, seq, payload, bytes) VALUES (?1, ?2, ?3, ?4)",
            params![id, seq as i64, payload, payload.len() as i64],
        )?;
        if let Some(state) = state {
            tx.execute(
                "UPDATE terminals SET last_seq=?2, state=?3, exit_code=?4, updated_at_ms=?5 WHERE id=?1",
                params![id, seq as i64, state_name(state), exit_code, now_ms()],
            )?;
        } else {
            tx.execute(
                "UPDATE terminals SET last_seq=?2, updated_at_ms=?3 WHERE id=?1",
                params![id, seq as i64, now_ms()],
            )?;
        }
        // Retain the newest complete frames. A single oversized frame is
        // discarded; the PTY remains live and can still send it to subscribers.
        let mut size: i64 = tx.query_row(
            "SELECT COALESCE(SUM(bytes), 0) FROM terminal_events WHERE terminal_id=?1",
            [id],
            |row| row.get(0),
        )?;
        let mut rows =
            tx.prepare("SELECT seq, bytes FROM terminal_events WHERE terminal_id=?1 ORDER BY seq")?;
        let events = rows
            .query_map([id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(rows);
        for (old_seq, bytes) in events {
            if size <= MAX_SAVED_OUTPUT_BYTES {
                break;
            }
            tx.execute(
                "DELETE FROM terminal_events WHERE terminal_id=?1 AND seq=?2",
                params![id, old_seq],
            )?;
            size -= bytes;
        }
        tx.commit()
    }

    pub(super) fn set_state(
        &self,
        id: &str,
        state: TerminalProcessState,
        exit_code: Option<i32>,
        last_seq: u64,
    ) -> rusqlite::Result<()> {
        lock(&self.db).execute(
            "UPDATE terminals SET state=?2, exit_code=?3, last_seq=?4, updated_at_ms=?5 WHERE id=?1",
            params![id, state_name(state), exit_code, last_seq as i64, now_ms()],
        )?;
        Ok(())
    }

    pub(super) fn list(
        &self,
        chat_id: Option<&str>,
    ) -> rusqlite::Result<Vec<TerminalInventoryEntry>> {
        let db = lock(&self.db);
        let sql =
            "SELECT id, chat_id, device_id, cwd, shell, state, exit_code, last_seq, updated_at_ms
                   FROM terminals WHERE (?1 IS NULL OR chat_id=?1)
                   ORDER BY updated_at_ms DESC, id DESC LIMIT 288";
        let mut statement = db.prepare(sql)?;
        statement
            .query_map([chat_id], |row| {
                Ok(TerminalInventoryEntry {
                    id: row.get(0)?,
                    chat_id: row.get(1)?,
                    device_id: row.get(2)?,
                    cwd: row.get(3)?,
                    shell: row.get(4)?,
                    state: parse_state(&row.get::<_, String>(5)?)?,
                    exit_code: row.get(6)?,
                    last_seq: row.get::<_, i64>(7)? as u64,
                    updated_at_ms: row.get(8)?,
                })
            })?
            .collect()
    }

    pub(super) fn exists(&self, id: &str) -> rusqlite::Result<bool> {
        lock(&self.db)
            .query_row("SELECT 1 FROM terminals WHERE id=?1", [id], |_| Ok(true))
            .optional()
            .map(|value| value.unwrap_or(false))
    }

    pub(super) fn replay(&self, id: &str, after_seq: u64) -> rusqlite::Result<Vec<TerminalEvent>> {
        let db = lock(&self.db);
        let mut statement = db.prepare(
            "SELECT payload FROM terminal_events WHERE terminal_id=?1 AND seq>?2 ORDER BY seq",
        )?;
        statement
            .query_map(params![id, after_seq as i64], |row| {
                let payload: String = row.get(0)?;
                serde_json::from_str(&payload).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })?
            .collect()
    }

    pub(super) fn delete_task(&self, chat_id: &str) -> rusqlite::Result<()> {
        lock(&self.db).execute("DELETE FROM terminals WHERE chat_id=?1", [chat_id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, chat_id: &str) -> TerminalInventoryEntry {
        TerminalInventoryEntry {
            id: id.into(),
            chat_id: chat_id.into(),
            device_id: "fixture-device".into(),
            cwd: "/tmp/fixture".into(),
            shell: "sh".into(),
            state: TerminalProcessState::Running,
            exit_code: None,
            last_seq: 0,
            updated_at_ms: now_ms(),
        }
    }

    #[test]
    fn bounded_history_reopens_as_interrupted_and_deletes_only_one_task() {
        let root = tempfile::tempdir().unwrap();
        let history = TerminalHistory::open(root.path()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        history.create(&entry("first", "task-a")).unwrap();
        history.create(&entry("second", "task-b")).unwrap();
        for seq in 1..=6 {
            history
                .append(
                    "first",
                    &TerminalEvent::Data {
                        seq,
                        data: "x".repeat(280_000),
                    },
                )
                .unwrap();
        }
        let saved = history.replay("first", 0).unwrap();
        assert!(saved.len() < 6 && !saved.is_empty());
        assert_eq!(
            saved.last(),
            Some(&TerminalEvent::Data {
                seq: 6,
                data: "x".repeat(280_000),
            })
        );
        assert_eq!(history.replay("first", 5).unwrap().len(), 1);
        drop(history);

        let reopened = TerminalHistory::open(root.path()).unwrap();
        assert_eq!(
            reopened.list(Some("task-a")).unwrap()[0].state,
            TerminalProcessState::Interrupted
        );
        reopened.delete_task("task-a").unwrap();
        assert!(reopened.list(Some("task-a")).unwrap().is_empty());
        assert_eq!(reopened.list(Some("task-b")).unwrap().len(), 1);
    }
}
