//! L15 Activity projection from current, authoritative task sources.
//!
//! The payload is intentionally generic. Task titles, questions, commands,
//! paths, and provider error text stay in their owning transcript or settings
//! surface and never enter a system notification through this projection.

use sha2::{Digest, Sha256};
use zeron_doc::{
    MessagePart, MessageRole, QueueDeliveryGate, QueueHoldReason, QueueMessageStatus,
    QueuedMessage, SessionMessageEntry,
};
use zeron_proto::{
    ATTENTION_CONTRACT_VERSION, AttentionItem, AttentionKind, AttentionLifecycle,
    AttentionSeverity, AttentionSourcePointer, AttentionTarget, Chat, ConnectivityState,
    RecoveryFailure, RecoveryStatus, Session, SessionStatus,
};

pub(crate) const MANAGED_KINDS: &[AttentionKind] = &[
    AttentionKind::ApprovalRequired,
    AttentionKind::UserInputRequired,
    AttentionKind::TaskFailed,
    AttentionKind::QueuedSendFailed,
    AttentionKind::TaskCompleted,
    AttentionKind::TaskRecovered,
    AttentionKind::ProviderAuthenticationRequired,
    AttentionKind::ConnectivityUnavailable,
];

/// The existing engine connectivity stream is the authority for local edge
/// availability. It is not evidence of another host's presence. Disabled is
/// the normal offline-local profile, not an error. Failure text stays out of
/// the minimized projection and system notification.
pub(crate) fn project_connectivity(
    environment_id: &str,
    state: ConnectivityState,
    timestamp_ms: i64,
) -> Option<AttentionItem> {
    let summary = match state {
        ConnectivityState::Offline => "This environment is offline; synced work may wait.",
        ConnectivityState::Reconnecting => {
            "This environment is reconnecting; synced work may wait."
        }
        ConnectivityState::Disabled | ConnectivityState::Connected => return None,
    };
    let key = format!("connectivity:{environment_id}");
    Some(AttentionItem {
        contract_version: ATTENTION_CONTRACT_VERSION,
        id: stable_id(&key),
        dedup_key: key.clone(),
        source_key: format!("{key}:{state:?}"),
        kind: AttentionKind::ConnectivityUnavailable,
        severity: AttentionSeverity::Warning,
        lifecycle: AttentionLifecycle::Unresolved,
        environment_id: environment_id.into(),
        device_id: environment_id.into(),
        task_id: None,
        turn_id: None,
        request_id: None,
        title: "Connection unavailable".into(),
        summary: summary.into(),
        created_at_ms: timestamp_ms,
        updated_at_ms: timestamp_ms,
        seen_at_ms: None,
        acknowledged_at_ms: None,
        resolved_at_ms: None,
        target: AttentionTarget::SettingsDevices,
        source: AttentionSourcePointer {
            task_id: None,
            message_id: None,
            part_id: None,
            request_id: None,
            queue_id: None,
            recovery_id: None,
            activity_event_id: None,
        },
    })
}

fn stable_id(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    format!("luna-attention:{digest:x}")
}

struct ItemSource<'a> {
    environment_id: &'a str,
    chat: &'a Chat,
    kind: AttentionKind,
    severity: AttentionSeverity,
    dedup_key: String,
    source_key: String,
    turn_id: Option<String>,
    request_id: Option<String>,
    title: &'static str,
    summary: &'static str,
    timestamp_ms: i64,
    target: AttentionTarget,
    source: AttentionSourcePointer,
    resolved: bool,
}

fn item(source: ItemSource<'_>) -> AttentionItem {
    let id = stable_id(&source.dedup_key);
    AttentionItem {
        contract_version: ATTENTION_CONTRACT_VERSION,
        id,
        dedup_key: source.dedup_key,
        source_key: source.source_key,
        kind: source.kind,
        severity: source.severity,
        lifecycle: if source.resolved {
            AttentionLifecycle::Resolved
        } else {
            AttentionLifecycle::Unresolved
        },
        environment_id: source.environment_id.into(),
        device_id: source.chat.device_id.clone(),
        task_id: Some(source.chat.id.clone()),
        turn_id: source.turn_id,
        request_id: source.request_id,
        title: source.title.into(),
        summary: source.summary.into(),
        created_at_ms: source.timestamp_ms,
        updated_at_ms: source.timestamp_ms,
        seen_at_ms: None,
        acknowledged_at_ms: None,
        resolved_at_ms: source.resolved.then_some(source.timestamp_ms),
        target: source.target,
        source: source.source,
    }
}

