//! Permission rules, approval options and permission replies.

use super::*;

pub(super) fn opencode_permission_rules(mode: luna_proto::PermissionMode) -> Value {
    match mode {
        luna_proto::PermissionMode::Full => json!([
            { "permission": "*", "pattern": "*", "action": "allow" }
        ]),
        // Auto is resolved to Ask by the engine for OpenCode.
        luna_proto::PermissionMode::Ask | luna_proto::PermissionMode::Auto => json!([
            { "permission": "*", "pattern": "*", "action": "ask" },
            { "permission": "read", "pattern": "*", "action": "allow" },
            { "permission": "edit", "pattern": "*", "action": "allow" },
            { "permission": "glob", "pattern": "*", "action": "allow" },
            { "permission": "grep", "pattern": "*", "action": "allow" },
            { "permission": "list", "pattern": "*", "action": "allow" },
            { "permission": "bash", "pattern": "*", "action": "ask" },
            { "permission": "webfetch", "pattern": "*", "action": "ask" },
            { "permission": "websearch", "pattern": "*", "action": "ask" },
            { "permission": "external_directory", "pattern": "*", "action": "ask" },
            // OpenCode uses the LAST matching rule. Keep sensitive env files
            // behind approval while allowing explicit example templates.
            { "permission": "read", "pattern": "*.env", "action": "ask" },
            { "permission": "read", "pattern": "*.env.*", "action": "ask" },
            { "permission": "read", "pattern": "*.env.example", "action": "allow" },
            { "permission": "edit", "pattern": "*.env", "action": "ask" },
            { "permission": "edit", "pattern": "*.env.*", "action": "ask" },
            { "permission": "edit", "pattern": "*.env.example", "action": "allow" },
            { "permission": "write", "pattern": "*.env", "action": "ask" },
            { "permission": "write", "pattern": "*.env.*", "action": "ask" },
            { "permission": "write", "pattern": "*.env.example", "action": "allow" }
        ]),
    }
}

pub(super) fn opencode_supports_task_allowance(protocol: Protocol) -> bool {
    // 1.x keeps `always` grants only in the lifetime of this LUNA-owned
    // server process (one task runtime). 2.x persists them into project-wide
    // rules, which outlives the task and is therefore not an L04 task grant.
    protocol == Protocol::V1
}

pub(super) fn opencode_approval_options(protocol: Protocol) -> Vec<String> {
    let mut options = vec!["Deny".into(), "Allow once".into()];
    if opencode_supports_task_allowance(protocol) {
        options.push("Allow for task".into());
    }
    options.push("Cancel".into());
    options
}

pub(super) fn opencode_permission_reply(
    protocol: Protocol,
    decision: Option<&str>,
) -> &'static str {
    match decision {
        Some("Allow once") => "once",
        Some("Allow for task") if opencode_supports_task_allowance(protocol) => "always",
        _ => "reject",
    }
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
    let session = session.to_owned();
    let protocol = server.protocol().await;
    let supports_task_allowance = opencode_supports_task_allowance(protocol);
    // 1.x: global permission endpoint + a session-scoped fallback;
    // 2.x: the reply rides the session's permission route
    // (`{"reply": "once" | "always" | "reject"}`).
    let (reply_path, fallback_path) = match protocol {
        Protocol::V1 => (
            format!("/permission/{id}/reply"),
            Some(format!("/session/{session}/permissions/{id}")),
        ),
        Protocol::V2 => (
            format!("/api/session/{session}/permission/{id}/reply"),
            None,
        ),
    };
    let base = server.base.clone();
    let auth = server.auth.clone();
    let dir_owned = dir.map(str::to_owned);
    let protocol_cell = server.protocol.clone();
    let permission_input = Arc::clone(request_input);
    let permission_name = props
        .get("permission")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let patterns = props
        .get("patterns")
        .and_then(Value::as_array)
        .map(|patterns| {
            patterns
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let summary = if patterns.is_empty() {
        format!("OpenCode requests {permission_name} permission")
    } else {
        format!("OpenCode requests {permission_name} permission for {patterns}")
    };
    let question = UserInputQuestion {
        id: format!("permission:{id}"),
        header: "Permission".into(),
        question: format!("{summary}. Target: {}.", permission_policy.machine_name),
        options: opencode_approval_options(protocol),
        multi_select: false,
        mcp: None,
        approval: Some(luna_proto::ApprovalRequest {
            provider_request_id: id.to_owned(),
            provider: HarnessId::Opencode,
            turn_id: props
                .get("messageID")
                .and_then(Value::as_str)
                .map(str::to_owned),
            machine_name: permission_policy.machine_name.clone(),
            action_kind: match permission_name {
                "bash" => luna_proto::ApprovalActionKind::Command,
                "edit" | "write" => luna_proto::ApprovalActionKind::FileChange,
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
            supports_task_allowance,
            details: None,
        }),
        input: None,
    };
    tokio::spawn(async move {
        let server = Server {
            child: None,
            base,
            auth,
            client: http_client(),
            stderr_tail: crate::StderrTail::default(),
            protocol: protocol_cell,
        };
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
        // V2 "always" writes durable project-wide permission rules,
        // so even a forged/stale task-allowance answer fails closed.
        let reply = opencode_permission_reply(protocol, decision.as_deref());
        if server
            .post_json(
                &reply_path,
                dir_owned.as_deref(),
                &json!({ "reply": reply }),
            )
            .await
            .is_err()
            && let Some(fallback_path) = fallback_path
        {
            let _ = server
                .post_json(
                    &fallback_path,
                    dir_owned.as_deref(),
                    &json!({ "response": reply }),
                )
                .await;
        }
    });
    BusOutcome::Continue
}
