//! CC-10 Claude native history at the real UI-RPC -> engine -> production
//! `ClaudeHarness` -> deterministic 2.1.286 peer boundary
//! (`crates/harness/tests/fixtures/fake-claude-history.py`). The peer writes
//! native transcripts under a synthetic `.fake-claude-config`; no real Claude
//! configuration, model or network is involved. Failure cases:
//! `$B/evidence/cc10-history/failure-cases.md`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{ClaudeHarness, Harness, NativeHistoryScope};
use luna_proto::{
    ChatConfig, CommandReceipt, CommandReceiptStatus, HarnessId, HistoryCapabilityView,
    NativeFileRewindView, PermissionMode, ProviderForkCapability, ReasoningLevel, RunRequest,
    SandboxLevel, TaskCommandRead,
};
use luna_rpc::{RpcClient, methods};
use sha2::Digest as _;

const PROJECT: &str = "cc10-project";

fn fake() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/fake-claude-history.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn claude_home(workspace: &Path) -> PathBuf {
    workspace.join(".fake-claude-config")
}

fn assemble(root: &Path, workspace: &Path) -> EngineCore {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(ClaudeHarness::new().with_executable(fake())));
    let identity = root.join("claude.json");
    std::fs::write(
        &identity,
        r#"{"oauthAccount":{"accountUuid":"cc10-fixture","emailAddress":"cc10@example.test"}}"#,
    )
    .unwrap();
    let accounts = AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: claude_home(workspace),
        claude_config_file: identity,
        codex_home: root.join("codex"),
    };
    accounts
        .disable_claude_keychain_fallback()
        .expect("synthetic accounts");
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("data"), "cc10-org", "cc10-user"),
        Arc::new(registry),
        HarnessId::ClaudeCode,
        None,
        accounts,
    )
    .unwrap()
}

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::ClaudeCode,
        provider_account_id: None,
        model: None,
        reasoning: Some(ReasoningLevel::Medium),
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn envelope(
    command_id: &str,
    task_id: &str,
    snapshot: Option<&TaskCommandRead>,
    intent: serde_json::Value,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "commandId": command_id, "actorId": "test-user", "clientId": "test-client",
        "clientSessionId": "test-session", "taskId": task_id, "intent": intent,
    });
    if let Some(snapshot) = snapshot {
        value["expectedTaskRevision"] = snapshot.snapshot.task_revision.into();
        if let Some(generation) = snapshot.snapshot.binding_generation {
            value["expectedBindingGeneration"] = generation.into();
        }
    }
    value
}

async fn execute(client: &RpcClient, value: serde_json::Value) -> CommandReceipt {
    client
        .call_as(methods::EXECUTE_TASK_COMMAND, value)
        .await
        .unwrap()
}

async fn snapshot(client: &RpcClient, task_id: &str) -> TaskCommandRead {
    client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": task_id }),
        )
        .await
        .unwrap()
}

fn entries(core: &EngineCore, task_id: &str) -> Vec<SessionMessageEntry> {
    core.doc_host
        .open(task_id)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap()
}

fn checkpoint(core: &EngineCore, task_id: &str, entry_id: &str) -> Option<String> {
    core.doc_host
        .open(task_id)
        .unwrap()
        .doc()
        .provider_checkpoint(entry_id)
}

fn text(entry: &SessionMessageEntry) -> String {
    entry
        .parts
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text, .. } => Some(text.trim()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn session(core: &EngineCore, task_id: &str) -> String {
    core.task_identity
        .view(task_id)
        .and_then(|view| view.current_binding)
        .map(|binding| binding.provider_task_id)
        .unwrap_or_default()
}

fn review(
    core: &EngineCore,
    read: &TaskCommandRead,
    task_id: &str,
    message_id: Option<&str>,
) -> serde_json::Value {
    let chat = core.workspace.chat(task_id).unwrap().unwrap();
    let message_text = message_id.map(|id| {
        text(
            entries(core, task_id)
                .iter()
                .find(|entry| entry.id == id)
                .unwrap(),
        )
    });
    serde_json::json!({
        "taskRevision": read.snapshot.task_revision,
        "bindingGeneration": read.snapshot.binding_generation.unwrap(),
        "providerSessionId": chat.harness_session_id.unwrap(),
        "providerAccountId": null,
        "cwd": chat.cwd.unwrap_or_else(|| ".".into()),
        "messageId": message_id,
        "messageText": message_text,
    })
}

async fn native_turns(workspace: &Path, session_id: &str) -> Vec<String> {
    ClaudeHarness::new()
        .with_executable(fake())
        .native_history_turns(NativeHistoryScope {
            session_id: session_id.into(),
            cwd: workspace.to_string_lossy().into_owned(),
            provider_home: Some(claude_home(workspace).to_string_lossy().into_owned()),
        })
        .await
        .unwrap()
        .turn_starts
}

fn transcript(workspace: &Path, session_id: &str) -> PathBuf {
    std::fs::read_dir(claude_home(workspace).join("projects"))
        .unwrap()
        .flatten()
        .map(|dir| dir.path().join(format!("{session_id}.jsonl")))
        .find(|path| path.is_file())
        .expect("native transcript")
}

fn sha(path: &Path) -> String {
    format!("{:x}", sha2::Sha256::digest(std::fs::read(path).unwrap()))
}

/// Wait until the task is idle and every assistant row is settled.
async fn settle(client: &RpcClient, core: &EngineCore, task_id: &str, assistants: usize) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let rows = entries(core, task_id);
            let done = rows
                .iter()
                .filter(|entry| {
                    entry.role == MessageRole::Assistant
                        && entry.status == Some(MessageStatus::Complete)
                })
                .count();
            if done >= assistants
                && !session(core, task_id).is_empty()
                && snapshot(client, task_id)
                    .await
                    .snapshot
                    .active_turn_id
                    .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("task {task_id} settled: {:#?}", entries(core, task_id)));
}

