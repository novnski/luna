//! Engine side of provider-native coordination tasks (Claude workflows,
//! teammates, remote agents) and the read-only Claude external-session list.
//!
//! Task snapshots are control-plane state written straight into the chat
//! document's `meta`. They bypass the parked-session gate because a workflow
//! keeps reporting after its launching turn has finished, and they never
//! reopen a turn, write transcript content or journal a run event.

use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::watch;

use luna_doc::SessionDoc;
use luna_proto::{ClaudeExternalSessionsView, NativeTaskSnapshot};

/// Persist one adapter observation. Returns whether the task is still active
/// (the caller keeps a parked provider process alive while it is).
pub(crate) fn persist(doc: &SessionDoc, chat_id: &str, task: &NativeTaskSnapshot) -> bool {
    if let Err(error) = doc.upsert_native_task(task) {
        tracing::warn!(chat = %chat_id, task = %task.task_id, %error, "native task write failed");
    }
    !task.status.is_terminal()
}

/// The provider process for this run is gone: its in-process workflows and
/// teammates ended with it.
pub(crate) fn settle_session_ended(doc: &SessionDoc, chat_id: &str) {
    match doc.settle_native_tasks_session_ended() {
        Ok(true) => tracing::info!(chat = %chat_id, "native tasks settled: provider process ended"),
        Ok(false) => {}
        Err(error) => tracing::warn!(chat = %chat_id, %error, "native task settle failed"),
    }
}

/// `WatchNativeTasks`: the current ledger first, then each change. The
/// transcript watch re-sends on every document commit, so it doubles as the
/// change signal; unchanged ledgers are skipped.
pub(crate) fn watch_stream<T>(
    rx: watch::Receiver<T>,
    doc: Arc<SessionDoc>,
) -> BoxStream<'static, serde_json::Value>
where
    T: Send + Sync + 'static,
{
    futures::stream::unfold(
        (rx, doc, None::<luna_proto::NativeTaskLedger>),
        |(mut rx, doc, previous)| async move {
            loop {
                if previous.is_some() {
                    rx.changed().await.ok()?;
                }
                let ledger = doc.native_tasks();
                if previous.as_ref() == Some(&ledger) {
                    continue;
                }
                let value = serde_json::to_value(&ledger).ok()?;
                return Some((value, (rx, doc, Some(ledger))));
            }
        },
    )
    .boxed()
}

/// `ListClaudeExternalSessions`: the native supervisor's view, read-only.
pub(crate) async fn list_claude_external_sessions() -> ClaudeExternalSessionsView {
    luna_harness::ClaudeHarness::new().external_sessions().await
}
