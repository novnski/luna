//! L14 derived activity envelopes over authoritative transcript and receipt records.

use zeron_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use zeron_proto::{
    ACTIVITY_ENVELOPE_VERSION, ActivityEnvelope, ActivityKind, ActivityOutcome, ActivityRedaction,
    ActivitySourceRef, Chat, CommandReceiptStatus, HistoryProjectPolicy, TaskCommandEvent,
    TaskIdentityView, ToolCall,
};

const SUMMARY_LIMIT: usize = 240;

pub(crate) fn build_activity_envelopes(
    local_device_id: &str,
    chat: &Chat,
    identity: Option<&TaskIdentityView>,
    entries: &[SessionMessageEntry],
    receipts: &[TaskCommandEvent],
    receipts_truncated: bool,
    policy: Option<&HistoryProjectPolicy>,
) -> Vec<ActivityEnvelope> {
    let binding = identity.and_then(|view| view.current_binding.as_ref());
    let environment_id = binding
        .map(|binding| binding.environment_id.clone())
        .unwrap_or_else(|| chat.device_id.clone());
    let workspace_id = identity
        .map(|view| view.task.workspace_id.clone())
        .or_else(|| chat.space_id.clone());
    let available_here = chat.device_id == local_device_id;
    let unavailable_reason = (!available_here).then(|| {
        format!(
            "This task belongs to environment {} on another device.",
            environment_id
        )
    });
    let mut events = Vec::new();
    for (message_index, entry) in entries.iter().enumerate() {
        for (part_index, part) in entry.parts.iter().enumerate() {
            let sequence = (message_index as u64)
                .saturating_mul(1_000)
                .saturating_add(part_index as u64);
            if let Some(event) = part_event(
                chat,
                binding.map(|binding| binding.provider_instance_id.as_str()),
                &environment_id,
                workspace_id.as_deref(),
                entry,
                part,
                sequence,
                available_here,
                unavailable_reason.clone(),
                policy,
            ) {
                events.push(event);
            }
        }
        if entry.role == MessageRole::Assistant {
            events.push(base_event(
                chat,
                binding.map(|binding| binding.provider_instance_id.as_str()),
                &environment_id,
                workspace_id.as_deref(),
                entry,
                None,
                (message_index as u64)
                    .saturating_mul(1_000)
                    .saturating_add(999),
                ActivityKind::Outcome,
                message_outcome(entry.status),
                outcome_summary(entry.status).into(),
                ActivityRedaction::None,
                available_here,
                unavailable_reason.clone(),
            ));
        }
    }
    let receipt_base = (entries.len() as u64).saturating_mul(1_000);
    for (index, command) in receipts.iter().enumerate() {
        let receipt = &command.receipt;
        let transcript_message = receipt
            .correlation_id
            .as_ref()
            .filter(|message_id| entries.iter().any(|entry| entry.id == message_id.as_str()));
        let receipt_available = available_here && transcript_message.is_some();
        let receipt_unavailable = if !available_here {
            unavailable_reason.clone()
        } else if transcript_message.is_none() {
            Some("This command receipt has no recorded transcript location.".into())
        } else {
            None
        };
        let (summary, redaction) = minimize_project_text(
            &format!(
                "{}: {}{}",
                receipt.intent,
                receipt_status_label(receipt.status),
                receipt
                    .reason
                    .as_ref()
                    .map(|reason| format!(" — {}", reason.message))
                    .unwrap_or_default()
            ),
            policy,
        );
        events.push(ActivityEnvelope {
            schema_version: ACTIVITY_ENVELOPE_VERSION,
            event_id: format!("receipt:{}:{}", chat.id, command.command_id),
            environment_id: receipt
                .execution_environment
                .clone()
                .unwrap_or_else(|| environment_id.clone()),
            device_id: chat.device_id.clone(),
            task_id: chat.id.clone(),
            turn_id: receipt
                .correlation_id
                .clone()
                .unwrap_or_else(|| command.command_id.clone()),
            sequence: receipt_base.saturating_add(index as u64),
            timestamp_ms: receipt.committed_at_ms.unwrap_or(receipt.observed_at_ms),
            kind: ActivityKind::Outcome,
            outcome: receipt_outcome(receipt.status),
            harness: chat.config.as_ref().map(|config| config.harness),
            provider: binding.map(|binding| binding.provider_instance_id.clone()),
            model: chat.config.as_ref().and_then(|config| config.model.clone()),
            reasoning: chat.config.as_ref().and_then(|config| config.reasoning),
            project_id: chat.space_id.clone(),
            workspace_id: workspace_id.clone(),
            tool: None,
            app: None,
            file: None,
            command: None,
            summary,
            redaction,
            source: ActivitySourceRef {
                task_id: chat.id.clone(),
                message_id: transcript_message
                    .cloned()
                    .unwrap_or_else(|| command.command_id.clone()),
                part_id: None,
                available_here: receipt_available,
                unavailable_reason: receipt_unavailable,
            },
        });
    }
    if receipts_truncated {
        let timestamp_ms = receipts
            .last()
            .map(|event| event.receipt.observed_at_ms)
            .unwrap_or_else(|| chat.created_at.timestamp_millis());
        events.push(ActivityEnvelope {
            schema_version: ACTIVITY_ENVELOPE_VERSION,
            event_id: format!("receipt:{}:bounded-history", chat.id),
            environment_id,
            device_id: chat.device_id.clone(),
            task_id: chat.id.clone(),
            turn_id: "bounded-command-history".into(),
            sequence: receipt_base.saturating_add(receipts.len() as u64),
            timestamp_ms,
            kind: ActivityKind::Withheld,
            outcome: ActivityOutcome::Unknown,
            harness: chat.config.as_ref().map(|config| config.harness),
            provider: binding.map(|binding| binding.provider_instance_id.clone()),
            model: chat.config.as_ref().and_then(|config| config.model.clone()),
            reasoning: chat.config.as_ref().and_then(|config| config.reasoning),
            project_id: chat.space_id.clone(),
            workspace_id,
            tool: None,
            app: None,
            file: None,
            command: None,
            summary: "Older command receipts were withheld from the bounded local index".into(),
            redaction: ActivityRedaction::Withheld,
            source: ActivitySourceRef {
                task_id: chat.id.clone(),
                message_id: String::new(),
                part_id: None,
                available_here: false,
                unavailable_reason: Some(
                    "This marker summarizes command activity beyond the local index bound.".into(),
                ),
            },
        });
    }
    events
}

