use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use luna_doc::MessagePart;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, ChatConfig, DoneStatus, HarnessId, Model, PermissionMode, ReasoningLevel,
    RunRequest, SandboxLevel, SessionStatus, SteeringMode, ToolCall,
};

const CODEX_CHAT: &str = "account-switch";

struct SyntheticCodex {
    runs: Arc<AtomicUsize>,
    retired: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<RunRequest>>>,
}

#[async_trait]
impl Harness for SyntheticCodex {
    fn id(&self) -> HarnessId {
        HarnessId::Codex
    }

    fn display_name(&self) -> &str {
        "Synthetic Codex"
    }

    fn supports_steering(&self) -> bool {
        true
    }

    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }

    fn deterministic_turn_end(&self) -> bool {
        true
    }

    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[ReasoningLevel::Medium]
    }

    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }

    async fn run(
        &self,
        request: RunRequest,
        mut controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let run = self.runs.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.clone());
        let retired = self.retired.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(AgentEvent::SessionStarted {
                    harness: HarnessId::Codex,
                    model: "synthetic".into(),
                    tools: vec!["read_file".into()],
                    cwd: request.cwd.clone(),
                    session_id: "stable-provider-session".into(),
                    assistant_message_id: format!("assistant-{run}"),
                }))
                .await;
            let _ = tx
                .send(Ok(AgentEvent::TextDelta {
                    text: format!("turn-{run}"),
                }))
                .await;
            if run == 0 {
                let _ = tx
                    .send(Ok(AgentEvent::ToolCall {
                        id: "tool-once".into(),
                        call: ToolCall::ReadFile {
                            path: "/synthetic/input.txt".into(),
                        },
                    }))
                    .await;
                let _ = tx
                    .send(Ok(AgentEvent::ToolResult {
                        id: "tool-once".into(),
                        is_error: false,
                        output: None,
                        diff: None,
                    }))
                    .await;
            }
            let _ = tx
                .send(Ok(AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: Some("stable-provider-session".into()),
                }))
                .await;

            loop {
                tokio::select! {
                    _ = controls.interrupt.cancelled() => {
                        retired.fetch_add(1, Ordering::SeqCst);
                        let _ = tx.send(Ok(AgentEvent::Done {
                            status: DoneStatus::Interrupted,
                            result: None,
                            error: None,
                            session_id: Some("stable-provider-session".into()),
                        })).await;
                        break;
                    }
                    steer = controls.steering.recv() => {
                        if steer.is_none() {
                            break;
                        }
                        let _ = tx.send(Ok(AgentEvent::Steered {
                            assistant_message_id: None,
                            next_assistant_message_id: None,
                        })).await;
                        let _ = tx.send(Ok(AgentEvent::Done {
                            status: DoneStatus::Completed,
                            result: None,
                            error: None,
                            session_id: Some("stable-provider-session".into()),
                        })).await;
                    }
                }
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

fn account_config(root: &std::path::Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("engine"),
        claude_config_dir: root.join("claude"),
        claude_config_file: root.join("claude.json"),
        codex_home: root.join("codex"),
    }
}

fn request(cwd: &std::path::Path, account: &str, prompt: &str) -> RunRequest {
    RunRequest {
        review: None,
        prompt: prompt.into(),
        harness: Some(HarnessId::Codex),
        provider_account_id: Some(account.into()),
        model: Some("synthetic".into()),
        reasoning: Some(ReasoningLevel::Medium),
        model_options: Default::default(),
        cwd: cwd.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: vec![],
        worktree: None,
    }
}

async fn wait_idle(core: &EngineCore, chat: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if core
                .sessions
                .session_status(chat)
                .map(|session| session.status)
                == Some(SessionStatus::Idle)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("synthetic turn settles");
}

