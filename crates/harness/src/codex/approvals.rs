//! Exact native decision tables: client labels never manufacture a grant.
//!
//! Who settles a request that reached LUNA follows the engine-resolved mode
//! (`docs/pending-tasks/permission-modes/README.md`):
//! - Full answers it once, without a card and without a saved rule.
//! - Auto declines it. Codex's own reviewer (`approvalsReviewer:
//!   "auto_review"`) already judged everything it could; a request still
//!   reaching the client is one it handed back to a person (the reviewer's
//!   `None` fallback, a connector the user's config reviews as `user`, or a
//!   server's own approval prompt). Auto has no person to ask, so it fails
//!   closed with the reason shown — the outcome Codex gives a failed review.
//! - Legacy Ask parks the card for the user.
use super::{RequestInputFn, ServerRequestContext, new_message_id};
use crate::HarnessError;
use crate::jsonrpc::RpcClient;
use luna_proto::{
    AgentEvent, ApprovalActionKind, ApprovalChoice, ApprovalDetails, ApprovalNetworkContext,
    ApprovalRequest, HarnessId, PermissionMode, UserInputAnswer, UserInputQuestion,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) type PendingApprovals = HashMap<String, CancellationToken>;

/// A server request addressed to a native sub-agent (child provider thread)
/// that this connection observed starting a turn. Codex sends the child's
/// own `threadId`/`turnId` (`bespoke_event_handling.rs:785`); the request is
/// answered on the same JSON-RPC id like a root request.
#[derive(Clone)]
pub(super) struct SubagentRequest {
    pub(super) thread_id: String,
    pub(super) turn_id: String,
    /// Display label (agent path leaf, e.g. `alpha`).
    pub(super) label: String,
    /// Cancelled when the child turn completes or the child thread closes.
    pub(super) cancel: CancellationToken,
}