#[allow(clippy::too_many_arguments)]
fn part_event(
    chat: &Chat,
    provider: Option<&str>,
    environment_id: &str,
    workspace_id: Option<&str>,
    entry: &SessionMessageEntry,
    part: &MessagePart,
    sequence: u64,
    available_here: bool,
    unavailable_reason: Option<String>,
    policy: Option<&HistoryProjectPolicy>,
) -> Option<ActivityEnvelope> {
    let (kind, summary, redaction, outcome, tool, app, file, command) = match part {
        MessagePart::Text { text, .. } => {
            let (summary, redaction) = minimize_project_text(text, policy);
            (
                if entry.role == MessageRole::User {
                    ActivityKind::Prompt
                } else {
                    ActivityKind::Answer
                },
                summary,
                redaction,
                message_outcome(entry.status),
                None,
                None,
                None,
                None,
            )
        }
        // Deliberately exclude model reasoning from the search index.
        MessagePart::Reasoning { .. } => return None,
        MessagePart::Image { name, .. } => (
            ActivityKind::Withheld,
            format!("Image activity withheld ({})", safe_label(name)),
            ActivityRedaction::Withheld,
            message_outcome(entry.status),
            None,
            None,
            None,
            None,
        ),
        MessagePart::Error { message, .. } => {
            let (summary, redaction) = minimize_project_text(message, policy);
            (
                ActivityKind::Outcome,
                summary,
                redaction,
                ActivityOutcome::Failed,
                None,
                None,
                None,
                None,
            )
        }
        MessagePart::Input { .. } => (
            ActivityKind::Withheld,
            "Interactive input withheld from search".into(),
            ActivityRedaction::Withheld,
            message_outcome(entry.status),
            None,
            None,
            None,
            None,
        ),
        MessagePart::Tool {
            call,
            is_error,
            resolved,
            ..
        } => tool_event(call, *resolved, *is_error, policy),
    };
    Some(base_event_with_metadata(
        chat,
        provider,
        environment_id,
        workspace_id,
        entry,
        Some(part.id()),
        sequence,
        kind,
        outcome,
        summary,
        redaction,
        available_here,
        unavailable_reason,
        tool,
        app,
        file,
        command,
    ))
}

