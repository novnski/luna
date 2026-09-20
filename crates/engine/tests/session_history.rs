//! L14 behavioral coverage through authoritative docs, engine RPC, SQLite,
//! and the same in-memory transport used by the desktop adapter.

use std::sync::Arc;
use std::time::Duration;

use zeron_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_harness::mock::MockHarness;
use zeron_proto::{
    ActivityRedaction, ChatConfig, HarnessId, HistoryProjectPolicy, HistoryRebuildResult,
    HistorySearchPage, HistorySearchRequest, PermissionMode, SandboxLevel, ToolCall,
};
use zeron_rpc::methods;

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Mock,
        provider_account_id: None,
        model: Some("synthetic-history-model".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Auto,
        effective_permission_mode: PermissionMode::Auto,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn tool(id: &str, call: ToolCall) -> MessagePart {
    MessagePart::Tool {
        id: id.into(),
        call,
        is_error: false,
        resolved: true,
        output: Some("private tool output must not enter the history index".into()),
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
    }
}

fn entry(id: &str, role: MessageRole, text: &str, timestamp: i64) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role,
        parts: vec![MessagePart::Text {
            id: "text".into(),
            text: text.into(),
        }],
        created_at: timestamp,
        device_id: "synthetic-device".into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    }
}

fn assemble(path: &std::path::Path) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness { script: Vec::new() }));
    EngineCore::assemble(path, Arc::new(registry), HarnessId::Mock, None).unwrap()
}

async fn search(
    client: &zeron_rpc::RpcClient,
    query: &str,
    project_id: Option<&str>,
    global: bool,
    filters: serde_json::Value,
) -> HistorySearchPage {
    let mut value = serde_json::json!({
        "query": query,
        "scope": if global { "global" } else { "selectedProject" },
        "projectId": project_id,
        "limit": 100,
    });
    value["filters"] = filters;
    serde_json::from_value(
        client
            .call(methods::SEARCH_SESSION_HISTORY, value)
            .await
            .unwrap(),
    )
    .unwrap()
}

