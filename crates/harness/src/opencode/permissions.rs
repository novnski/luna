//! Permission rules, approval options and permission replies.
//!
//! OpenCode 2 rules are `[{action, resource, effect}]`, evaluated LAST match
//! wins, with the session's rules merged after the agent's (so they decide).
//! Replies are `POST /api/session/{s}/permission/{id}/reply` with
//! `{decision: "once" | "always" | "reject", message?}`. LUNA never sends
//! `always` (PD-8): on 2.x it writes durable project-wide rules.

use super::*;

/// The LUNA permission mode as a v2 session ruleset (v2 action names:
/// `shell`, `edit`, `read`, `glob`, `grep`, `webfetch`, `websearch`,
/// `external_directory`, `question`, `subagent`, MCP tool names).
pub(super) fn opencode_permission_rules(mode: luna_proto::PermissionMode) -> Value {
    match mode {
        luna_proto::PermissionMode::Full => json!([
            { "action": "*", "resource": "*", "effect": "allow" }
        ]),
        // Auto is resolved to Ask by the engine for OpenCode.
        luna_proto::PermissionMode::Ask | luna_proto::PermissionMode::Auto => json!([
            { "action": "*", "resource": "*", "effect": "ask" },
            { "action": "read", "resource": "*", "effect": "allow" },
            { "action": "glob", "resource": "*", "effect": "allow" },
            { "action": "grep", "resource": "*", "effect": "allow" },
            // The native question tool asks the user through LUNA's input
            // panel; gating it behind an approval first would double-ask
            // (OpenCode's own default agent allows it).
            { "action": "question", "resource": "*", "effect": "allow" },
            { "action": "shell", "resource": "*", "effect": "ask" },
            { "action": "edit", "resource": "*", "effect": "ask" },
            { "action": "external_directory", "resource": "*", "effect": "ask" },
            { "action": "webfetch", "resource": "*", "effect": "ask" },
            { "action": "websearch", "resource": "*", "effect": "ask" },
            // Last match wins: keep sensitive env files behind approval while
            // allowing explicit example templates.
            { "action": "read", "resource": "*.env", "effect": "ask" },
            { "action": "read", "resource": "*.env.*", "effect": "ask" },
            { "action": "read", "resource": "*.env.example", "effect": "allow" }
        ]),
    }
}

/// 2.x persists `always` grants into project-wide rules that outlive the
/// task, so there is no task-scoped allowance to offer (PD-8).
pub(super) fn opencode_supports_task_allowance() -> bool {
    false
}

pub(super) fn opencode_approval_options() -> Vec<String> {
    let mut options = vec!["Deny".into(), "Allow once".into()];
    if opencode_supports_task_allowance() {
        options.push("Allow for task".into());
    }
    options.push("Cancel".into());
    options
}

/// The native decision for a LUNA answer. Anything but an explicit "Allow
/// once" — including a forged or stale "Allow for task" — fails closed.
pub(super) fn opencode_permission_reply(decision: Option<&str>) -> &'static str {
    match decision {
        Some("Allow once") => "once",
        _ => "reject",
    }
}

/// The v2 reply body.
pub(super) fn opencode_permission_reply_body(decision: Option<&str>) -> Value {
    json!({ "decision": opencode_permission_reply(decision) })
}

/// A native `permission.asked` request, with what answering it needs.
pub(super) struct PermissionAsk<'a> {
    pub(super) props: &'a Value,
    pub(super) event_session: Option<&'a str>,
    pub(super) session_id: &'a str,
    pub(super) children: &'a HashMap<String, ChildRun>,
    pub(super) unbound_children: &'a HashMap<String, String>,
    pub(super) server: &'a Server,
    pub(super) dir: Option<&'a str>,
    pub(super) event_tx: &'a mpsc::Sender<Result<AgentEvent, HarnessError>>,
    pub(super) request_input: &'a Arc<RequestInput>,
    pub(super) auto_approve: bool,
    pub(super) permission_policy: &'a luna_proto::PermissionPolicy,
}

/// Route a permission request owned by this run (or one of its live
/// children) to the approval surface and reply on the native route.
pub(super) async fn handle_permission_asked(ask: PermissionAsk<'_>) -> BusOutcome {
    let PermissionAsk {
        props,
        event_session,
        session_id,
        children,
        unbound_children,
        server,
        dir,
        event_tx,
        request_input,
        auto_approve,
        permission_policy,
    } = ask;
    // A global bus includes unrelated sessions. Never answer their
    // requests, or a malformed request without an explicit owner.
    let Some(session) = event_session.filter(|session| {
        *session == session_id
            || children.get(*session).is_some_and(|child| !child.done)
            || unbound_children.contains_key(*session)
    }) else {
        return BusOutcome::Continue;
    };
    let Some(id) = props.get("id").and_then(Value::as_str) else {
        return BusOutcome::Continue;
    };
    let reply_path = format!("/api/session/{session}/permission/{id}/reply");
    let reply_server = server.detached();
    let dir_owned = dir.map(str::to_owned);
    let failures = event_tx.clone();
    let permission_input = Arc::clone(request_input);
    // v2 request: `{id, sessionID, action, resources[], save?, metadata?,
    // source?: {type, messageID, id}, message?}`.
    let action = props
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let resources = props
        .get("resources")
        .and_then(Value::as_array)
        .map(|resources| {
            resources
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let summary = if resources.is_empty() {
        format!("OpenCode requests {action} permission")
    } else {
        format!("OpenCode requests {action} permission for {resources}")
    };
    let question = UserInputQuestion {
        id: format!("permission:{id}"),
        header: "Permission".into(),
        question: format!("{summary}. Target: {}.", permission_policy.machine_name),
        options: opencode_approval_options(),
        multi_select: false,
        mcp: None,
        approval: Some(luna_proto::ApprovalRequest {
            provider_request_id: id.to_owned(),
            provider: HarnessId::Opencode,
            turn_id: props
                .pointer("/source/messageID")
                .and_then(Value::as_str)
                .map(str::to_owned),
            machine_name: permission_policy.machine_name.clone(),
            action_kind: match action {
                "shell" => luna_proto::ApprovalActionKind::Command,
                "edit" => luna_proto::ApprovalActionKind::FileChange,
                "webfetch" | "websearch" => luna_proto::ApprovalActionKind::Network,
                "external_directory" => luna_proto::ApprovalActionKind::ExternalPath,
                _ => luna_proto::ApprovalActionKind::Tool,
            },
            summary,
            requested_scope: permission_policy.scope,
            expires_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64
                + 120_000,
            supports_task_allowance: opencode_supports_task_allowance(),
            details: None,
        }),
        input: None,
    };
    tokio::spawn(async move {
        let decision: Option<String> = if auto_approve {
            Some("Allow once".into())
        } else {
            tokio::time::timeout(
                std::time::Duration::from_secs(120),
                (permission_input)(vec![question.clone()]),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|answers| {
                answers
                    .into_iter()
                    .find(|answer| answer.question_id == question.id)
                    .and_then(|answer| answer.labels.into_iter().next())
            })
        };
        let body = opencode_permission_reply_body(decision.as_deref());
        // A failed reply leaves the native tool blocked: say so instead of
        // silently pretending the approval was delivered.
        if let Err(error) = reply_server
            .post_json(&reply_path, dir_owned.as_deref(), &body)
            .await
        {
            let _ = failures
                .send(Ok(AgentEvent::Error {
                    message: format!("OpenCode did not accept the permission reply: {error}"),
                }))
                .await;
        }
    });
    BusOutcome::Continue
}