#[allow(clippy::type_complexity)]
fn tool_event(
    call: &ToolCall,
    resolved: bool,
    is_error: bool,
    policy: Option<&HistoryProjectPolicy>,
) -> (
    ActivityKind,
    String,
    ActivityRedaction,
    ActivityOutcome,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let outcome = if !resolved {
        ActivityOutcome::Pending
    } else if is_error {
        ActivityOutcome::Failed
    } else {
        ActivityOutcome::Succeeded
    };
    let (kind, tool, app, file, command, raw) = match call {
        ToolCall::Exec { command } => (
            ActivityKind::Command,
            "exec".into(),
            None,
            None,
            Some(command.clone()),
            command.clone(),
        ),
        ToolCall::ReadFile { path }
        | ToolCall::WriteFile { path, .. }
        | ToolCall::EditFile { path, .. } => (
            ActivityKind::File,
            tool_name(call),
            None,
            Some(path.clone()),
            None,
            path.clone(),
        ),
        ToolCall::ApplyPatch { path } => (
            ActivityKind::File,
            "applyPatch".into(),
            None,
            path.clone(),
            None,
            path.clone().unwrap_or_else(|| "patch".into()),
        ),
        ToolCall::Search { pattern, path } => (
            ActivityKind::Tool,
            "search".into(),
            None,
            path.clone(),
            None,
            format!(
                "Search {} in {}",
                pattern,
                path.as_deref().unwrap_or("workspace")
            ),
        ),
        ToolCall::Glob { pattern } => (
            ActivityKind::Tool,
            "glob".into(),
            None,
            None,
            None,
            format!("Glob {pattern}"),
        ),
        ToolCall::WebFetch { url, .. } => (
            ActivityKind::App,
            "webFetch".into(),
            Some("browser".into()),
            None,
            None,
            strip_url_secrets(url),
        ),
        ToolCall::WebSearch { query } => (
            ActivityKind::App,
            "webSearch".into(),
            Some("browser".into()),
            None,
            None,
            format!("Web search {query}"),
        ),
        ToolCall::Mcp { server, tool, .. } => (
            ActivityKind::App,
            tool.clone(),
            Some(server.clone()),
            None,
            None,
            format!("{server} · {tool}"),
        ),
        ToolCall::Unknown { name, .. } => (
            ActivityKind::Tool,
            name.clone(),
            None,
            None,
            None,
            name.clone(),
        ),
        ToolCall::Todo { .. } => (
            ActivityKind::Tool,
            "todo".into(),
            None,
            None,
            None,
            "Updated task plan".into(),
        ),
    };
    if file
        .as_deref()
        .is_some_and(|path| excluded_path(path, policy))
    {
        return (
            ActivityKind::Withheld,
            "File activity excluded by project history policy".into(),
            ActivityRedaction::Excluded,
            outcome,
            Some(tool),
            app,
            None,
            None,
        );
    }
    let (summary, redaction) = minimize_project_text(&raw, policy);
    let file = (redaction != ActivityRedaction::Heuristic)
        .then_some(file)
        .flatten();
    let command = (redaction != ActivityRedaction::Heuristic)
        .then_some(command)
        .flatten();
    (
        kind,
        summary,
        redaction,
        outcome,
        Some(tool),
        app,
        file,
        command,
    )
}