async fn send(
    client: &RpcClient,
    core: &EngineCore,
    task_id: &str,
    workspace: &Path,
    message_id: &str,
    prompt: &str,
) -> String {
    let before = entries(core, task_id)
        .iter()
        .filter(|entry| entry.role == MessageRole::Assistant)
        .count();
    let receipt = execute(
        client,
        envelope(
            &format!("send-{message_id}"),
            task_id,
            Some(&snapshot(client, task_id).await),
            serde_json::json!({
                "kind": "session",
                "command": {
                    "kind": "run",
                    "messageId": message_id,
                    "request": run_request(workspace, prompt),
                }
            }),
        ),
    )
    .await;
    assert!(
        matches!(
            receipt.status,
            CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
        ),
        "{receipt:?}"
    );
    settle(client, core, task_id, before + 1).await;
    let rows = entries(core, task_id);
    let assistant = rows
        .iter()
        .rev()
        .find(|entry| entry.role == MessageRole::Assistant)
        .unwrap();
    checkpoint(core, task_id, &assistant.id).expect("completed Claude turn has a checkpoint")
}

fn run_request(workspace: &Path, prompt: &str) -> RunRequest {
    RunRequest {
        review: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: Some(HarnessId::ClaudeCode),
        provider_account_id: None,
        model: None,
        reasoning: Some(ReasoningLevel::Medium),
        model_options: Default::default(),
        cwd: workspace.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: Vec::new(),
        worktree: None,
    }
}

fn knobs(workspace: &Path, value: serde_json::Value) {
    std::fs::write(
        claude_home(workspace).join("fake-claude-knobs.json"),
        value.to_string(),
    )
    .unwrap();
}

fn conversation_ids(core: &EngineCore, task_id: &str) -> Vec<String> {
    entries(core, task_id)
        .into_iter()
        .filter(|entry| matches!(entry.role, MessageRole::User | MessageRole::Assistant))
        .map(|entry| entry.id)
        .collect()
}