struct Choice {
    display: ApprovalChoice,
    response: Value,
}
fn choice(label: &str, scope: &str, description: &str, response: Value) -> Choice {
    Choice {
        display: ApprovalChoice {
            label: label.into(),
            scope: scope.into(),
            description: description.into(),
        },
        response,
    }
}
fn command_choice(value: &Value, params: &Value, network_index: &mut usize) -> Option<Choice> {
    let response = json!({"decision": value});
    match value.as_str() {
        Some("accept") => Some(choice(
            "Allow once",
            "request",
            "Approve only this command or terminal input.",
            response,
        )),
        Some("acceptForSession") => Some(choice(
            "Allow for session",
            "session",
            "Allow future prompts covered by this provider's session approval cache.",
            response,
        )),
        Some("decline") => Some(choice(
            "Deny",
            "none",
            "Reject this action; the agent may continue.",
            response,
        )),
        Some("cancel") => Some(choice(
            "Cancel turn",
            "none",
            "Reject this action and interrupt the provider turn.",
            response,
        )),
        Some(_) => None,
        None => {
            if let Some(rule) = value.pointer("/acceptWithExecpolicyAmendment/execpolicyAmendment")
            {
                let valid = rule.as_array().is_some_and(|parts| {
                    !parts.is_empty()
                        && parts
                            .iter()
                            .all(|part| part.as_str().is_some_and(|s| !s.is_empty()))
                });
                let matches_proposal = params
                    .get("proposedExecpolicyAmendment")
                    .filter(|v| !v.is_null())
                    .is_none_or(|proposed| proposed == rule);
                if valid && matches_proposal && value.as_object()?.len() == 1 {
                    return Some(choice(
                        "Allow proposed command rule",
                        "persistent policy",
                        "Approve and persist the exact proposed command prefix rule for future matching commands.",
                        response,
                    ));
                }
            }
            if let Some(rule) = value.pointer("/applyNetworkPolicyAmendment/networkPolicyAmendment")
            {
                let valid = valid_network_rule(rule);
                let matches_proposal = params
                    .get("proposedNetworkPolicyAmendments")
                    .and_then(Value::as_array)
                    .is_none_or(|proposed| proposed.contains(rule));
                if valid && matches_proposal && value.as_object()?.len() == 1 {
                    *network_index += 1;
                    let label = format!("Apply proposed network rule {network_index}");
                    let description = format!(
                        "Persist {} for host {} in the native network policy.",
                        rule["action"].as_str()?,
                        rule["host"].as_str()?
                    );
                    return Some(choice(&label, "persistent policy", &description, response));
                }
            }
            None
        }
    }
}
fn valid_network_rule(value: &Value) -> bool {
    value.as_object().is_some_and(|object| object.len() == 2)
        && value
            .get("host")
            .and_then(Value::as_str)
            .is_some_and(|host| !host.is_empty())
        && matches!(
            value.get("action").and_then(Value::as_str),
            Some("allow" | "deny")
        )
}
fn command_choices(params: &Value) -> Vec<Choice> {
    let fallback;
    let decisions = if let Some(value) = params.get("availableDecisions").filter(|v| !v.is_null()) {
        // An explicit empty/invalid capability list never falls back to wider grants.
        value.as_array().map(Vec::as_slice).unwrap_or_default()
    } else {
        fallback = vec![
            json!("decline"),
            json!("accept"),
            json!("acceptForSession"),
            json!("cancel"),
        ];
        &fallback
    };
    let mut network_index = 0;
    let mut choices: Vec<_> = decisions
        .iter()
        .filter_map(|decision| command_choice(decision, params, &mut network_index))
        .collect();
    if params.get("availableDecisions").is_none_or(Value::is_null) {
        if let Some(rule) = params
            .get("proposedExecpolicyAmendment")
            .filter(|v| !v.is_null())
            && let Some(item) = command_choice(
                &json!({"acceptWithExecpolicyAmendment": {"execpolicyAmendment": rule}}),
                params,
                &mut network_index,
            )
        {
            choices.push(item);
        }
        for rule in params
            .get("proposedNetworkPolicyAmendments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(item) = command_choice(
                &json!({"applyNetworkPolicyAmendment": {"networkPolicyAmendment": rule}}),
                params,
                &mut network_index,
            ) {
                choices.push(item);
            }
        }
    }
    // Malformed/unknown lists still have a fail-closed UI exit, never an allow.
    if choices.is_empty() {
        choices.push(choice(
            "Deny",
            "none",
            "No supported native allow decision was advertised.",
            json!({"decision":"decline"}),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    choices.retain(|choice| seen.insert(choice.display.label.clone()));
    choices
}
fn details(params: &Value) -> ApprovalDetails {
    let text = |key| params.get(key).and_then(Value::as_str).map(str::to_owned);
    let context = |key| params.get(key).filter(|value| !value.is_null()).cloned();
    ApprovalDetails {
        provider_thread_id: text("threadId"),
        item_id: text("itemId"),
        approval_id: text("approvalId"),
        kind: text("kind"),
        environment_id: text("environmentId"),
        cwd: text("cwd"),
        command: text("command").or_else(|| {
            params
                .get("command")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
        }),
        reason: text("reason"),
        command_actions: context("commandActions"),
        file_changes: context("changes"),
        grant_root: text("grantRoot"),
        network: params.get("networkApprovalContext").and_then(|network| {
            Some(ApprovalNetworkContext {
                host: network.get("host")?.as_str()?.into(),
                protocol: network.get("protocol")?.as_str()?.into(),
            })
        }),
        permissions: context("permissions").or_else(|| context("additionalPermissions")),
        proposed_exec_policy_amendment: context("proposedExecpolicyAmendment"),
        proposed_network_policy_amendments: params
            .get("proposedNetworkPolicyAmendments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        choices: Vec::new(),
        subagent: None,
        tool: None,
    }
}
fn valid_permission_profile(value: &Value) -> bool {
    let Some(profile) = value.as_object() else {
        return false;
    };
    if profile.is_empty()
        || profile
            .keys()
            .any(|key| !matches!(key.as_str(), "network" | "fileSystem"))
    {
        return false;
    }
    if let Some(network) = profile.get("network").filter(|v| !v.is_null())
        && !network.as_object().is_some_and(|n| {
            n.keys().all(|key| key == "enabled")
                && n.get("enabled")
                    .is_none_or(|v| v.is_null() || v.is_boolean())
        })
    {
        return false;
    }
    if let Some(files) = profile.get("fileSystem").filter(|v| !v.is_null()) {
        let Some(files) = files.as_object() else {
            return false;
        };
        if files.keys().any(|key| {
            !matches!(
                key.as_str(),
                "read" | "write" | "entries" | "globScanMaxDepth"
            )
        }) {
            return false;
        }
        for key in ["read", "write"] {
            if let Some(paths) = files.get(key).filter(|v| !v.is_null())
                && !paths
                    .as_array()
                    .is_some_and(|paths| paths.iter().all(Value::is_string))
            {
                return false;
            }
        }
        if files
            .get("entries")
            .filter(|v| !v.is_null())
            .is_some_and(|v| {
                !v.as_array()
                    .is_some_and(|entries| entries.iter().all(Value::is_object))
            })
        {
            return false;
        }
    }
    true
}
fn question(
    method: &str,
    params: &Value,
    id: &Value,
    policy: &luna_proto::PermissionPolicy,
    choices: &[Choice],
    subagent: Option<&str>,
) -> UserInputQuestion {
    let mut details = details(params);
    details.subagent = subagent.map(str::to_owned);
    let (header, summary, action_kind) = match method {
        "item/permissions/requestApproval" => (
            "Approve provider permissions",
            "Codex requests additional permissions.",
            ApprovalActionKind::Tool,
        ),
        "item/fileChange/requestApproval" => (
            "Approve file change",
            "Codex wants to modify files.",
            ApprovalActionKind::FileChange,
        ),
        _ if details.kind.as_deref() == Some("writeStdin") => (
            "Approve terminal input",
            "Codex wants to send input to an existing terminal.",
            ApprovalActionKind::Command,
        ),
        _ => (
            "Approve command",
            "Codex wants to run a command.",
            ApprovalActionKind::Command,
        ),
    };
    details.choices = choices
        .iter()
        .map(|choice| choice.display.clone())
        .collect();
    if method == "item/fileChange/requestApproval" {
        for choice in &mut details.choices {
            if choice.scope == "request" {
                choice.description = "Approve only these file changes.".into();
            }
            if choice.scope == "session" {
                choice.description = "Approve these changes and future changes to the same files for this session. Any proposed write root remains provider-controlled.".into();
            }
        }
    }
    // Retain advertised policy payloads even when the server omits the separate
    // proposed-* fields. Metadata is display-only; responses stay in this module.
    for choice in choices {
        if let Some(rule) = choice
            .response
            .pointer("/decision/acceptWithExecpolicyAmendment/execpolicyAmendment")
        {
            details.proposed_exec_policy_amendment = Some(rule.clone());
        }
        if let Some(rule) = choice
            .response
            .pointer("/decision/applyNetworkPolicyAmendment/networkPolicyAmendment")
            && !details.proposed_network_policy_amendments.contains(rule)
        {
            details
                .proposed_network_policy_amendments
                .push(rule.clone());
        }
    }
    // Legacy question text remains readable by clients that ignore details.
    let legacy_context = details
        .command
        .clone()
        .or_else(|| {
            details
                .file_changes
                .as_ref()
                .and_then(Value::as_array)
                .map(|changes| {
                    changes
                        .iter()
                        .filter_map(|change| change.get("path").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
        })
        .unwrap_or_default();
    let target = if policy.machine_name.is_empty() {
        "this machine"
    } else {
        &policy.machine_name
    };
    let (header, summary) = match subagent {
        Some(label) => (
            format!("{header} · sub-agent {label}"),
            format!(
                "Codex sub-agent {label} {}",
                summary.trim_start_matches("Codex ")
            ),
        ),
        None => (header.to_owned(), summary.to_owned()),
    };
    UserInputQuestion {
        id: new_message_id(),
        header,
        question: format!("{summary} {legacy_context} Target: {target}."),
        options: choices
            .iter()
            .map(|choice| choice.display.label.clone())
            .collect(),
        multi_select: false,
        mcp: None,
        approval: Some(ApprovalRequest {
            provider_request_id: id.to_string(),
            provider: HarnessId::Codex,
            turn_id: params
                .get("turnId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            machine_name: policy.machine_name.clone(),
            action_kind,
            summary,
            requested_scope: policy.scope,
            // Codex v2 approvals carry no expiry: wait for the user, Codex's
            // `serverRequest/resolved`, turn end/interrupt or session close.
            expires_at_ms: luna_proto::APPROVAL_NO_EXPIRY,
            supports_task_allowance: choices
                .iter()
                .any(|choice| choice.display.scope == "session"),
            details: Some(details),
        }),
        input: None,
    }
}

/// Strict for permissions; old command/file peers may omit routing fields.
/// A supplied foreign/stale ID is always rejected, including Full mode. A
/// sub-agent request must name its child thread and that child's live turn.
fn exact_context(params: &Value, context: &ServerRequestContext<'_>, strict: bool) -> bool {
    let thread = params.get("threadId").and_then(Value::as_str);
    let turn = params.get("turnId").and_then(Value::as_str);
    let routed = match &context.subagent {
        Some(child) => {
            !child.cancel.is_cancelled()
                && thread == Some(child.thread_id.as_str())
                && turn == Some(child.turn_id.as_str())
        }
        None => {
            !context.computer_use_turn_cancel.is_cancelled()
                && context.active_turn_id.is_some()
                && (thread == Some(context.thread_id)
                    || (!strict && params.get("threadId").is_none()))
                && (turn == context.active_turn_id || (!strict && params.get("turnId").is_none()))
        }
    };
    !context.interrupt.is_cancelled()
        && routed
        && (!strict
            || context.runtime_contract.is_some_and(|contract| {
                !contract.context.thread_id.is_empty() && !contract.context.machine_id.is_empty()
            }))
        && params
            .get("itemId")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
}

/// The cancellation that closes a parked answer when its turn is over: the
/// child's turn for a sub-agent request, otherwise the root turn.
fn turn_cancel(context: &ServerRequestContext<'_>) -> CancellationToken {
    context
        .subagent
        .as_ref()
        .map(|child| child.cancel.clone())
        .unwrap_or_else(|| context.computer_use_turn_cancel.clone())
}

pub(super) fn handle(
    client: &RpcClient,
    id: Value,
    method: &str,
    params: &Value,
    context: &ServerRequestContext<'_>,
    pending: &mut PendingApprovals,
) {
    let permissions = method == "item/permissions/requestApproval";
    let denied = if permissions {
        json!({"permissions":{}, "scope":"turn"})
    } else {
        json!({"decision":"decline"})
    };
    if let Some(previous) = pending.remove(&id.to_string()) {
        previous.cancel();
        client.respond(&id, denied);
        return;
    }
    if !exact_context(params, context, permissions) || params.to_string().len() > 64 * 1024 {
        client.respond(&id, denied);
        return;
    }
    let choices = if permissions {
        let Some(profile) = params
            .get("permissions")
            .filter(|value| valid_permission_profile(value))
        else {
            client.respond(&id, denied);
            return;
        };
        vec![
            choice(
                "Deny",
                "none",
                "Grant no additional permissions.",
                denied.clone(),
            ),
            choice(
                "Allow for turn",
                "turn",
                "Grant exactly these permissions until this provider turn ends.",
                json!({"permissions":profile, "scope":"turn"}),
            ),
            choice(
                "Allow for session",
                "session",
                "Grant exactly these permissions for this provider session.",
                json!({"permissions":profile, "scope":"session"}),
            ),
        ]
    } else {
        command_choices(params)
    };
    // Full never parks a prompt. Additional permissions are scoped to this
    // exact turn; ordinary requests remain once-only, never a rule.
    let full = context.permission_policy.effective_mode == PermissionMode::Full;
    if full && permissions {
        client.respond(&id, choices[1].response.clone());
        return;
    }
    // Auto: Codex's reviewer handed this back to a person (see the module
    // docs). Checked before the legacy `auto_approve` bit: Auto is a review,
    // never a blanket approval.
    if context.permission_policy.effective_mode == PermissionMode::Auto {
        decline_without_review(
            client,
            &id,
            denied,
            context.events,
            &request_subject(method, params),
            context.subagent.as_ref().map(|child| child.label.as_str()),
        );
        return;
    }
    if (context.auto_approve || full) && !permissions {
        client.respond(
            &id,
            choices
                .iter()
                .find(|choice| choice.response == json!({"decision":"accept"}))
                .map(|choice| choice.response.clone())
                .unwrap_or(denied),
        );
        return;
    }
    let mut question = question(
        method,
        params,
        &id,
        &context.permission_policy,
        &choices,
        context.subagent.as_ref().map(|child| child.label.as_str()),
    );
    if permissions && let Some(contract) = context.runtime_contract {
        question.question = format!(
            "LUNA task {}. {}",
            contract.context.thread_id, question.question
        );
    }
    park(
        client,
        id,
        question,
        choices,
        denied,
        context,
        turn_cancel(context),
        pending,
    );
}

/// Park one approval until the user answers. Codex expresses no deadline, so
/// none is invented here: the answer receiver closes (no response is sent) on
/// `serverRequest/resolved`, Stop, or the end of the owning turn.
#[allow(clippy::too_many_arguments)]
fn park(
    client: &RpcClient,
    id: Value,
    question: UserInputQuestion,
    choices: Vec<Choice>,
    denied: Value,
    context: &ServerRequestContext<'_>,
    turn_cancel: CancellationToken,
    pending: &mut PendingApprovals,
) {
    let cancel = CancellationToken::new();
    pending.insert(id.to_string(), cancel.clone());
    let client = client.clone();
    let request_input: Arc<RequestInputFn> = Arc::clone(context.request_input);
    let interrupt = context.interrupt.clone();
    tokio::spawn(async move {
        let receiver = (request_input)(vec![question.clone()]);
        let answers = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = interrupt.cancelled() => {cancel.cancel(); return;},
            _ = turn_cancel.cancelled() => {cancel.cancel(); return;},
            result = receiver => result.ok(),
        };
        let response =
            if !interrupt.is_cancelled() && !turn_cancel.is_cancelled() && !cancel.is_cancelled() {
                answers
                    .as_ref()
                    .filter(|answers| answers.len() == 1)
                    .and_then(|answers| exact_answer(&question, answers))
                    .and_then(|label| choices.iter().find(|choice| choice.display.label == label))
                    .map(|choice| choice.response.clone())
                    .unwrap_or(denied)
            } else {
                denied
            };
        client.respond(&id, response);
        cancel.cancel();
    });
}

/// Codex MCP/app tool-call approval (`_meta.codex_approval_kind:
/// "mcp_tool_call"`, `core/src/mcp_tool_call.rs`). Choices and responses
/// mirror the Codex TUI (`tui/src/bottom_pane/mcp_server_elicitation.rs`):
/// persistence is offered only when `_meta.persist` advertises it.
pub(super) fn handle_tool_approval(
    client: &RpcClient,
    id: Value,
    params: &Value,
    turn_id: &str,
    approval: super::elicitation::ToolApproval,
    context: &ServerRequestContext<'_>,
    pending: &mut PendingApprovals,
) {
    let declined = json!({"action":"decline"});
    let accept = |persist: Option<&str>| match persist {
        Some(persist) => json!({"action":"accept","content":null,"_meta":{"persist":persist}}),
        None => json!({"action":"accept","content":null}),
    };
    // The caller already validated the live thread/turn and duplicate ID.
    // Full authorizes this invocation only, without changing connector policy.
    if context.permission_policy.effective_mode == PermissionMode::Full {
        client.respond(&id, accept(None));
        return;
    }
    // Auto: the reviewer handed this tool call back to a person. The user's
    // saved connector policy is left exactly as configured.
    if context.permission_policy.effective_mode == PermissionMode::Auto {
        let actor = approval
            .context
            .connector
            .as_deref()
            .unwrap_or(approval.context.server.as_str());
        let tool = approval.context.tool.as_deref().unwrap_or("a tool");
        decline_without_review(
            client,
            &id,
            declined,
            context.events,
            &format!("{actor}'s {tool} call"),
            context.subagent.as_ref().map(|child| child.label.as_str()),
        );
        return;
    }
    let mut choices = vec![choice(
        "Allow once",
        "request",
        "Run the tool and continue.",
        accept(None),
    )];
    if approval.persist_session {
        choices.push(choice(
            "Allow for session",
            "session",
            "Run the tool and remember this choice for this session.",
            accept(Some("session")),
        ));
    }
    if approval.persist_always {
        choices.push(choice(
            "Always allow",
            "persistent policy",
            "Run the tool and remember this choice for future tool calls.",
            accept(Some("always")),
        ));
    }
    choices.push(choice(
        "Deny",
        "none",
        "Decline this tool call; the agent may continue.",
        declined.clone(),
    ));
    choices.push(choice(
        "Cancel",
        "none",
        "Cancel this tool call.",
        json!({"action":"cancel"}),
    ));
    let subagent = context.subagent.as_ref().map(|child| child.label.clone());
    let actor = approval
        .context
        .connector
        .clone()
        .unwrap_or_else(|| approval.context.server.clone());
    let tool = approval
        .context
        .tool
        .clone()
        .unwrap_or_else(|| "a tool".into());
    let mut header = "Approve MCP tool".to_owned();
    if let Some(label) = &subagent {
        header.push_str(&format!(" · sub-agent {label}"));
    }
    let question = UserInputQuestion {
        id: new_message_id(),
        header,
        question: approval.message.clone(),
        options: choices
            .iter()
            .map(|choice| choice.display.label.clone())
            .collect(),
        multi_select: false,
        mcp: None,
        input: None,
        approval: Some(ApprovalRequest {
            provider_request_id: id.to_string(),
            provider: HarnessId::Codex,
            turn_id: Some(turn_id.to_owned()),
            machine_name: context.permission_policy.machine_name.clone(),
            action_kind: ApprovalActionKind::Tool,
            summary: format!("{actor} wants to run {tool}"),
            requested_scope: context.permission_policy.scope,
            expires_at_ms: luna_proto::APPROVAL_NO_EXPIRY,
            supports_task_allowance: approval.persist_session,
            details: Some(ApprovalDetails {
                provider_thread_id: params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                kind: Some("mcp_tool_call".into()),
                choices: choices
                    .iter()
                    .map(|choice| choice.display.clone())
                    .collect(),
                subagent,
                tool: Some(approval.context),
                ..Default::default()
            }),
        }),
    };
    park(
        client,
        id,
        question,
        choices,
        declined,
        context,
        turn_cancel(context),
        pending,
    );
}

/// Auto's answer to a request Codex handed back to a person: `response` (the
/// native decline) goes out at once, never a parked card, and the reason
/// becomes a visible transcript error. The user's saved provider policy is
/// not touched.
pub(super) fn decline_without_review(
    client: &RpcClient,
    id: &Value,
    response: Value,
    events: &mpsc::Sender<Result<AgentEvent, HarnessError>>,
    subject: &str,
    subagent: Option<&str>,
) {
    client.respond(id, response);
    let by = subagent
        .map(|label| format!(" for sub-agent {}", bounded(label, 128)))
        .unwrap_or_default();
    let message = format!(
        "Auto access declined {}{by}. It needed a person's approval, and Auto never asks, \
         so it did not run. Switch this chat to Full access to allow it.",
        bounded(subject, 512)
    );
    send_event(events, AgentEvent::Error { message });
}

/// Deliver an event from a synchronous request handler without reordering it
/// behind later frames; only a full channel defers it to a task.
fn send_event(events: &mpsc::Sender<Result<AgentEvent, HarnessError>>, event: AgentEvent) {
    if let Err(mpsc::error::TrySendError::Full(event)) = events.try_send(Ok(event)) {
        let events = events.clone();
        tokio::spawn(async move {
            let _ = events.send(event).await;
        });
    }
}

/// What a command/file/permissions request asks for, in words.
fn request_subject(method: &str, params: &Value) -> String {
    let details = details(params);
    match method {
        "item/permissions/requestApproval" => "a request for additional permissions".into(),
        "item/fileChange/requestApproval" => "a file change".into(),
        _ if details.kind.as_deref() == Some("writeStdin") => "terminal input".into(),
        _ => details
            .command
            .filter(|command| !command.trim().is_empty())
            .map(|command| format!("the command `{}`", bounded(&command, 256)))
            .unwrap_or_else(|| "a command".into()),
    }
}

/// Codex's `item/autoApprovalReview/completed` (app-server v2
/// `ItemGuardianApprovalReviewCompletedNotification`): the reviewer's final
/// decision on one approval. A denial or timeout is fail-closed in Codex
/// already (the model receives the rationale); this is its visible reason.
/// Approvals and aborted reviews (Stop, a cancelled turn) add nothing.
pub(super) fn auto_review_outcome(params: &Value, root_thread: &str) -> Option<AgentEvent> {
    let review = params.get("review")?;
    let status = review.get("status")?.as_str()?;
    if !matches!(status, "denied" | "timedOut") {
        return None;
    }
    let subject = review_action_subject(params.get("action"));
    let by = params
        .get("threadId")
        .and_then(Value::as_str)
        .filter(|thread| !thread.is_empty() && *thread != root_thread)
        .map(|_| " for a sub-agent")
        .unwrap_or("");
    let rationale = review
        .get("rationale")
        .and_then(Value::as_str)
        .map(|rationale| bounded(rationale.trim().trim_end_matches('.'), 1024))
        .filter(|rationale| !rationale.is_empty());
    let outcome = match (status, rationale) {
        ("timedOut", _) => format!("timed out on {subject}{by}"),
        (_, Some(rationale)) => format!("denied {subject}{by}: {rationale}"),
        (_, None) => format!("denied {subject}{by}"),
    };
    Some(AgentEvent::Error {
        message: format!("Codex's automatic reviewer {outcome}. It did not run."),
    })
}

/// The reviewed action (`GuardianApprovalReviewAction`, tagged by `type`).
fn review_action_subject(action: Option<&Value>) -> String {
    let field = |key: &str| {
        action
            .and_then(|action| action.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| bounded(value, 256))
    };
    let subject = match action
        .and_then(|action| action.get("type"))
        .and_then(Value::as_str)
    {
        Some("command") => field("command").map(|command| format!("the command `{command}`")),
        Some("execve") => field("program").map(|program| format!("running `{program}`")),
        Some("writeStdin") => Some("terminal input".into()),
        Some("applyPatch") => Some("a file change".into()),
        Some("networkAccess") => field("host").map(|host| format!("network access to {host}")),
        Some("mcpToolCall") => {
            let tool = field("toolTitle").or_else(|| field("toolName"));
            let server = field("connectorName").or_else(|| field("server"));
            match (tool, server) {
                (Some(tool), Some(server)) => Some(format!("{server}'s {tool} call")),
                (Some(tool), None) => Some(format!("the {tool} call")),
                (None, Some(server)) => Some(format!("a {server} tool call")),
                (None, None) => Some("an MCP tool call".into()),
            }
        }
        Some("requestPermissions") => Some("a request for additional permissions".into()),
        _ => None,
    };
    subject.unwrap_or_else(|| "an action".into())
}

/// Provider text for a visible message: control characters dropped, bounded.
fn bounded(text: &str, max_chars: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.chars().count() <= max_chars {
        return cleaned.to_owned();
    }
    let mut out: String = cleaned.chars().take(max_chars).collect();
    out.push('…');
    out
}

fn exact_answer<'a>(
    question: &UserInputQuestion,
    answers: &'a [UserInputAnswer],
) -> Option<&'a str> {
    let answer = answers.first()?;
    (answer.question_id == question.id && answer.labels.len() == 1)
        .then(|| answer.labels[0].as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rows `tests/codex.rs` `approvals_round_trip_as_input_requests`
    /// cannot reach through the fake peer (one string command, one path, no
    /// machine): argv-array commands and multi-file changes stay readable,
    /// and the card is scoped to the policy's machine.
    #[test]
    fn approval_card_joins_argv_and_paths_and_is_machine_scoped() {
        let policy = luna_proto::PermissionPolicy {
            machine_name: "test-machine".into(),
            ..Default::default()
        };
        let card = |method: &str, params: Value| {
            question(
                method,
                &params,
                &json!("provider-1"),
                &policy,
                &command_choices(&params),
                None,
            )
        };
        let command = card(
            "item/commandExecution/requestApproval",
            json!({"command": ["git", "push", "--force"]}),
        );
        assert!(command.question.contains("git push --force"));
        assert_eq!(command.approval.unwrap().machine_name, "test-machine");
        let files = card(
            "item/fileChange/requestApproval",
            json!({"changes": [{"path": "/a.rs"}, {"path": "/b.rs"}]}),
        );
        assert!(files.question.contains("/a.rs, /b.rs"));
    }
}
