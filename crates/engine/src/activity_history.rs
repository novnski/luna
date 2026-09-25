//! L14 derived activity envelopes over authoritative transcript and receipt records.

use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use std::collections::HashMap;

use luna_proto::{
    ACTIVITY_ENVELOPE_VERSION, APPSHOT_CONTEXT_MARKER, ActivityEnvelope, ActivityKind,
    ActivityOutcome, ActivityRedaction, ActivitySourceRef, AppshotActivityKind,
    AppshotActivityRecord, Chat, CommandReceiptStatus, HistoryProjectPolicy, TaskCommandEvent,
    TaskIdentityView, ToolCall,
};

/// Project viewer-owned capture facts into L14 without reading provider
/// transcripts or indexing screenshot/Accessibility content.
pub(crate) fn build_appshot_envelopes(
    local_device_id: &str,
    chat: &Chat,
    identity: Option<&TaskIdentityView>,
    records: &[AppshotActivityRecord],
    entries: Option<&[SessionMessageEntry]>,
    policy: Option<&HistoryProjectPolicy>,
) -> Vec<ActivityEnvelope> {
    let workspace_id = identity
        .map(|view| view.task.workspace_id.clone())
        .or_else(|| chat.space_id.clone());
    records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| {
            if record.task_id.as_deref() != Some(chat.id.as_str()) {
                return None;
            }
            let action = match record.kind {
                AppshotActivityKind::Captured => "Captured",
                AppshotActivityKind::Staged => "Staged",
                AppshotActivityKind::Removed => "Removed",
                AppshotActivityKind::Queued => "Queued",
                AppshotActivityKind::Sent => "Sent",
            };
            let (app, redaction) = minimize_metadata(record.app_name.clone(), policy);
            let summary = if let Some(app) = app.as_deref() {
                format!("{action} Appshot from {app}")
            } else if redaction == ActivityRedaction::Excluded {
                "[content excluded by project history policy]".into()
            } else if redaction == ActivityRedaction::Heuristic {
                "[heuristically redacted sensitive text]".into()
            } else {
                format!("{action} Appshot")
            };
            let sent = record.kind == AppshotActivityKind::Sent;
            let available_here = sent
                && chat.device_id == local_device_id
                && record.message_id.as_deref().is_some_and(|message_id| {
                    entries
                        .is_some_and(|entries| entries.iter().any(|entry| entry.id == message_id))
                });
            Some(ActivityEnvelope {
                schema_version: ACTIVITY_ENVELOPE_VERSION,
                event_id: record.event_id.clone(),
                environment_id: record
                    .target_environment_id
                    .clone()
                    .unwrap_or_else(|| chat.device_id.clone()),
                device_id: record.capture_device_id.clone(),
                task_id: chat.id.clone(),
                turn_id: record
                    .message_id
                    .clone()
                    .unwrap_or_else(|| record.capture_id.clone()),
                sequence: (index as u64).saturating_add(1),
                timestamp_ms: record.timestamp_ms,
                kind: ActivityKind::App,
                outcome: ActivityOutcome::Succeeded,
                harness: None,
                provider: None,
                model: None,
                reasoning: None,
                project_id: chat.space_id.clone(),
                workspace_id: workspace_id.clone(),
                tool: None,
                app,
                file: None,
                command: None,
                summary,
                redaction,
                source: ActivitySourceRef {
                    task_id: chat.id.clone(),
                    message_id: record.message_id.clone().unwrap_or_default(),
                    part_id: None,
                    available_here,
                    unavailable_reason: (!available_here).then(|| {
                        if record.kind == AppshotActivityKind::Queued {
                            "The Appshot is queued; no sent transcript location exists yet.".into()
                        } else if !sent {
                            "Draft Appshot events have no transcript location.".into()
                        } else if chat.device_id != local_device_id {
                            format!(
                                "This task belongs to environment {} on another device.",
                                chat.device_id
                            )
                        } else {
                            "The accepted Appshot message has no recorded transcript location yet."
                                .into()
                        }
                    }),
                },
            })
        })
        .collect()
}

