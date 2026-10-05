//! These rows share docs.sqlite3's consistent backup and project cleanup.
use super::*;
use luna_proto::{ProjectAction, ProjectCommandRun};

impl DocsStore {
    pub(super) fn command_writer(
        &self,
        permit: Option<&WritePermit>,
    ) -> Result<MutexGuard<'_, Connection>, StoreError> {
        if self.write_epoch.is_some() {
            self.require_writer(permit.ok_or(StoreError::AdmissionRequired)?)?;
        }
        Ok(self.snapshot_write_conn())
    }
    pub fn project_command_config(
        &self,
        project: &str,
        device: &str,
    ) -> Result<Vec<ProjectAction>, StoreError> {
        let bytes: Option<Vec<u8>> = self
            .conn()
            .query_row(
                "SELECT config FROM project_command_configs WHERE project_id=?1 AND device_id=?2",
                params![project, device],
                |r| r.get(0),
            )
            .optional()?;
        bytes
            .map(|b| serde_json::from_slice(&b))
            .transpose()
            .map(|v| v.unwrap_or_default())
            .map_err(Into::into)
    }
    pub fn save_project_command_config(
        &self,
        project: &str,
        device: &str,
        actions: &[ProjectAction],
        permit: Option<&WritePermit>,
    ) -> Result<(), StoreError> {
        self.command_writer(permit)?.execute("INSERT INTO project_command_configs(project_id,device_id,config) VALUES(?1,?2,?3) ON CONFLICT(project_id,device_id) DO UPDATE SET config=excluded.config", params![project,device,serde_json::to_vec(actions)?])?;
        Ok(())
    }
    pub fn project_command_runs(&self, device: &str) -> Result<Vec<ProjectCommandRun>, StoreError> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT receipt FROM project_command_runs WHERE device_id=?1 ORDER BY updated_at DESC",
        )?;
        let bytes = statement
            .query_map([device], |r| r.get::<_, Vec<u8>>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        bytes
            .into_iter()
            .map(|b| Ok(serde_json::from_slice(&b)?))
            .collect()
    }
    /// Compare-and-swap attempt fencing. None admits a new logical receipt once.
    pub fn save_project_command_run(
        &self,
        run: &ProjectCommandRun,
        expected_attempt: Option<u32>,
        permit: Option<&WritePermit>,
    ) -> Result<bool, StoreError> {
        let conn = self.command_writer(permit)?;
        let bytes = serde_json::to_vec(run)?;
        let changed = if let Some(expected) = expected_attempt {
            conn.execute("UPDATE project_command_runs SET receipt=?1,attempt=?2,updated_at=?3 WHERE id=?4 AND device_id=?5 AND attempt=?6", params![bytes,run.attempt,run.updated_at_ms,run.id,run.device_id,expected])?
        } else {
            conn.execute("INSERT OR IGNORE INTO project_command_runs(id,project_id,device_id,attempt,updated_at,receipt) VALUES(?1,?2,?3,?4,?5,?6)", params![run.id,run.project_id,run.device_id,run.attempt,run.updated_at_ms,bytes])?
        };
        Ok(changed == 1)
    }
    pub fn prune_project_commands(
        &self,
        device: &str,
        projects: &[String],
        permit: Option<&WritePermit>,
    ) -> Result<(), StoreError> {
        let mut conn = self.command_writer(permit)?;
        let tx = conn.transaction()?;
        for table in ["project_command_configs", "project_command_runs"] {
            let ids = {
                let mut statement = tx.prepare(&format!(
                    "SELECT DISTINCT project_id FROM {table} WHERE device_id=?1"
                ))?;
                statement
                    .query_map([device], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for id in ids {
                if !projects.contains(&id) {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE project_id=?1 AND device_id=?2"),
                        params![id, device],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}
