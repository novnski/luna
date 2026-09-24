//! L13 provider-history planning and recoverable conversation rewrite state.
//!
//! Filesystem state is deliberately absent. These helpers operate only on
//! LUNA transcript projections and provider-native history boundaries.

use std::collections::HashMap;

use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionDoc, SessionMessageEntry};
use luna_proto::{
    ConversationForkBoundaryKind, HarnessId, HistoryRecoveryView, ProviderForkBoundary,
    ProviderForkCapability, ProviderForkResult,
};
use luna_sync::DocsStore;
use serde::{Deserialize, Serialize};

use crate::EngineError;

const HISTORY_RECOVERY_INDEX_PREFIX: &str = "luna-history-recovery-index-v1";
const HISTORY_RECOVERY_SCHEMA_VERSION: u32 = 1;

pub fn capability_for(provider: HarnessId) -> ProviderForkCapability {
    match provider {
        HarnessId::Codex | HarnessId::Opencode => ProviderForkCapability::Checkpoint,
        // The current Claude adapter uses the CLI streaming wire and has no
        // non-turn session-management dependency. ACP/Pi implement new/load
        // but do not negotiate session/fork in this pinned client yet.
        HarnessId::ClaudeCode
        | HarnessId::Pi
        | HarnessId::Cursor
        | HarnessId::Devin
        | HarnessId::Grok
        | HarnessId::Hermes
        | HarnessId::Antigravity
        | HarnessId::Mock => ProviderForkCapability::None,
    }
}

pub fn unavailable_reason(provider: HarnessId) -> Option<&'static str> {
    match provider {
        HarnessId::ClaudeCode => Some(
            "Claude's pinned LUNA CLI adapter has no safe idle session-fork operation; use Start new task with context.",
        ),
        HarnessId::Pi => Some(
            "The pinned pi-acp client does not negotiate ACP session/fork; use Start new task with context.",
        ),
        HarnessId::Cursor
        | HarnessId::Devin
        | HarnessId::Grok
        | HarnessId::Hermes
        | HarnessId::Antigravity => Some(
            "This ACP adapter has no verified independent native fork in the current LUNA client; use Start new task with context.",
        ),
        HarnessId::Mock => Some("The synthetic Mock provider has no native history store."),
        HarnessId::Codex | HarnessId::Opencode => None,
    }
}

#[derive(Debug, Clone)]
pub struct HistoryPlan {
    pub entries: Vec<SessionMessageEntry>,
    pub checkpoints: HashMap<String, String>,
    pub provider_boundary: ProviderForkBoundary,
    pub selected_text: Option<String>,
    pub cutoff_message_id: Option<String>,
}

impl HistoryPlan {
    pub fn remapped_checkpoints(
        &self,
        result: &ProviderForkResult,
    ) -> Result<HashMap<String, String>, EngineError> {
        self.checkpoints
            .iter()
            .map(|(message_id, source_checkpoint)| {
                result
                    .checkpoint_map
                    .get(source_checkpoint)
                    .cloned()
                    .map(|fork_checkpoint| (message_id.clone(), fork_checkpoint))
                    .ok_or_else(|| {
                        EngineError::Other(format!(
                            "provider fork did not verify a checkpoint mapping for {source_checkpoint}"
                        ))
                    })
            })
            .collect()
    }

    pub fn tip_checkpoint(&self) -> Option<&str> {
        self.entries.iter().rev().find_map(|entry| {
            (entry.role == MessageRole::Assistant)
                .then(|| self.checkpoints.get(&entry.id).map(String::as_str))
                .flatten()
        })
    }
}