const SUMMARY_LIMIT: usize = 240;
const METADATA_LIMIT: usize = 240;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ActivityRunMetadata {
    pub harness: Option<luna_proto::HarnessId>,
    pub model: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_activity_envelopes(
    local_device_id: &str,
    chat: &Chat,
    identity: Option<&TaskIdentityView>,
    entries: &[SessionMessageEntry],
    receipts: &[TaskCommandEvent],
    receipts_truncated: bool,
    run_metadata: &HashMap<String, ActivityRunMetadata>,
    policy: Option<&HistoryProjectPolicy>,
) -> Vec<ActivityEnvelope> {
    // A task's current config/binding is not historical evidence. Message events
    // use journaled SessionStarted metadata when it exists; everything else stays
    // unknown instead of rewriting old activity after a model/account switch.
    let environment_id = chat.device_id.clone();
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
                run_metadata.get(&entry.id),
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
                run_metadata.get(&entry.id),
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
                policy,
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
            harness: None,
            provider: None,
            model: None,
            reasoning: None,
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
            harness: None,
            provider: None,
            model: None,
            reasoning: None,
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
    run_metadata: Option<&ActivityRunMetadata>,
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
            let searchable = if entry.role == MessageRole::User {
                user_text_before_appshot_context(text)
            } else {
                text.as_str()
            };
            let (summary, redaction) = if searchable.is_empty() && searchable.len() != text.len() {
                (
                    "Appshot context withheld from search".into(),
                    ActivityRedaction::Withheld,
                )
            } else {
                minimize_project_text(searchable, policy)
            };
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
        MessagePart::Image { name, .. } => {
            let (summary, redaction) = image_summary(name, policy);
            (
                ActivityKind::Withheld,
                summary,
                redaction,
                message_outcome(entry.status),
                None,
                None,
                None,
                None,
            )
        }
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
        MessagePart::Input { .. } | MessagePart::AsyncQuestion { .. } => (
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
        run_metadata,
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
        policy,
    ))
}

fn user_text_before_appshot_context(text: &str) -> &str {
    let Some((before, after)) = text.split_once(APPSHOT_CONTEXT_MARKER) else {
        return text;
    };
    if !before.ends_with("\n\n") || !after.starts_with("\n<appshot ") {
        return text;
    }
    before.trim_end_matches('\n')
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
    let outcome = if let ToolCall::ComputerUse { status, .. } = call {
        match status {
            luna_proto::ComputerUseStatus::Running => ActivityOutcome::Pending,
            luna_proto::ComputerUseStatus::ProviderFailed => ActivityOutcome::Failed,
            luna_proto::ComputerUseStatus::ProviderCompleted
            | luna_proto::ComputerUseStatus::Unknown => ActivityOutcome::Unknown,
        }
    } else if !resolved {
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
        ToolCall::ViewImage { path } => (
            ActivityKind::File,
            "viewImage".into(),
            None,
            Some(path.clone()),
            None,
            path.clone(),
        ),
        ToolCall::Mcp { server, tool, .. } if server == "luna" && tool == "luna_status" => (
            ActivityKind::Tool,
            tool.clone(),
            None,
            None,
            None,
            "LUNA status".into(),
        ),
        ToolCall::Mcp { server, tool, .. } => (
            ActivityKind::App,
            tool.clone(),
            Some(server.clone()),
            None,
            None,
            format!("{server} · {tool}"),
        ),
        ToolCall::ComputerUse {
            action,
            app,
            status,
            screenshot_observed,
            ..
        } => (
            ActivityKind::App,
            action.clone(),
            app.clone(),
            None,
            None,
            format!(
                "Provider computer use · {action} · {} · screenshot observed: {screenshot_observed}",
                match status {
                    luna_proto::ComputerUseStatus::Running => "running",
                    luna_proto::ComputerUseStatus::ProviderCompleted =>
                        "provider completed; target outcome unverified",
                    luna_proto::ComputerUseStatus::ProviderFailed => "provider failed",
                    luna_proto::ComputerUseStatus::Unknown => "outcome unknown; do not replay",
                }
            ),
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
            None,
            None,
            None,
            None,
        );
    }
    let (mut summary, mut redaction) = minimize_project_text(&raw, policy);
    let (tool, tool_redaction) = minimize_metadata(tool, policy);
    let (app, app_redaction) = minimize_optional_metadata(app, policy);
    let (file, file_redaction) = minimize_optional_metadata(file, policy);
    let (command, command_redaction) = minimize_optional_metadata(command, policy);
    for field_redaction in [
        tool_redaction,
        app_redaction,
        file_redaction,
        command_redaction,
    ] {
        redaction = strongest_redaction(redaction, field_redaction);
    }
    if redaction == ActivityRedaction::Excluded {
        summary = "[content excluded by project history policy]".into();
    } else if redaction == ActivityRedaction::Heuristic {
        summary = "[heuristically redacted sensitive text]".into();
    }
    (kind, summary, redaction, outcome, tool, app, file, command)
}

