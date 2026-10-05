//! Provider-native coordination tasks (workflows, teammates, remote agents)
//! as one atomic JSON value in the session document's `meta` map, next to
//! `contextUsage`. They are observations of the provider's own task registry:
//! never transcript entries, and never a turn boundary.

use loro::LoroValue;

use crate::schema::{DocError, SessionDoc};

const META_KEY: &str = "nativeTasks";
// Separate additive key: older clients must still decode the workflow ledger.
const SHELL_META_KEY: &str = "nativeShellTasks";

impl SessionDoc {
    /// The last observed ledger. A missing or unreadable value reads as empty.
    pub fn native_tasks(&self) -> luna_proto::NativeTaskLedger {
        self.native_task_ledger(META_KEY)
    }

    pub fn native_shell_tasks(&self) -> luna_proto::NativeTaskLedger {
        self.native_task_ledger(SHELL_META_KEY)
    }

    fn native_task_ledger(&self, key: &str) -> luna_proto::NativeTaskLedger {
        let Some(loro::ValueOrContainer::Value(LoroValue::String(value))) =
            self.doc().get_map("meta").get(key)
        else {
            return Default::default();
        };
        serde_json::from_str(&value).unwrap_or_default()
    }

    /// Merge one adapter observation (terminal state is sticky). Commits only
    /// when the ledger changed; returns whether it did.
    pub fn upsert_native_task(
        &self,
        task: &luna_proto::NativeTaskSnapshot,
    ) -> Result<bool, DocError> {
        let key = if task.kind == luna_proto::NativeTaskKind::Shell {
            SHELL_META_KEY
        } else {
            META_KEY
        };
        let mut ledger = self.native_task_ledger(key);
        if !ledger.upsert(task.clone()) {
            return Ok(false);
        }
        self.write_native_tasks(key, &ledger)?;
        Ok(true)
    }

    /// The provider process ended: mark every still-active task stopped with
    /// `SessionEnded`, so a restart never shows a dead run as live.
    pub fn settle_native_tasks_session_ended(&self) -> Result<bool, DocError> {
        let mut changed = false;
        for key in [META_KEY, SHELL_META_KEY] {
            let mut ledger = self.native_task_ledger(key);
            if ledger.settle_session_ended() {
                self.write_native_tasks(key, &ledger)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    fn write_native_tasks(
        &self,
        key: &str,
        ledger: &luna_proto::NativeTaskLedger,
    ) -> Result<(), DocError> {
        self.doc()
            .get_map("meta")
            .insert(key, serde_json::to_string(ledger)?)?;
        self.doc().commit();
        Ok(())
    }
}