fn visible_text(entry: &SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.trim()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn completed_copy(entry: &SessionMessageEntry) -> Option<SessionMessageEntry> {
    if entry.role == MessageRole::System || entry.status != Some(MessageStatus::Complete) {
        return None;
    }
    let mut entry = entry.clone();
    entry.parts.retain(|part| match part {
        MessagePart::Input { resolved, .. } | MessagePart::Tool { resolved, .. } => *resolved,
        _ => true,
    });
    Some(entry)
}

pub fn plan_history(
    doc: &SessionDoc,
    provider: HarnessId,
    kind: ConversationForkBoundaryKind,
    message_id: Option<&str>,
) -> Result<HistoryPlan, EngineError> {
    let entries = doc.read_entries()?;
    let (end, boundary, selected_text, cutoff_message_id) = match kind {
        ConversationForkBoundaryKind::Tip => {
            let last = entries
                .iter()
                .rposition(|entry| {
                    entry.role == MessageRole::Assistant
                        && entry.status == Some(MessageStatus::Complete)
                })
                .map_or(0, |index| index + 1);
            if entries[last..]
                .iter()
                .any(|entry| matches!(entry.role, MessageRole::User | MessageRole::Assistant))
            {
                return Err(EngineError::Other(
                    "the source has incomplete conversational history at its tip".into(),
                ));
            }
            (last, ProviderForkBoundary::Tip, None, None)
        }
        ConversationForkBoundaryKind::BeforeUserMessage => {
            let message_id = message_id.ok_or_else(|| {
                EngineError::Other("before-user fork requires a message id".into())
            })?;
            let index = entries
                .iter()
                .position(|entry| entry.id == message_id && entry.role == MessageRole::User)
                .ok_or_else(|| {
                    EngineError::Other("selected user message is not eligible".into())
                })?;
            if let Some(previous) = entries[..index]
                .iter()
                .rev()
                .find(|entry| matches!(entry.role, MessageRole::User | MessageRole::Assistant))
                && (previous.role != MessageRole::Assistant
                    || doc.provider_checkpoint(&previous.id).is_none())
            {
                return Err(EngineError::Other(
                    "selected user message is inside a provider turn and has no exact native before-turn boundary"
                        .into(),
                ));
            }
            if provider == HarnessId::Opencode {
                return Err(EngineError::Other(
                    "OpenCode's current adapter cannot map a LUNA user row to its exclusive native message boundary; fork after a completed turn or at the tip"
                        .into(),
                ));
            }
            let assistant = entries[index + 1..]
                .iter()
                .find(|entry| matches!(entry.role, MessageRole::User | MessageRole::Assistant))
                .filter(|entry| {
                    entry.role == MessageRole::Assistant
                        && entry.status == Some(MessageStatus::Complete)
                })
                .ok_or_else(|| EngineError::Other("selected user turn never completed".into()))?;
            let checkpoint = doc.provider_checkpoint(&assistant.id).ok_or_else(|| {
                EngineError::Other("selected user turn has no provider checkpoint".into())
            })?;
            (
                index,
                ProviderForkBoundary::BeforeCheckpoint {
                    checkpoint_id: checkpoint,
                },
                Some(visible_text(&entries[index])),
                Some(message_id.to_string()),
            )
        }
        ConversationForkBoundaryKind::AfterCompletedTurn => {
            let message_id = message_id.ok_or_else(|| {
                EngineError::Other("completed-turn fork requires a message id".into())
            })?;
            let index = entries
                .iter()
                .position(|entry| {
                    entry.id == message_id
                        && entry.role == MessageRole::Assistant
                        && entry.status == Some(MessageStatus::Complete)
                })
                .ok_or_else(|| {
                    EngineError::Other("selected assistant turn is not complete".into())
                })?;
            let checkpoint = doc.provider_checkpoint(message_id).ok_or_else(|| {
                EngineError::Other("selected completed turn has no provider checkpoint".into())
            })?;
            (
                index + 1,
                ProviderForkBoundary::ThroughCheckpoint {
                    checkpoint_id: checkpoint,
                },
                None,
                Some(message_id.to_string()),
            )
        }
    };

    let copied = entries[..end]
        .iter()
        .filter_map(completed_copy)
        .collect::<Vec<_>>();
    let checkpoints = copied
        .iter()
        .filter_map(|entry| {
            doc.provider_checkpoint(&entry.id)
                .map(|checkpoint| (entry.id.clone(), checkpoint))
        })
        .collect();
    Ok(HistoryPlan {
        entries: copied,
        checkpoints,
        provider_boundary: boundary,
        selected_text,
        cutoff_message_id,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryRecoveryRecord {
    view: HistoryRecoveryView,
    snapshot_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryRecoveryIndex {
    schema_version: u32,
    records: Vec<HistoryRecoveryRecord>,
}

impl Default for HistoryRecoveryIndex {
    fn default() -> Self {
        Self {
            schema_version: HISTORY_RECOVERY_SCHEMA_VERSION,
            records: Vec::new(),
        }
    }
}

pub struct PreparedRecovery {
    pub index_id: String,
    pub index_bytes: Vec<u8>,
    pub snapshot_id: String,
    pub view: HistoryRecoveryView,
}

pub fn prepare_recovery(
    store: &DocsStore,
    view: HistoryRecoveryView,
) -> Result<PreparedRecovery, EngineError> {
    let index_id = format!("{HISTORY_RECOVERY_INDEX_PREFIX}:{}", view.task_id);
    let mut index = match store.load_snapshot(&index_id)? {
        Some(bytes) => serde_json::from_slice::<HistoryRecoveryIndex>(&bytes)
            .map_err(|error| EngineError::Other(format!("history recovery index: {error}")))?,
        None => HistoryRecoveryIndex::default(),
    };
    if index.schema_version != HISTORY_RECOVERY_SCHEMA_VERSION {
        return Err(EngineError::Other(
            "unsupported history recovery index version".into(),
        ));
    }
    let snapshot_id = format!("luna-history-recovery:{}", view.recovery_id);
    index.records.push(HistoryRecoveryRecord {
        view: view.clone(),
        snapshot_id: snapshot_id.clone(),
    });
    Ok(PreparedRecovery {
        index_id,
        index_bytes: serde_json::to_vec(&index)?,
        snapshot_id,
        view,
    })
}

pub fn list_recoveries(
    store: &DocsStore,
    task_id: &str,
) -> Result<Vec<HistoryRecoveryView>, EngineError> {
    let index_id = format!("{HISTORY_RECOVERY_INDEX_PREFIX}:{task_id}");
    let Some(bytes) = store.load_snapshot(&index_id)? else {
        return Ok(Vec::new());
    };
    let index = serde_json::from_slice::<HistoryRecoveryIndex>(&bytes)
        .map_err(|error| EngineError::Other(format!("history recovery index: {error}")))?;
    Ok(index
        .records
        .into_iter()
        .filter(|record| record.view.task_id == task_id)
        .map(|record| record.view)
        .collect())
}

pub fn load_recovery(
    store: &DocsStore,
    task_id: &str,
    recovery_id: &str,
) -> Result<(HistoryRecoveryView, Vec<u8>), EngineError> {
    let index_id = format!("{HISTORY_RECOVERY_INDEX_PREFIX}:{task_id}");
    let bytes = store
        .load_snapshot(&index_id)?
        .ok_or_else(|| EngineError::Other("history recovery does not exist".into()))?;
    let index = serde_json::from_slice::<HistoryRecoveryIndex>(&bytes)
        .map_err(|error| EngineError::Other(format!("history recovery index: {error}")))?;
    let record = index
        .records
        .into_iter()
        .find(|record| record.view.task_id == task_id && record.view.recovery_id == recovery_id)
        .ok_or_else(|| EngineError::Other("history recovery does not exist".into()))?;
    let snapshot = store
        .load_snapshot(&record.snapshot_id)?
        .ok_or_else(|| EngineError::Other("history recovery snapshot is unavailable".into()))?;
    Ok((record.view, snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use luna_doc::{MessagePart, MessageRole, MessageStatus};

    fn entry(id: &str, role: MessageRole, text: &str) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role,
            parts: vec![MessagePart::Text {
                id: format!("part-{id}"),
                text: text.into(),
            }],
            created_at: 1,
            device_id: "fixture".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        }
    }

    fn conversation() -> SessionDoc {
        let doc = SessionDoc::init("history-fixture").unwrap();
        doc.push_message(&entry("u1", MessageRole::User, "first"))
            .unwrap();
        doc.push_message(&entry("a1", MessageRole::Assistant, "answer one"))
            .unwrap();
        doc.set_provider_checkpoint("a1", "turn-1").unwrap();
        doc.push_message(&entry("u2", MessageRole::User, "second"))
            .unwrap();
        doc.push_message(&entry("a2", MessageRole::Assistant, "answer two"))
            .unwrap();
        doc.set_provider_checkpoint("a2", "turn-2").unwrap();
        doc
    }

    #[test]
    fn codex_before_user_is_exclusive_and_after_assistant_is_inclusive() {
        let doc = conversation();
        let before = plan_history(
            &doc,
            HarnessId::Codex,
            ConversationForkBoundaryKind::BeforeUserMessage,
            Some("u2"),
        )
        .unwrap();
        assert_eq!(
            before
                .entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["u1", "a1"]
        );
        assert_eq!(before.selected_text.as_deref(), Some("second"));
        assert_eq!(
            before.provider_boundary,
            ProviderForkBoundary::BeforeCheckpoint {
                checkpoint_id: "turn-2".into()
            }
        );

        let after = plan_history(
            &doc,
            HarnessId::Codex,
            ConversationForkBoundaryKind::AfterCompletedTurn,
            Some("a1"),
        )
        .unwrap();
        assert_eq!(
            after
                .entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["u1", "a1"]
        );
        assert_eq!(
            after.provider_boundary,
            ProviderForkBoundary::ThroughCheckpoint {
                checkpoint_id: "turn-1".into()
            }
        );
    }

    #[test]
    fn opencode_before_user_is_truthfully_unavailable() {
        let error = plan_history(
            &conversation(),
            HarnessId::Opencode,
            ConversationForkBoundaryKind::BeforeUserMessage,
            Some("u2"),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("exclusive native message boundary")
        );
    }

    #[test]
    fn capability_matrix_does_not_claim_unimplemented_native_forks() {
        assert_eq!(
            capability_for(HarnessId::Codex),
            ProviderForkCapability::Checkpoint
        );
        assert_eq!(
            capability_for(HarnessId::Opencode),
            ProviderForkCapability::Checkpoint
        );
        for provider in [
            HarnessId::ClaudeCode,
            HarnessId::Pi,
            HarnessId::Cursor,
            HarnessId::Mock,
        ] {
            assert_eq!(capability_for(provider), ProviderForkCapability::None);
            assert!(unavailable_reason(provider).is_some());
        }
    }

    #[test]
    fn tip_and_before_user_reject_unpaired_user_history() {
        let doc = conversation();
        doc.push_message(&entry("u3", MessageRole::User, "unanswered"))
            .unwrap();
        assert!(
            plan_history(
                &doc,
                HarnessId::Codex,
                ConversationForkBoundaryKind::Tip,
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("incomplete conversational history")
        );

        let broken = SessionDoc::init("broken-turn").unwrap();
        broken
            .push_message(&entry("u1", MessageRole::User, "first"))
            .unwrap();
        broken
            .push_message(&entry("u2", MessageRole::User, "second"))
            .unwrap();
        broken
            .push_message(&entry("a2", MessageRole::Assistant, "second answer"))
            .unwrap();
        broken.set_provider_checkpoint("a2", "turn-2").unwrap();
        assert!(
            plan_history(
                &broken,
                HarnessId::Codex,
                ConversationForkBoundaryKind::BeforeUserMessage,
                Some("u1"),
            )
            .unwrap_err()
            .to_string()
            .contains("never completed")
        );

        let steered = SessionDoc::init("steered-turn").unwrap();
        steered
            .push_message(&entry("u1", MessageRole::User, "first"))
            .unwrap();
        steered
            .push_message(&entry("a1", MessageRole::Assistant, "partial turn"))
            .unwrap();
        steered
            .push_message(&entry("u2", MessageRole::User, "steer"))
            .unwrap();
        steered
            .push_message(&entry("a2", MessageRole::Assistant, "turn end"))
            .unwrap();
        steered.set_provider_checkpoint("a2", "turn-1").unwrap();
        assert!(
            plan_history(
                &steered,
                HarnessId::Codex,
                ConversationForkBoundaryKind::BeforeUserMessage,
                Some("u2"),
            )
            .unwrap_err()
            .to_string()
            .contains("inside a provider turn")
        );
    }
}