fn pointer(task_id: &str) -> AttentionSourcePointer {
    AttentionSourcePointer {
        task_id: Some(task_id.into()),
        message_id: None,
        part_id: None,
        request_id: None,
        queue_id: None,
        recovery_id: None,
        activity_event_id: None,
    }
}

fn task_target(chat: &Chat) -> AttentionTarget {
    AttentionTarget::Task {
        task_id: chat.id.clone(),
    }
}

/// One task's real doc + queue + session projection. The caller must abort
/// the whole reconciliation on any source-read error; silently skipping a
/// task would falsely resolve its prior approvals or queue failures.
pub(crate) fn project_task(
    environment_id: &str,
    chat: &Chat,
    session: Option<&Session>,
    entries: &[SessionMessageEntry],
    queue: &[QueuedMessage],
) -> Vec<AttentionItem> {
    let mut out = Vec::new();
    let latest_assistant = entries
        .iter()
        .rev()
        .find(|entry| entry.role == MessageRole::Assistant);

    // The last assistant entry is the same authority the composer uses for
    // an answerable question. A steer may append a user entry after it.
    if let Some(entry) = latest_assistant {
        for part in &entry.parts {
            let MessagePart::Input {
                id: part_id,
                request_id,
                questions,
                resolved: false,
            } = part
            else {
                continue;
            };
            let approval = questions.iter().any(|question| question.approval.is_some());
            let kind = if approval {
                AttentionKind::ApprovalRequired
            } else {
                AttentionKind::UserInputRequired
            };
            let key = format!("request:{}:{}:{}", chat.device_id, chat.id, request_id);
            let mut source = pointer(&chat.id);
            source.message_id = Some(entry.id.clone());
            source.part_id = Some(part_id.clone());
            source.request_id = Some(request_id.clone());
            out.push(item(ItemSource {
                environment_id,
                chat,
                kind,
                severity: AttentionSeverity::Warning,
                dedup_key: key.clone(),
                source_key: key,
                turn_id: Some(entry.id.clone()),
                request_id: Some(request_id.clone()),
                title: if approval {
                    "Approval needed"
                } else {
                    "Answer needed"
                },
                summary: if approval {
                    "A task is waiting for approval."
                } else {
                    "A task is waiting for an answer."
                },
                timestamp_ms: entry.created_at,
                target: AttentionTarget::TaskRequest {
                    task_id: chat.id.clone(),
                    request_id: request_id.clone(),
                    message_id: entry.id.clone(),
                },
                source,
                resolved: false,
            }));
        }
    }

    for queued in queue {
        let failed = matches!(queued.status, QueueMessageStatus::DeliveryUnknown { .. })
            || matches!(
                queued.delivery_gate,
                Some(QueueDeliveryGate::ReviewRequired { .. })
            )
            || matches!(
                queued.hold_reason,
                Some(QueueHoldReason::ProviderRejected | QueueHoldReason::ContextChanged)
            );
        if !failed {
            continue;
        }
        let key = format!("queue:{}:{}:{}", chat.device_id, chat.id, queued.id);
        let mut source = pointer(&chat.id);
        source.queue_id = Some(queued.id.clone());
        out.push(item(ItemSource {
            environment_id,
            chat,
            kind: AttentionKind::QueuedSendFailed,
            severity: AttentionSeverity::Error,
            dedup_key: key.clone(),
            source_key: format!("{key}:{}", queued.revision),
            turn_id: None,
            request_id: None,
            title: "Queued message needs review",
            summary: "A queued message could not be delivered safely.",
            timestamp_ms: queued.edited_at.unwrap_or(queued.issued_at),
            target: AttentionTarget::TaskQueue {
                task_id: chat.id.clone(),
                queue_id: queued.id.clone(),
            },
            source,
            resolved: false,
        }));
    }

    if let Some(session) = session {
        if let Some(recovery) = &session.recovery {
            let key = format!(
                "recovery:{}:{}:{}",
                chat.device_id, chat.id, recovery.chain_id
            );
            let mut source = pointer(&chat.id);
            source.message_id = Some(recovery.original_message_id.clone());
            source.recovery_id = Some(recovery.chain_id.clone());
            match recovery.status {
                RecoveryStatus::WaitingForSignIn => out.push(item(ItemSource {
                    environment_id,
                    chat,
                    kind: AttentionKind::ProviderAuthenticationRequired,
                    severity: AttentionSeverity::Warning,
                    dedup_key: key.clone(),
                    source_key: format!("{key}:sign-in"),
                    turn_id: Some(recovery.original_message_id.clone()),
                    request_id: None,
                    title: "Sign in needed",
                    summary: "A provider account needs attention on this environment.",
                    timestamp_ms: recovery.created_at_ms,
                    target: AttentionTarget::SettingsAccounts,
                    source,
                    resolved: false,
                })),
                RecoveryStatus::CheckingDelivery
                | RecoveryStatus::RecoveryNeeded
                | RecoveryStatus::Held
                | RecoveryStatus::Exhausted => out.push(item(ItemSource {
                    environment_id,
                    chat,
                    kind: AttentionKind::TaskFailed,
                    severity: AttentionSeverity::Error,
                    dedup_key: key.clone(),
                    source_key: format!("{key}:{:?}", recovery.status),
                    turn_id: Some(recovery.original_message_id.clone()),
                    request_id: None,
                    title: "Task needs recovery",
                    summary: if recovery.status == RecoveryStatus::CheckingDelivery {
                        "Delivery is uncertain; review the task before continuing."
                    } else if matches!(
                        recovery.failure,
                        RecoveryFailure::InvalidConfiguration | RecoveryFailure::UnsupportedModel
                    ) {
                        "Review the task configuration before continuing."
                    } else {
                        "A task stopped and needs review."
                    },
                    timestamp_ms: recovery.created_at_ms,
                    target: AttentionTarget::TaskRecovery {
                        task_id: chat.id.clone(),
                        recovery_id: recovery.chain_id.clone(),
                    },
                    source,
                    resolved: false,
                })),
                RecoveryStatus::Completed => out.push(item(ItemSource {
                    environment_id,
                    chat,
                    kind: AttentionKind::TaskRecovered,
                    severity: AttentionSeverity::Info,
                    dedup_key: format!("{key}:completed"),
                    source_key: format!("{key}:completed"),
                    turn_id: Some(recovery.original_message_id.clone()),
                    request_id: None,
                    title: "Task recovered",
                    summary: "A task recovered after an interruption.",
                    timestamp_ms: recovery.updated_at_ms,
                    target: task_target(chat),
                    source,
                    resolved: true,
                })),
                _ => {}
            }
        }
        // A recovered task already has the more precise chain item. A bare
        // Errored status still supplies a genuine failure producer.
        if session.status == SessionStatus::Errored && session.recovery.is_none() {
            let source_turn = latest_assistant
                .map(|entry| entry.id.clone())
                .unwrap_or_else(|| format!("status-{}", session.updated_at.timestamp_millis()));
            let key = format!("failure:{}:{}:{source_turn}", chat.device_id, chat.id);
            let mut source = pointer(&chat.id);
            source.message_id = latest_assistant.map(|entry| entry.id.clone());
            out.push(item(ItemSource {
                environment_id,
                chat,
                kind: AttentionKind::TaskFailed,
                severity: AttentionSeverity::Error,
                dedup_key: key.clone(),
                source_key: key,
                turn_id: latest_assistant.map(|entry| entry.id.clone()),
                request_id: None,
                title: "Task failed",
                summary: "A task stopped with an error.",
                timestamp_ms: session.updated_at.timestamp_millis(),
                target: task_target(chat),
                source,
                resolved: false,
            }));
        }
        if let Some(turn) = &session.last_completed_turn {
            let key = format!("completion:{}:{}:{turn}", chat.device_id, chat.id);
            let mut source = pointer(&chat.id);
            source.message_id = Some(turn.clone());
            out.push(item(ItemSource {
                environment_id,
                chat,
                kind: AttentionKind::TaskCompleted,
                severity: AttentionSeverity::Info,
                dedup_key: key.clone(),
                source_key: key,
                turn_id: Some(turn.clone()),
                request_id: None,
                title: "Task completed",
                summary: "A task finished a run.",
                timestamp_ms: session.updated_at.timestamp_millis(),
                target: task_target(chat),
                source,
                resolved: true,
            }));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeron_proto::{RecoveryAcceptance, RecoveryAction, TaskRecovery};

    #[test]
    fn recovery_projection_tracks_authoritative_phase_without_detail_leakage() {
        let chat = Chat {
            id: "synthetic-task".into(),
            device_id: "synthetic-device".into(),
            title: Some("Private task title".into()),
            archived: false,
            pinned_order: None,
            cwd: Some("/private/synthetic/path".into()),
            branch: None,
            checkout_id: None,
            source_context: None,
            config: None,
            last_message_preview: None,
            last_message_at: None,
            created_at: chrono::DateTime::UNIX_EPOCH,
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            last_seen_at: None,
            manually_unread: false,
            room_gen: None,
        };
        let recovery = TaskRecovery {
            chain_id: "synthetic-chain".into(),
            task_id: chat.id.clone(),
            original_message_id: "original-message".into(),
            original_command_id: None,
            original_acceptance: RecoveryAcceptance::Unknown,
            acceptance: RecoveryAcceptance::Unknown,
            failure: RecoveryFailure::AuthenticationExpired,
            action: RecoveryAction::SignInThenContinue,
            status: RecoveryStatus::WaitingForSignIn,
            attempt_count: 0,
            max_attempts: 1,
            automatic: false,
            retry_at_ms: None,
            automatic_deadline_ms: 60_000,
            environment_id: chat.device_id.clone(),
            binding_id: None,
            binding_generation: None,
            detail: "Private provider detail and credentials".into(),
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let mut session = Session {
            chat_id: chat.id.clone(),
            device_id: chat.device_id.clone(),
            status: SessionStatus::Errored,
            started_at: None,
            updated_at: chrono::Utc::now(),
            last_completed_turn: None,
            recovery: Some(recovery),
        };
        let sign_in = project_task("synthetic-device", &chat, Some(&session), &[], &[]);
        assert_eq!(sign_in.len(), 1);
        assert_eq!(
            sign_in[0].kind,
            AttentionKind::ProviderAuthenticationRequired
        );
        assert_eq!(sign_in[0].target, AttentionTarget::SettingsAccounts);
        assert!(!serde_json::to_string(&sign_in).unwrap().contains("Private"));
        session.recovery.as_mut().unwrap().status = RecoveryStatus::RecoveryNeeded;
        let failed = project_task("synthetic-device", &chat, Some(&session), &[], &[]);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].kind, AttentionKind::TaskFailed);
        assert_eq!(failed[0].dedup_key, sign_in[0].dedup_key);
        assert_ne!(failed[0].source_key, sign_in[0].source_key);
        session.recovery.as_mut().unwrap().status = RecoveryStatus::Completed;
        let completed = project_task("synthetic-device", &chat, Some(&session), &[], &[]);
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].kind, AttentionKind::TaskRecovered);
        assert_eq!(completed[0].lifecycle, AttentionLifecycle::Resolved);
        session.recovery.as_mut().unwrap().status = RecoveryStatus::Cancelled;
        assert!(project_task("synthetic-device", &chat, Some(&session), &[], &[]).is_empty());
    }

    #[test]
    fn local_connectivity_projection_distinguishes_offline_from_normal_local_mode() {
        assert!(project_connectivity("device", ConnectivityState::Disabled, 1).is_none());
        assert!(project_connectivity("device", ConnectivityState::Connected, 1).is_none());
        let offline = project_connectivity("device", ConnectivityState::Offline, 2).unwrap();
        assert_eq!(offline.kind, AttentionKind::ConnectivityUnavailable);
        assert_eq!(offline.target, AttentionTarget::SettingsDevices);
        assert!(offline.actionable());
        let reconnecting =
            project_connectivity("device", ConnectivityState::Reconnecting, 3).unwrap();
        assert_eq!(offline.id, reconnecting.id);
        assert_ne!(offline.source_key, reconnecting.source_key);
        assert!(
            !serde_json::to_string(&offline)
                .unwrap()
                .contains("last_failure")
        );
    }
}