#[allow(clippy::too_many_arguments)]
fn base_event(
    chat: &Chat,
    provider: Option<&str>,
    environment_id: &str,
    workspace_id: Option<&str>,
    entry: &SessionMessageEntry,
    part_id: Option<&str>,
    sequence: u64,
    kind: ActivityKind,
    outcome: ActivityOutcome,
    summary: String,
    redaction: ActivityRedaction,
    available_here: bool,
    unavailable_reason: Option<String>,
) -> ActivityEnvelope {
    base_event_with_metadata(
        chat,
        provider,
        environment_id,
        workspace_id,
        entry,
        part_id,
        sequence,
        kind,
        outcome,
        summary,
        redaction,
        available_here,
        unavailable_reason,
        None,
        None,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn base_event_with_metadata(
    chat: &Chat,
    provider: Option<&str>,
    environment_id: &str,
    workspace_id: Option<&str>,
    entry: &SessionMessageEntry,
    part_id: Option<&str>,
    sequence: u64,
    kind: ActivityKind,
    outcome: ActivityOutcome,
    summary: String,
    redaction: ActivityRedaction,
    available_here: bool,
    unavailable_reason: Option<String>,
    tool: Option<String>,
    app: Option<String>,
    file: Option<String>,
    command: Option<String>,
) -> ActivityEnvelope {
    ActivityEnvelope {
        schema_version: ACTIVITY_ENVELOPE_VERSION,
        event_id: format!(
            "message:{}:{}:{}",
            chat.id,
            entry.id,
            part_id
                .map(|part_id| format!("part:{part_id}"))
                .unwrap_or_else(|| "outcome".into())
        ),
        environment_id: environment_id.into(),
        device_id: chat.device_id.clone(),
        task_id: chat.id.clone(),
        turn_id: entry.id.clone(),
        sequence,
        timestamp_ms: entry.created_at,
        kind,
        outcome,
        harness: chat.config.as_ref().map(|config| config.harness),
        provider: provider.map(str::to_owned),
        model: chat.config.as_ref().and_then(|config| config.model.clone()),
        reasoning: chat.config.as_ref().and_then(|config| config.reasoning),
        project_id: chat.space_id.clone(),
        workspace_id: workspace_id.map(str::to_owned),
        tool,
        app,
        file,
        command,
        summary,
        redaction,
        source: ActivitySourceRef {
            task_id: chat.id.clone(),
            message_id: entry.id.clone(),
            part_id: part_id.map(str::to_owned),
            available_here,
            unavailable_reason,
        },
    }
}

fn message_outcome(status: Option<MessageStatus>) -> ActivityOutcome {
    match status {
        Some(MessageStatus::Streaming) => ActivityOutcome::Pending,
        Some(MessageStatus::Complete) => ActivityOutcome::Succeeded,
        Some(MessageStatus::Aborted) => ActivityOutcome::Interrupted,
        None => ActivityOutcome::Unknown,
    }
}

fn outcome_summary(status: Option<MessageStatus>) -> &'static str {
    match status {
        Some(MessageStatus::Streaming) => "Assistant turn in progress",
        Some(MessageStatus::Complete) => "Assistant turn completed",
        Some(MessageStatus::Aborted) => "Assistant turn interrupted",
        None => "Assistant turn outcome unknown",
    }
}

fn receipt_outcome(status: CommandReceiptStatus) -> ActivityOutcome {
    match status {
        CommandReceiptStatus::Pending => ActivityOutcome::Pending,
        CommandReceiptStatus::Applied => ActivityOutcome::Succeeded,
        CommandReceiptStatus::Rejected | CommandReceiptStatus::Superseded => {
            ActivityOutcome::Interrupted
        }
        CommandReceiptStatus::Conflict | CommandReceiptStatus::Failed => ActivityOutcome::Failed,
    }
}

fn receipt_status_label(status: CommandReceiptStatus) -> &'static str {
    match status {
        CommandReceiptStatus::Pending => "pending",
        CommandReceiptStatus::Applied => "applied",
        CommandReceiptStatus::Rejected => "rejected",
        CommandReceiptStatus::Conflict => "conflict",
        CommandReceiptStatus::Superseded => "superseded",
        CommandReceiptStatus::Failed => "failed",
    }
}

fn tool_name(call: &ToolCall) -> String {
    match call {
        ToolCall::ReadFile { .. } => "readFile",
        ToolCall::WriteFile { .. } => "writeFile",
        ToolCall::EditFile { .. } => "editFile",
        _ => "tool",
    }
    .into()
}

fn excluded_path(path: &str, policy: Option<&HistoryProjectPolicy>) -> bool {
    let lower = path.replace('\\', "/").to_lowercase();
    if lower.split('/').any(|part| {
        part == ".env"
            || part.starts_with(".env.")
            || part == ".git"
            || part.contains("keychain")
            || part.contains("credential")
    }) {
        return true;
    }
    policy.is_some_and(|policy| {
        policy
            .excluded_path_prefixes
            .iter()
            .any(|prefix| lower.starts_with(&prefix.replace('\\', "/").to_lowercase()))
    })
}