#[tokio::test]
async fn synthetic_account_switch_retires_runtime_without_rebinding_or_tool_replay() {
    let root = tempfile::tempdir().unwrap();
    let config = account_config(root.path());
    std::fs::create_dir_all(&config.codex_home).unwrap();
    std::fs::create_dir_all(&config.claude_config_dir).unwrap();
    std::fs::write(
        config.codex_home.join("auth.json"),
        serde_json::json!({"OPENAI_API_KEY":"synthetic-native"}).to_string(),
    )
    .unwrap();
    let named_home = config.data_dir.join("provider-auth/codex/named-b");
    std::fs::create_dir_all(&named_home).unwrap();
    std::fs::write(
        named_home.join("auth.json"),
        serde_json::json!({"OPENAI_API_KEY":"synthetic-b"}).to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(&config.data_dir).unwrap();
    std::fs::write(
        config.data_dir.join("provider-account-state-v1.json"),
        serde_json::json!({
            "schemaVersion": 1,
            "claudeEnabled": true,
            "defaultCodexAccountId": "codex-native",
            "codexAccounts": [{
                "id": "named-b",
                "label": "Work",
                "accountKey": "synthetic-b",
                "profile": {"email":"b@example.test", "authKind":"api-key"},
                "createdAt": 1
            }]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        &config.claude_config_file,
        serde_json::json!({
            "oauthAccount": {"accountUuid":"claude-fixture", "emailAddress":"claude@example.test"}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        config.claude_config_dir.join(".credentials.json"),
        serde_json::json!({"claudeAiOauth":{"accessToken":"synthetic"}}).to_string(),
    )
    .unwrap();

    let runs = Arc::new(AtomicUsize::new(0));
    let retired = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(SyntheticCodex {
        runs: runs.clone(),
        retired: retired.clone(),
        requests: requests.clone(),
    }));
    let profile = EngineProfile::development(&config.data_dir, "fixture-org", "fixture-user");
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config(
        profile,
        Arc::new(registry),
        HarnessId::Codex,
        None,
        config.clone(),
    )
    .unwrap();
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    core.workspace
        .create_chat(
            CODEX_CHAT,
            None,
            Some(&core.device_id),
            Some(ChatConfig {
                harness: HarnessId::Codex,
                provider_account_id: Some("codex-native".into()),
                model: Some("synthetic".into()),
                reasoning: Some(ReasoningLevel::Medium),
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
                permission_mode: PermissionMode::Ask,
                effective_permission_mode: PermissionMode::Ask,
                permission_ceiling_source: None,
                permission_degradation_reason: None,
                permission_audit: vec![],
            }),
            Some(cwd.to_string_lossy().into_owned()),
        )
        .unwrap();

    core.sessions
        .dispatch(
            CODEX_CHAT,
            HarnessId::Codex,
            request(&cwd, "codex-native", "first"),
            Some("message-a".into()),
        )
        .await
        .unwrap();
    wait_idle(&core, CODEX_CHAT).await;
    core.sessions
        .set_provider_account(CODEX_CHAT, "named-b")
        .await
        .unwrap();
    assert_eq!(retired.load(Ordering::SeqCst), 1);

    core.sessions
        .dispatch(
            CODEX_CHAT,
            HarnessId::Codex,
            request(&cwd, "named-b", "second"),
            Some("message-b".into()),
        )
        .await
        .unwrap();
    wait_idle(&core, CODEX_CHAT).await;

    let seen = requests.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].cwd, seen[1].cwd);
    assert_eq!(seen[1].resume.as_deref(), Some("stable-provider-session"));
    assert_eq!(seen[0].provider_account_id.as_deref(), Some("codex-native"));
    assert_eq!(seen[1].provider_account_id.as_deref(), Some("named-b"));
    assert_eq!(
        seen[1]
            .model_options
            .get(luna_harness::codex::LUNA_CODEX_FILE_AUTH_OPTION)
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    let entries = core
        .doc_host
        .open(CODEX_CHAT)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    let tool_count = entries
        .iter()
        .flat_map(|entry| &entry.parts)
        .filter(|part| matches!(part, MessagePart::Tool { id, .. } if id == "tool-once"))
        .count();
    assert_eq!(tool_count, 1, "the old runtime's tool must not replay");

    let claude_chat = uuid::Uuid::new_v4().to_string();
    core.workspace
        .create_chat(
            &claude_chat,
            None,
            Some(&core.device_id),
            None,
            Some(cwd.to_string_lossy().into_owned()),
        )
        .unwrap();
    core.task_identity
        .register_new_task(
            &claude_chat,
            None,
            Some(&core.device_id),
            Some(cwd.to_str().unwrap()),
            luna_proto::TaskLineage::default(),
        )
        .unwrap();
    core.task_identity
        .record_session_binding(
            &claude_chat,
            HarnessId::ClaudeCode,
            "claude-session",
            cwd.to_str().unwrap(),
        )
        .unwrap();
    core.agent_accounts
        .forget(HarnessId::ClaudeCode, "claude-native")
        .await
        .unwrap();
    core.agent_accounts
        .activate(HarnessId::ClaudeCode, "claude-native")
        .await
        .unwrap();
    assert_eq!(
        core.task_identity
            .prepare_resume(&claude_chat, HarnessId::ClaudeCode, cwd.to_str().unwrap())
            .await
            .unwrap()
            .as_deref(),
        Some("claude-session")
    );

    core.shutdown().await;
}
