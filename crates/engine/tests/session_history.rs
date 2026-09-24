//! L14 behavioral coverage through authoritative docs, engine RPC, SQLite,
//! and the same in-memory transport used by the desktop adapter.

use std::sync::Arc;
use std::time::Duration;

use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_engine::{EngineCore, HarnessRegistry, RunJournal};
use luna_harness::mock::MockHarness;
use luna_proto::{
    ActivityRedaction, AgentEvent, ChatConfig, HarnessId, HistoryProjectPolicy,
    HistoryRebuildResult, HistorySearchPage, HistorySearchRequest, PermissionMode, SandboxLevel,
    ToolCall,
};
use luna_rpc::methods;

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
        subagent_activity: None,
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

fn profile_root(path: &std::path::Path) -> std::path::PathBuf {
    path.join("orgs/dev-org/dev-user")
}

fn raw_activity_index(path: &std::path::Path) -> String {
    let connection = rusqlite::Connection::open(profile_root(path).join("docs.sqlite3")).unwrap();
    let mut statement = connection
        .prepare("SELECT search_text, envelope_json FROM activity_events ORDER BY event_id")
        .unwrap();
    statement
        .query_map([], |row| {
            let search: String = row.get(0)?;
            let envelope: Vec<u8> = row.get(1)?;
            Ok(format!("{search}\n{}", String::from_utf8_lossy(&envelope)))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n")
}

fn raw_activity_index_for_task(path: &std::path::Path, task_id: &str) -> String {
    let connection = rusqlite::Connection::open(profile_root(path).join("docs.sqlite3")).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT search_text, envelope_json FROM activity_events WHERE task_id=?1 ORDER BY event_id",
        )
        .unwrap();
    statement
        .query_map([task_id], |row| {
            let search: String = row.get(0)?;
            let envelope: Vec<u8> = row.get(1)?;
            Ok(format!("{search}\n{}", String::from_utf8_lossy(&envelope)))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n")
}

async fn search(
    client: &luna_rpc::RpcClient,
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
    client: &luna_rpc::RpcClient,
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
                tool(
                    "excluded-command-term",
                    ToolCall::Exec {
                        command: "echo PRIVATE_SENTINEL".into(),
                    },
                ),
                tool(
                    "excluded-file-term",
                    ToolCall::ReadFile {
                        path: "/synthetic/project-a/PRIVATE_SENTINEL.txt".into(),
                    },
                ),
                tool(
                    "excluded-app-tool-term",
                    ToolCall::Mcp {
                        server: "PRIVATE_SENTINEL-app".into(),
                        tool: "PRIVATE_SENTINEL-tool".into(),
                        input: None,
                    },
                ),
                MessagePart::Image {
                    id: "excluded-image-term".into(),
                    path: "/synthetic/ignored-image.png".into(),
                    name: "PRIVATE_SENTINEL.png".into(),
                    mime_type: "image/png".into(),
                },
            ],
            created_at: 20,
            device_id: core.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
        .unwrap();

    let journal = RunJournal::open(profile_root(dir.path()).join("journals")).unwrap();
    journal
        .append(
            "task-a",
            &AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "Recorded-History-Model".into(),
                tools: Vec::new(),
                cwd: "/synthetic/project-a".into(),
                session_id: "recorded-session".into(),
                assistant_message_id: "answer-a".into(),
            },
        )
        .unwrap();
    drop(journal);
    core.doc_host
        .open("task-b")
        .unwrap()
        .doc()
        .push_message(&entry("prompt-b", MessageRole::User, "alpha request", 30))
        .unwrap();

    let client = luna_rpc::memory_client(core.rpc_service());
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
        serde_json::json!({"apps": ["linear"], "models": ["RECORDED-HISTORY-MODEL"]}),
    )
    .await;
    assert_eq!(app.events.len(), 1);
    assert_eq!(
        app.events[0].model.as_deref(),
        Some("Recorded-History-Model")
    );
    assert_eq!(app.events[0].provider, None);
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
        excluded_terms: vec!["alpha request".into(), "private_sentinel".into()],
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
    let minimized = raw_activity_index(dir.path()).to_lowercase();
    assert!(
        !minimized.contains("private_sentinel"),
        "project exclusions must remove matching command/file/app/tool/image metadata from search_text and envelope_json"
    );
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

    core.workspace
        .create_space(
            "project-policy-initial",
            &core.device_id,
            "/synthetic/project-policy-initial",
            Some("Policy initial".into()),
            false,
        )
        .unwrap();
    client
        .call(
            methods::SET_SESSION_HISTORY_POLICY,
            serde_json::to_value(&HistoryProjectPolicy {
                project_id: "project-policy-initial".into(),
                excluded_path_prefixes: Vec::new(),
                excluded_terms: vec!["private_sentinel".into()],
                index_terminal_output: false,
                index_browser_fields: false,
                index_accessibility_text: false,
                index_screenshot_text: false,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    core.workspace
        .create_chat(
            "task-policy-initial",
            Some("project-policy-initial"),
            None,
            Some(config()),
            None,
        )
        .unwrap();
    core.doc_host
        .open("task-policy-initial")
        .unwrap()
        .doc()
        .push_message(&SessionMessageEntry {
            id: "policy-initial-answer".into(),
            role: MessageRole::Assistant,
            parts: vec![
                MessagePart::Text {
                    id: "safe".into(),
                    text: "initial policy index ready".into(),
                },
                tool(
                    "private-command",
                    ToolCall::Exec {
                        command: "echo PRIVATE_SENTINEL".into(),
                    },
                ),
            ],
            created_at: 50,
            device_id: core.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
        .unwrap();
    core.workspace
        .note_message("task-policy-initial", "initial policy index ready");
    wait_for_search(
        &client,
        "initial policy index ready",
        "project-policy-initial",
    )
    .await;
    assert!(
        !raw_activity_index_for_task(dir.path(), "task-policy-initial")
            .to_lowercase()
            .contains("private_sentinel")
    );
    client
        .call(
            methods::REBUILD_SESSION_HISTORY,
            serde_json::json!({"taskIds": []}),
        )
        .await
        .unwrap();
    assert!(
        !raw_activity_index_for_task(dir.path(), "task-policy-initial")
            .to_lowercase()
            .contains("private_sentinel")
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
    task_a
        .doc()
        .push_message(&SessionMessageEntry {
            id: "incremental-private-metadata".into(),
            role: MessageRole::Assistant,
            parts: vec![
                MessagePart::Text {
                    id: "safe".into(),
                    text: "incremental policy reconciliation completed".into(),
                },
                tool(
                    "private-command",
                    ToolCall::Exec {
                        command: "echo PRIVATE_SENTINEL".into(),
                    },
                ),
            ],
            created_at: 41,
            device_id: core.device_id.clone(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
        .unwrap();
    // The real producer updates workspace freshness after transcript persist;
    // the index reconciler consumes that metadata edge without search-time raw reads.
    core.workspace
        .note_message("task-a", "incremental sentinel");
    let incremental = wait_for_search(&client, "incremental sentinel", "project-a").await;
    assert_eq!(incremental.events[0].source.message_id, "incremental");
    let incremental_policy =
        wait_for_search(&client, "incremental policy reconciliation", "project-a").await;
    assert_eq!(
        incremental_policy.events[0].source.message_id,
        "incremental-private-metadata"
    );
    assert!(
        !raw_activity_index(dir.path())
            .to_lowercase()
            .contains("private_sentinel")
    );

    let mut changed_config = config();
    changed_config.model = Some("current-task-model-must-not-rewrite-history".into());
    core.workspace
        .set_chat_config("task-a", &changed_config)
        .unwrap();
    client
        .call(
            methods::REBUILD_SESSION_HISTORY,
            serde_json::json!({"taskIds": ["task-a"]}),
        )
        .await
        .unwrap();
    let recorded = search(
        &client,
        "linear",
        Some("project-a"),
        false,
        serde_json::json!({"models": ["recorded-history-model"]}),
    )
    .await;
    assert_eq!(recorded.events.len(), 1);
    assert_eq!(
        recorded.events[0].model.as_deref(),
        Some("Recorded-History-Model")
    );
    assert!(
        search(
            &client,
            "linear",
            Some("project-a"),
            false,
            serde_json::json!({"models": ["current-task-model-must-not-rewrite-history"]}),
        )
        .await
        .events
        .is_empty()
    );

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
    let reopened_client = luna_rpc::memory_client(reopened.rpc_service());
    let persisted = wait_for_search(&reopened_client, "incremental sentinel", "project-a").await;
    assert_eq!(persisted.events[0].task_id, "task-a");
    assert!(
        !raw_activity_index(dir.path())
            .to_lowercase()
            .contains("private_sentinel")
    );
    let persisted_recorded = search(
        &reopened_client,
        "linear",
        Some("project-a"),
        false,
        serde_json::json!({"models": ["RECORDED-HISTORY-MODEL"]}),
    )
    .await;
    assert_eq!(persisted_recorded.events.len(), 1);
    reopened.shutdown().await;
}

#[test]
fn request_contract_remains_bounded() {
    let request: HistorySearchRequest = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(request.limit, 50);
}
