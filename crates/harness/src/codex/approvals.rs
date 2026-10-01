//! Exact native decision tables: client labels never manufacture a grant.
use super::{RequestInputFn, ServerRequestContext, new_message_id};
use crate::jsonrpc::RpcClient;
use luna_proto::{
    ApprovalActionKind, ApprovalChoice, ApprovalDetails, ApprovalNetworkContext, ApprovalRequest,
    HarnessId, UserInputAnswer, UserInputQuestion,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) type PendingApprovals = HashMap<String, CancellationToken>;

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
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
fn question(
    method: &str,
    params: &Value,
    id: &Value,
    policy: &luna_proto::PermissionPolicy,
    choices: &[Choice],
) -> UserInputQuestion {
    let mut details = details(params);
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
    UserInputQuestion {
        id: new_message_id(),
        header: header.into(),
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
            summary: summary.into(),
            requested_scope: policy.scope,
            expires_at_ms: now_ms().saturating_add(120_000),
            supports_task_allowance: choices
                .iter()
                .any(|choice| choice.display.scope == "session"),
            details: Some(details),
        }),
    }
}

#[cfg(test)]
pub(super) fn legacy_question(
    method: &str,
    params: &Value,
    id: &Value,
    policy: &luna_proto::PermissionPolicy,
) -> UserInputQuestion {
    question(method, params, id, policy, &command_choices(params))
}

/// Strict for permissions; old command/file peers may omit routing fields.
/// A supplied foreign/stale ID is always rejected, including Full mode.
fn exact_context(params: &Value, context: &ServerRequestContext<'_>, strict: bool) -> bool {
    !context.interrupt.is_cancelled()
        && !context.computer_use_turn_cancel.is_cancelled()
        && context.active_turn_id.is_some()
        && (!strict
            || context.runtime_contract.is_some_and(|contract| {
                !contract.context.thread_id.is_empty() && !contract.context.machine_id.is_empty()
            }))
        && (params.get("threadId").and_then(Value::as_str) == Some(context.thread_id)
            || (!strict && params.get("threadId").is_none()))
        && (params.get("turnId").and_then(Value::as_str) == context.active_turn_id
            || (!strict && params.get("turnId").is_none()))
        && params
            .get("itemId")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
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
    // Full mode remains once-only; never choose a session or durable rule.
    if context.auto_approve && !permissions {
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
    let mut question = question(method, params, &id, &context.permission_policy, &choices);
    if permissions && let Some(contract) = context.runtime_contract {
        question.question = format!(
            "LUNA task {}. {}",
            contract.context.thread_id, question.question
        );
    }
    let cancel = CancellationToken::new();
    pending.insert(id.to_string(), cancel.clone());
    let client = client.clone();
    let request_input: Arc<RequestInputFn> = Arc::clone(context.request_input);
    let interrupt = context.interrupt.clone();
    let turn_cancel = context.computer_use_turn_cancel.clone();
    tokio::spawn(async move {
        let receiver = (request_input)(vec![question.clone()]);
        let answers = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = interrupt.cancelled() => {cancel.cancel(); return;},
            _ = turn_cancel.cancelled() => {cancel.cancel(); return;},
            result = tokio::time::timeout(std::time::Duration::from_secs(120), receiver) => result.ok().and_then(Result::ok),
        };
        let response = if !interrupt.is_cancelled()
            && !turn_cancel.is_cancelled()
            && !cancel.is_cancelled()
            && now_ms() < question.approval.as_ref().unwrap().expires_at_ms
        {
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
fn exact_answer<'a>(
    question: &UserInputQuestion,
    answers: &'a [UserInputAnswer],
) -> Option<&'a str> {
    let answer = answers.first()?;
    (answer.question_id == question.id && answer.labels.len() == 1)
        .then(|| answer.labels[0].as_str())
}