async fn wait_for_search(
    client: &zeron_rpc::RpcClient,
    query: &str,
    project: &str,
) -> HistorySearchPage {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let page = search(client, query, Some(project), false, serde_json::json!({})).await;
        if !page.events.is_empty() {
            return page;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "history index did not update"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_history_search_rebuild_filters_links_redacts_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path());
    core.workspace
        .create_space(
            "project-a",
            &core.device_id,
            "/synthetic/project-a",
            Some("Duplicate".into()),
            false,
        )
        .unwrap();
    core.workspace
        .create_space(
            "project-b",
            &core.device_id,
            "/synthetic/project-b",
            Some("Duplicate".into()),
            false,
        )
        .unwrap();
    core.workspace
        .create_chat("task-a", Some("project-a"), None, Some(config()), None)
        .unwrap();
    core.workspace
        .create_chat("task-b", Some("project-b"), None, Some(config()), None)
        .unwrap();

    let task_a = core.doc_host.open("task-a").unwrap();
    task_a
        .doc()
        .push_message(&entry("prompt-a", MessageRole::User, "alpha request", 10))
        .unwrap();
    task_a
        .doc()
        .push_message(&SessionMessageEntry {
            id: "answer-a".into(),
            role: MessageRole::Assistant,
            parts: vec![
                MessagePart::Text {
                    id: "answer".into(),
                    text: "alpha answer token=synthetic-secret".into(),
                },
                tool(
                    "command",
                    ToolCall::Exec {
                        command: "cargo test session_history".into(),
                    },
                ),
                tool(
                    "sensitive-file",
                    ToolCall::ReadFile {
                        path: "/synthetic/project-a/.env".into(),
                    },
                ),
                tool(
                    "app",
                    ToolCall::Mcp {
                        server: "linear".into(),
                        tool: "read_issue".into(),
                        input: Some(serde_json::json!({"secret": "not indexed"})),
                    },
                ),
            ],
            created_at: 20,
            device_id: core.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
        .unwrap();
    core.doc_host
        .open("task-b")
        .unwrap()
        .doc()
        .push_message(&entry("prompt-b", MessageRole::User, "alpha request", 30))
        .unwrap();

    let client = zeron_rpc::memory_client(core.rpc_service());
    let rebuilt: HistoryRebuildResult = serde_json::from_value(
        client
            .call(
                methods::REBUILD_SESSION_HISTORY,
                serde_json::json!({"taskIds": []}),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(rebuilt.indexed_tasks, 2);
    assert!(rebuilt.indexed_events >= 7);

    let project = search(
        &client,
        "alpha",
        Some("project-a"),
        false,
        serde_json::json!({}),
    )
    .await;
    assert!(project.events.iter().all(|event| event.task_id == "task-a"));
    assert!(
        project
            .events
            .iter()
            .all(|event| event.source.available_here)
    );
    assert!(
        project
            .events
            .iter()
            .any(|event| event.source.message_id == "prompt-a")
    );

    let global = search(&client, "alpha", None, true, serde_json::json!({})).await;
    assert!(global.events.iter().any(|event| event.task_id == "task-a"));
    assert!(global.events.iter().any(|event| event.task_id == "task-b"));
    let command = search(
        &client,
        "cargo",
        Some("project-a"),
        false,
        serde_json::json!({"tools": ["exec"], "outcomes": ["succeeded"]}),
    )
    .await;
    assert_eq!(command.events.len(), 1);
    assert_eq!(
        command.events[0].command.as_deref(),
        Some("cargo test session_history")
    );
    let app = search(
        &client,
        "linear",
        Some("project-a"),
        false,
        serde_json::json!({"apps": ["linear"], "models": ["synthetic-history-model"]}),
    )
    .await;
    assert_eq!(app.events.len(), 1);
    assert!(!serde_json::to_string(&app).unwrap().contains("not indexed"));
    assert!(
        search(
            &client,
            "synthetic-secret",
            Some("project-a"),
            false,
            serde_json::json!({}),
        )
        .await
        .events
        .is_empty()
    );
    assert!(
        search(
            &client,
            "redacted",
            Some("project-a"),
            false,
            serde_json::json!({}),
        )
        .await
        .events
        .iter()
        .any(|event| event.redaction == ActivityRedaction::Heuristic)
    );
    assert!(
        search(
            &client,
            ".env",
            Some("project-a"),
            false,
            serde_json::json!({}),
        )
        .await
        .events
        .is_empty()
    );

    let policy = HistoryProjectPolicy {
        project_id: "project-a".into(),
        excluded_path_prefixes: vec!["/synthetic/project-a/private".into()],
        excluded_terms: vec!["alpha request".into()],
        index_terminal_output: false,
        index_browser_fields: false,
        index_accessibility_text: false,
        index_screenshot_text: false,
    };
    let mut unsafe_policy = policy.clone();
    unsafe_policy.index_terminal_output = true;
    assert!(
        client
            .call(
                methods::SET_SESSION_HISTORY_POLICY,
                serde_json::to_value(&unsafe_policy).unwrap(),
            )
            .await
            .is_err(),
        "the policy surface must not imply unsupported sensitive capture"
    );
    client
        .call(
            methods::SET_SESSION_HISTORY_POLICY,
            serde_json::to_value(&policy).unwrap(),
        )
        .await
        .unwrap();
    assert!(
        search(
            &client,
            "alpha request",
            Some("project-a"),
            false,
            serde_json::json!({}),
        )
        .await
        .events
        .is_empty()
    );

    task_a
        .doc()
        .push_message(&entry(
            "incremental",
            MessageRole::User,
            "incremental sentinel",
            40,
        ))
        .unwrap();
    // The real producer updates workspace freshness after transcript persist;
    // the index reconciler consumes that metadata edge without search-time raw reads.
    core.workspace
        .note_message("task-a", "incremental sentinel");
    let incremental = wait_for_search(&client, "incremental sentinel", "project-a").await;
    assert_eq!(incremental.events[0].source.message_id, "incremental");

    core.workspace.delete_chat("task-b").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let page = search(&client, "alpha", None, true, serde_json::json!({})).await;
        if page.events.iter().all(|event| event.task_id != "task-b") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "deleted task rows remained searchable"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    drop(client);
    core.shutdown().await;
    drop(core);
    let reopened = assemble(dir.path());
    let reopened_client = zeron_rpc::memory_client(reopened.rpc_service());
    let persisted = wait_for_search(&reopened_client, "incremental sentinel", "project-a").await;
    assert_eq!(persisted.events[0].task_id, "task-a");
    reopened.shutdown().await;
}

#[test]
fn request_contract_remains_bounded() {
    let request: HistorySearchRequest = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(request.limit, 50);
}
