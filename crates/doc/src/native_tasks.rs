//! Provider-native coordination tasks (workflows, teammates, remote agents)
//! as one atomic JSON value in the session document's `meta` map, next to
//! `contextUsage`. They are observations of the provider's own task registry:
//! never transcript entries, and never a turn boundary.

use loro::LoroValue;

use crate::schema::{DocError, SessionDoc};

const META_KEY: &str = "nativeTasks";

impl SessionDoc {
    /// The last observed ledger. A missing or unreadable value reads as empty.
    pub fn native_tasks(&self) -> luna_proto::NativeTaskLedger {
        let Some(loro::ValueOrContainer::Value(LoroValue::String(value))) =
            self.doc().get_map("meta").get(META_KEY)
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
        let mut ledger = self.native_tasks();
        if !ledger.upsert(task.clone()) {
            return Ok(false);
        }
        self.write_native_tasks(&ledger)?;
        Ok(true)
    }

    /// The provider process ended: mark every still-active task stopped with
    /// `SessionEnded`, so a restart never shows a dead run as live.
    pub fn settle_native_tasks_session_ended(&self) -> Result<bool, DocError> {
        let mut ledger = self.native_tasks();
        if !ledger.settle_session_ended() {
            return Ok(false);
        }
        self.write_native_tasks(&ledger)?;
        Ok(true)
    }

    fn write_native_tasks(&self, ledger: &luna_proto::NativeTaskLedger) -> Result<(), DocError> {
        self.doc()
            .get_map("meta")
            .insert(META_KEY, serde_json::to_string(ledger)?)?;
        self.doc().commit();
        Ok(())
    }
}