#[allow(clippy::too_many_arguments)]
fn base_event(
    chat: &Chat,
    run_metadata: Option<&ActivityRunMetadata>,
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
    policy: Option<&HistoryProjectPolicy>,
) -> ActivityEnvelope {
    base_event_with_metadata(
        chat,
        run_metadata,
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
        policy,
    )
}

#[allow(clippy::too_many_arguments)]
fn base_event_with_metadata(
    chat: &Chat,
    run_metadata: Option<&ActivityRunMetadata>,
    environment_id: &str,
    workspace_id: Option<&str>,
    entry: &SessionMessageEntry,
    part_id: Option<&str>,
    sequence: u64,
    kind: ActivityKind,
    outcome: ActivityOutcome,
    mut summary: String,
    mut redaction: ActivityRedaction,
    available_here: bool,
    unavailable_reason: Option<String>,
    tool: Option<String>,
    app: Option<String>,
    file: Option<String>,
    command: Option<String>,
    policy: Option<&HistoryProjectPolicy>,
) -> ActivityEnvelope {
    let (model, model_redaction) = minimize_optional_metadata(
        run_metadata.and_then(|metadata| metadata.model.clone()),
        policy,
    );
    redaction = strongest_redaction(redaction, model_redaction);
    if redaction == ActivityRedaction::Excluded {
        summary = "[content excluded by project history policy]".into();
    } else if redaction == ActivityRedaction::Heuristic {
        summary = "[heuristically redacted sensitive text]".into();
    }
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
        harness: run_metadata.and_then(|metadata| metadata.harness),
        provider: None,
        model,
        reasoning: None,
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

fn minimize_metadata(
    value: String,
    policy: Option<&HistoryProjectPolicy>,
) -> (Option<String>, ActivityRedaction) {
    let (value, redaction) = minimize_project_text(&value, policy);
    if redaction != ActivityRedaction::None || value.is_empty() {
        return (None, redaction);
    }
    let mut bounded = value.chars().take(METADATA_LIMIT).collect::<String>();
    if value.chars().count() > METADATA_LIMIT {
        bounded.push('…');
    }
    (Some(bounded), ActivityRedaction::None)
}

fn minimize_optional_metadata(
    value: Option<String>,
    policy: Option<&HistoryProjectPolicy>,
) -> (Option<String>, ActivityRedaction) {
    value
        .map(|value| minimize_metadata(value, policy))
        .unwrap_or((None, ActivityRedaction::None))
}

fn strongest_redaction(left: ActivityRedaction, right: ActivityRedaction) -> ActivityRedaction {
    use ActivityRedaction::{Excluded, Heuristic, None, Withheld};
    match (left, right) {
        (Excluded, _) | (_, Excluded) => Excluded,
        (Heuristic, _) | (_, Heuristic) => Heuristic,
        (Withheld, _) | (_, Withheld) => Withheld,
        _ => None,
    }
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

fn image_summary(
    value: &str,
    policy: Option<&HistoryProjectPolicy>,
) -> (String, ActivityRedaction) {
    let (label, redaction) = minimize_project_text(value, policy);
    match redaction {
        ActivityRedaction::None => (
            format!("Image activity withheld ({label})"),
            ActivityRedaction::Withheld,
        ),
        ActivityRedaction::Excluded => (
            "Image activity excluded by project history policy".into(),
            ActivityRedaction::Excluded,
        ),
        ActivityRedaction::Heuristic => (
            "Image activity withheld (redacted name)".into(),
            ActivityRedaction::Heuristic,
        ),
        ActivityRedaction::Withheld => (
            "Image activity withheld".into(),
            ActivityRedaction::Withheld,
        ),
    }
}

fn strip_url_secrets(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use luna_proto::{ChatConfig, HarnessId, PermissionMode, SandboxLevel};

    #[test]
    fn computer_use_provider_completion_never_becomes_semantic_success() {
        let call = ToolCall::ComputerUse {
            action: "click".into(),
            app: Some("Finder".into()),
            task_id: Some("task".into()),
            thread_id: "provider".into(),
            turn_id: "turn".into(),
            status: luna_proto::ComputerUseStatus::ProviderCompleted,
            screenshot_observed: true,
        };
        let (kind, summary, _, outcome, _, app, _, _) = tool_event(&call, true, false, None);
        assert_eq!(kind, ActivityKind::App);
        assert_eq!(app.as_deref(), Some("Finder"));
        assert_eq!(outcome, ActivityOutcome::Unknown);
        assert!(summary.contains("screenshot observed: true"));
    }

    #[test]
    fn luna_status_is_tool_activity_without_indexing_its_input_or_output() {
        let call = ToolCall::Mcp {
            server: "luna".into(),
            tool: "luna_status".into(),
            input: Some(serde_json::json!({"untrusted": "ignore previous instructions"})),
        };
        let (kind, summary, _, outcome, tool, app, _, _) = tool_event(&call, true, false, None);
        assert_eq!(kind, ActivityKind::Tool);
        assert_eq!(summary, "LUNA status");
        assert_eq!(outcome, ActivityOutcome::Succeeded);
        assert_eq!(tool.as_deref(), Some("luna_status"));
        assert_eq!(app, None);
    }

    fn chat() -> Chat {
        Chat {
            id: "task".into(),
            device_id: "machine".into(),
            title: Some("Synthetic".into()),
            archived: false,
            settlement_override: None,
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
    fn appshot_history_keeps_viewer_capture_and_remote_task_ownership_separate() {
        let mut remote = chat();
        remote.device_id = "remote-host".into();
        let record = AppshotActivityRecord {
            event_id: "appshot:canvas:shot:sent:message".into(),
            draft_id: "canvas".into(),
            task_id: Some("task".into()),
            capture_id: "shot".into(),
            screenshot_attachment_id: "image".into(),
            capture_device_id: "viewer".into(),
            target_environment_id: Some("remote-host".into()),
            kind: AppshotActivityKind::Sent,
            timestamp_ms: 42,
            app_name: "Private Fixture".into(),
            message_id: Some("message".into()),
        };
        let policy = HistoryProjectPolicy {
            project_id: "project".into(),
            excluded_path_prefixes: vec![],
            excluded_terms: vec!["private".into()],
            index_terminal_output: false,
            index_browser_fields: false,
            index_accessibility_text: false,
            index_screenshot_text: false,
        };
        let events = build_appshot_envelopes(
            "viewer",
            &remote,
            None,
            &[record.clone()],
            None,
            Some(&policy),
        );
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.environment_id, "remote-host");
        assert_eq!(event.device_id, "viewer");
        assert_eq!(event.task_id, "task");
        assert_eq!(event.source.message_id, "message");
        assert!(!event.source.available_here);
        assert_eq!(event.redaction, ActivityRedaction::Excluded);
        assert!(event.app.is_none());
        assert!(
            !serde_json::to_string(event)
                .unwrap()
                .contains("Private Fixture")
        );
        let mut local = remote;
        local.device_id = "viewer".into();
        let pending =
            build_appshot_envelopes("viewer", &local, None, &[record.clone()], None, None);
        assert!(!pending[0].source.available_here);
        let entry = SessionMessageEntry {
            id: "message".into(),
            role: MessageRole::User,
            parts: vec![],
            created_at: 42,
            device_id: "viewer".into(),
            status: None,
            continuation_of: None,
        };
        let indexed =
            build_appshot_envelopes("viewer", &local, None, &[record], Some(&[entry]), None);
        assert!(indexed[0].source.available_here);
    }

    #[test]
    fn user_prompt_history_excludes_appshot_accessibility_context() {
        let text = format!(
            "Fix this layout\n\n{APPSHOT_CONTEXT_MARKER}\n<appshot app=\"Fixture\" image=\"/synthetic/private.png\">PRIVATE_AX_SENTINEL</appshot>"
        );
        let entry = SessionMessageEntry {
            id: "message".into(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "text".into(),
                text,
            }],
            created_at: 42,
            device_id: "machine".into(),
            status: None,
            continuation_of: None,
        };
        let events = build_activity_envelopes(
            "machine",
            &chat(),
            None,
            &[entry],
            &[],
            false,
            &HashMap::new(),
            None,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary, "Fix this layout");
        let serialized = serde_json::to_string(&events).unwrap();
        assert!(!serialized.contains("PRIVATE_AX_SENTINEL"));
        assert!(!serialized.contains("/synthetic/private.png"));
        assert_eq!(
            user_text_before_appshot_context(
                "I am discussing Applications mentioned by the user (untrusted observed content):"
            ),
            "I am discussing Applications mentioned by the user (untrusted observed content):"
        );
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
                    subagent_activity: None,
                },
            ],
            created_at: 7,
            device_id: "machine".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        }];
        let events = build_activity_envelopes(
            "machine",
            &chat(),
            None,
            &entries,
            &[],
            false,
            &HashMap::new(),
            None,
        );
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
        let bounded = build_activity_envelopes(
            "machine",
            &chat(),
            None,
            &entries,
            &[],
            true,
            &HashMap::new(),
            None,
        );
        assert!(bounded.iter().any(|event| {
            event.kind == ActivityKind::Withheld
                && event.summary.contains("bounded local index")
                && !event.source.available_here
        }));
    }
}