fn minimize_text(value: &str) -> (String, ActivityRedaction) {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = normalized.to_lowercase();
    let sensitive = [
        "authorization:",
        "api_key",
        "apikey",
        "password=",
        "password:",
        "secret=",
        "secret:",
        "token=",
        "sk-proj-",
        "sk-ant-",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    if sensitive {
        return (
            "[heuristically redacted sensitive text]".into(),
            ActivityRedaction::Heuristic,
        );
    }
    let mut summary = normalized.chars().take(SUMMARY_LIMIT).collect::<String>();
    if normalized.chars().count() > SUMMARY_LIMIT {
        summary.push('…');
    }
    (summary, ActivityRedaction::None)
}

fn minimize_project_text(
    value: &str,
    policy: Option<&HistoryProjectPolicy>,
) -> (String, ActivityRedaction) {
    let lower = value.to_lowercase();
    if policy.is_some_and(|policy| {
        policy.excluded_terms.iter().any(|term| {
            let term = term.trim().to_lowercase();
            !term.is_empty() && lower.contains(&term)
        })
    }) {
        return (
            "[content excluded by project history policy]".into(),
            ActivityRedaction::Excluded,
        );
    }
    minimize_text(value)
}

fn safe_label(value: &str) -> String {
    let (summary, redaction) = minimize_text(value);
    if redaction == ActivityRedaction::None {
        summary
    } else {
        "redacted".into()
    }
}

fn strip_url_secrets(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use zeron_proto::{ChatConfig, HarnessId, PermissionMode, SandboxLevel};

    fn chat() -> Chat {
        Chat {
            id: "task".into(),
            device_id: "machine".into(),
            title: Some("Synthetic".into()),
            archived: false,
            pinned_order: None,
            cwd: Some("/synthetic/project".into()),
            branch: None,
            checkout_id: None,
            source_context: None,
            config: Some(ChatConfig {
                harness: HarnessId::Mock,
                provider_account_id: None,
                model: Some("fixture-model".into()),
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
                permission_mode: PermissionMode::Auto,
                effective_permission_mode: PermissionMode::Auto,
                permission_ceiling_source: None,
                permission_degradation_reason: None,
                permission_audit: Vec::new(),
            }),
            last_message_preview: None,
            last_message_at: None,
            created_at: Utc::now(),
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: Some("project".into()),
            last_seen_at: None,
            manually_unread: false,
            room_gen: Some(2),
        }
    }

    #[test]
    fn redacts_secrets_and_excludes_sensitive_paths_without_indexing_output() {
        let entries = vec![SessionMessageEntry {
            id: "turn".into(),
            role: MessageRole::Assistant,
            parts: vec![
                MessagePart::Text {
                    id: "text".into(),
                    text: "token=synthetic-secret".into(),
                },
                MessagePart::Tool {
                    id: "tool".into(),
                    call: ToolCall::ReadFile {
                        path: "/synthetic/project/.env".into(),
                    },
                    is_error: false,
                    resolved: true,
                    output: Some("must never be indexed".into()),
                    diff: None,
                    output_ref: None,
                    output_bytes: None,
                    diff_ref: None,
                    diff_stats: None,
                    subagent_ref: None,
                    subagent_status: None,
                    subagent_tail: None,
                },
            ],
            created_at: 7,
            device_id: "machine".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        }];
        let events = build_activity_envelopes("machine", &chat(), None, &entries, &[], false, None);
        assert!(events.iter().any(|event| {
            event.redaction == ActivityRedaction::Heuristic
                && !event.summary.contains("synthetic-secret")
        }));
        assert!(events.iter().any(|event| {
            event.redaction == ActivityRedaction::Excluded && event.file.is_none()
        }));
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("must never be indexed")
        );
        let bounded = build_activity_envelopes("machine", &chat(), None, &entries, &[], true, None);
        assert!(bounded.iter().any(|event| {
            event.kind == ActivityKind::Withheld
                && event.summary.contains("bounded local index")
                && !event.source.available_here
        }));
    }
}