/// E15/E27: native UUID checkpoints, provider fork, same-chat rewind with an
/// uncertain outcome and reconciliation, refusal on divergent history, file
/// checkpoint preview/restore and earlier-message edit, all on Claude.
#[tokio::test(flavor = "multi_thread")]
async fn claude_history_operations_use_native_identities_end_to_end() {
    let root = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    std::fs::write(workspace.join("notes.txt"), "alpha\n").unwrap();
    let core = assemble(root.path(), &workspace);
    let client = luna_rpc::memory_client(core.rpc_service());
    client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createSpace", "spaceId": PROJECT, "deviceId": core.device_id,
                "path": workspace.to_string_lossy(),
            }),
        )
        .await
        .expect("project");
    let task = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        execute(
            &client,
            envelope(
                "cc10-create",
                &task,
                None,
                serde_json::json!({
                    "kind": "createChat", "spaceId": PROJECT,
                    "cwd": workspace.to_string_lossy(), "config": config(),
                }),
            ),
        )
        .await
        .status,
        CommandReceiptStatus::Applied
    );

    // A1: completed turns carry the native prompt uuid as their checkpoint.
    let c1 = send(
        &client,
        &core,
        &task,
        &workspace,
        "u1",
        "edit:notes.txt=beta",
    )
    .await;
    let c2 = send(
        &client,
        &core,
        &task,
        &workspace,
        "u2",
        "edit:notes.txt=gamma",
    )
    .await;
    let c3 = send(&client, &core, &task, &workspace, "u3", "third prompt").await;
    let source_session = session(&core, &task);
    assert_eq!(
        native_turns(&workspace, &source_session).await,
        vec![c1.clone(), c2.clone(), c3.clone()]
    );
    let capability: HistoryCapabilityView = client
        .call_as(
            methods::GET_HISTORY_CAPABILITY,
            serde_json::json!({ "taskId": task }),
        )
        .await
        .unwrap();
    assert_eq!(capability.capability, ProviderForkCapability::Checkpoint);
    assert_eq!(capability.unavailable_reason, None);

    // D3/D4: preview without touching files; a stale digest is refused.
    let notes = workspace.join("notes.txt");
    let preview: NativeFileRewindView = client
        .call_as(
            methods::PREVIEW_NATIVE_FILE_REWIND,
            serde_json::json!({ "taskId": task, "messageId": "u2" }),
        )
        .await
        .unwrap();
    assert!(preview.dry_run && preview.can_rewind, "{preview:?}");
    assert_eq!(preview.files_changed.len(), 1);
    assert!(preview.limitations.contains("Bash"));
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "gamma\n");
    let stale = client
        .call_as::<NativeFileRewindView>(
            methods::APPLY_NATIVE_FILE_REWIND,
            serde_json::json!({ "taskId": task, "messageId": "u2", "digest": "stale" }),
        )
        .await;
    assert!(stale.unwrap_err().to_string().contains("since the preview"));
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "gamma\n");
    let restored: NativeFileRewindView = client
        .call_as(
            methods::APPLY_NATIVE_FILE_REWIND,
            serde_json::json!({ "taskId": task, "messageId": "u2", "digest": preview.digest }),
        )
        .await
        .unwrap();
    assert!(!restored.dry_run && restored.can_rewind);
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "beta\n");

    // B1–B3: fork after turn one into a new task; the source is unchanged.
    let source_hash = sha(&transcript(&workspace, &source_session));
    let a1 = entries(&core, &task)
        .into_iter()
        .find(|entry| entry.role == MessageRole::Assistant)
        .unwrap();
    let read = snapshot(&client, &task).await;
    let forked_task = uuid::Uuid::new_v4().to_string();
    let fork = execute(
        &client,
        envelope(
            "cc10-fork",
            &task,
            Some(&read),
            serde_json::json!({
                "kind": "forkTask", "newTaskId": forked_task, "boundary": "afterCompletedTurn",
                "messageId": a1.id, "review": review(&core, &read, &task, Some(&a1.id)),
            }),
        ),
    )
    .await;
    assert_eq!(fork.status, CommandReceiptStatus::Applied, "{fork:?}");
    let fork_session = session(&core, &forked_task);
    assert!(!fork_session.is_empty() && fork_session != source_session);
    let fork_turns = native_turns(&workspace, &fork_session).await;
    assert_eq!(fork_turns.len(), 1);
    assert_ne!(fork_turns[0], c1, "fresh native uuids");
    let fork_rows = entries(&core, &forked_task);
    let fork_assistant = fork_rows
        .iter()
        .find(|entry| entry.role == MessageRole::Assistant)
        .unwrap();
    assert_eq!(
        checkpoint(&core, &forked_task, &fork_assistant.id),
        Some(fork_turns[0].clone()),
        "B3: remapped checkpoint"
    );
    assert_eq!(sha(&transcript(&workspace, &source_session)), source_hash);

    // Same-chat rewind before turn three on the same Claude session.
    let read = snapshot(&client, &task).await;
    let rewound = execute(
        &client,
        envelope(
            "cc10-rewind",
            &task,
            Some(&read),
            serde_json::json!({
                "kind": "rewindConversation", "messageId": "u3", "keepFiles": true,
                "review": review(&core, &read, &task, Some("u3")),
            }),
        ),
    )
    .await;
    assert_eq!(rewound.status, CommandReceiptStatus::Applied, "{rewound:?}");
    assert_eq!(session(&core, &task), source_session);
    assert_eq!(
        native_turns(&workspace, &source_session).await,
        vec![c1.clone(), c2.clone()]
    );
    let ids = conversation_ids(&core, &task);
    assert_eq!(ids.len(), 4, "{ids:?}");
    assert!(!ids.contains(&"u3".to_string()));
    assert!(
        core.doc_host
            .context_control(&task)
            .unwrap()
            .pending_rewind
            .is_none()
    );
    // The same session continues from the retained prefix.
    let c4 = send(&client, &core, &task, &workspace, "u4", "after rewind").await;
    assert_eq!(session(&core, &task), source_session);
    assert_eq!(
        native_turns(&workspace, &source_session).await,
        vec![c1.clone(), c2.clone(), c4.clone()]
    );

    // C5: the CLI persists the cut and dies before answering. The receipt
    // stays unknown, sends stay blocked, and Reconcile proves the cut.
    knobs(
        &workspace,
        serde_json::json!({ "rewind_conversation": { "die_after_request": true, "persist_before_dying": true } }),
    );
    let read = snapshot(&client, &task).await;
    let unknown = execute(
        &client,
        envelope(
            "cc10-rewind-unknown",
            &task,
            Some(&read),
            serde_json::json!({
                "kind": "rewindConversation", "messageId": "u4", "keepFiles": true,
                "review": review(&core, &read, &task, Some("u4")),
            }),
        ),
    )
    .await;
    assert_ne!(unknown.status, CommandReceiptStatus::Applied, "{unknown:?}");
    assert_ne!(unknown.status, CommandReceiptStatus::Failed, "{unknown:?}");
    assert_eq!(
        core.doc_host
            .context_control(&task)
            .unwrap()
            .pending_rewind
            .as_deref(),
        Some("cc10-rewind-unknown")
    );
    assert!(conversation_ids(&core, &task).contains(&"u4".to_string()));
    knobs(&workspace, serde_json::json!({}));
    let read = snapshot(&client, &task).await;
    let reconciled = execute(
        &client,
        envelope(
            "cc10-reconcile",
            &task,
            Some(&read),
            serde_json::json!({
                "kind": "reconcileConversationRewind", "rewindCommandId": "cc10-rewind-unknown",
            }),
        ),
    )
    .await;
    assert_eq!(
        reconciled.status,
        CommandReceiptStatus::Applied,
        "{reconciled:?}"
    );
    assert!(!conversation_ids(&core, &task).contains(&"u4".to_string()));
    assert_eq!(
        native_turns(&workspace, &source_session).await,
        vec![c1.clone(), c2.clone()]
    );

    // C6: a turn LUNA never saw (the session resumed in a terminal) makes
    // the histories differ; the rewind is refused before any native request.
    let c5 = send(&client, &core, &task, &workspace, "u5", "fifth prompt").await;
    let path = transcript(&workspace, &source_session);
    let leaf = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .rev()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|row| row["type"] == "system" || row["type"] == "assistant")
        .unwrap()["uuid"]
        .clone();
    let external = uuid::Uuid::new_v4().to_string();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    use std::io::Write as _;
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "type": "user", "uuid": external, "parentUuid": leaf, "sessionId": source_session,
            "isSidechain": false, "message": { "role": "user", "content": "typed in a terminal" },
        })
    )
    .unwrap();
    drop(file);
    let external_hash = sha(&path);
    let read = snapshot(&client, &task).await;
    let refused = execute(
        &client,
        envelope(
            "cc10-rewind-diverged",
            &task,
            Some(&read),
            serde_json::json!({
                "kind": "rewindConversation", "messageId": "u5", "keepFiles": true,
                "review": review(&core, &read, &task, Some("u5")),
            }),
        ),
    )
    .await;
    assert_eq!(refused.status, CommandReceiptStatus::Failed, "{refused:?}");
    assert_eq!(sha(&path), external_hash, "no native request was sent");
    assert!(
        core.doc_host
            .context_control(&task)
            .unwrap()
            .pending_rewind
            .is_none()
    );
    assert!(conversation_ids(&core, &task).contains(&"u5".to_string()));
    let _ = c5;

    // Earlier-message edit of the FIRST prompt on the forked task: a native
    // fork before turn one, rebound, then the edited prompt runs there.
    let read = snapshot(&client, &forked_task).await;
    let first_user = entries(&core, &forked_task)
        .into_iter()
        .find(|entry| entry.role == MessageRole::User)
        .unwrap();
    let edited = execute(
        &client,
        envelope(
            "cc10-edit-first",
            &forked_task,
            Some(&read),
            serde_json::json!({
                "kind": "editEarlierMessage", "messageId": first_user.id,
                "text": "edited first prompt",
                "review": review(&core, &read, &forked_task, Some(&first_user.id)),
            }),
        ),
    )
    .await;
    assert_eq!(edited.status, CommandReceiptStatus::Applied, "{edited:?}");
    settle(&client, &core, &forked_task, 1).await;
    let edited_session = session(&core, &forked_task);
    assert_ne!(edited_session, fork_session);
    let edited_rows = entries(&core, &forked_task);
    let edited_assistant = edited_rows
        .iter()
        .rev()
        .find(|entry| entry.role == MessageRole::Assistant)
        .unwrap();
    let edited_checkpoint = checkpoint(&core, &forked_task, &edited_assistant.id).unwrap();
    assert_eq!(
        native_turns(&workspace, &edited_session).await,
        vec![edited_checkpoint]
    );
    assert!(
        edited_rows
            .iter()
            .any(|entry| entry.role == MessageRole::User && text(entry) == "edited first prompt")
    );
}
