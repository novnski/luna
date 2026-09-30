//! Engine/RPC + native metadata subprocess regression for idle Codex writers.
//! All homes, thread files and writer markers are disposable synthetic state.
#![cfg(unix)]

use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls, codex::native::CodexNativeClient};
use luna_proto::{
    AgentEvent, ChatConfig, CommandReceipt, CommandReceiptStatus, DoneStatus, HarnessId, Model,
    PermissionMode, ReasoningLevel, RunRequest, SandboxLevel, SessionStatus, SteeringMode,
    TaskCommandRead,
};
use luna_rpc::{RpcClient, methods};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const IDLE_A: &str = "10000000-0000-4000-8000-000000000001";
const IDLE_B: &str = "10000000-0000-4000-8000-000000000002";
const ACTIVE_A: &str = "10000000-0000-4000-8000-000000000003";
const IDLE_STUCK: &str = "10000000-0000-4000-8000-000000000004";

fn writer(root: &Path, task: &str) -> PathBuf {
    root.join(format!("native/native-{task}.writer"))
}

const METADATA: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, sys, time
root = pathlib.Path(os.environ['CODEX_HOME'])
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request: continue
    method, params = request['method'], request.get('params', {})
    thread_id = params.get('threadId')
    result, error = {}, None
    path = root / (str(thread_id) + '.json')
    if method == 'thread/read':
        result = {'thread': json.loads(path.read_text())}
    elif method == 'thread/list':
        result = {'data': [json.loads(p.read_text()) for p in root.glob('native-*.json')
                  if json.loads(p.read_text())['archived'] == params.get('archived', False)]}
    elif method == 'thread/name/set':
        record = json.loads(path.read_text()); record['name'] = params['name']
        path.write_text(json.dumps(record))
    elif method in ('thread/archive', 'thread/unarchive'):
        with (root / 'archive-calls.jsonl').open('a') as log:
            log.write(json.dumps({'method': method, 'threadId': thread_id}) + '\n')
        mode = (root / 'archive-mode').read_text() if (root / 'archive-mode').exists() else ''
        if (root / (thread_id + '.writer')).exists():
            error = {'code': -32600, 'message': 'thread already has an active writer'}
        elif mode == 'reject':
            error = {'code': -32600, 'message': 'synthetic archive rejection'}
        else:
            if (root / 'archive-block').exists():
                (root / 'archive-entered').write_text(thread_id)
                for _ in range(500):
                    if (root / 'archive-release').exists(): break
                    time.sleep(0.01)
                else: raise RuntimeError('synthetic archive release timed out')
            record = json.loads(path.read_text()); record['archived'] = method == 'thread/archive'
            path.write_text(json.dumps(record))
            if mode == 'lost': sys.exit(0)
    frame = {'id': request['id']}
    frame['error' if error else 'result'] = error if error else result
    print(json.dumps(frame), flush=True)
"#;

struct WarmWriter {
    home: PathBuf,
}

#[async_trait]
impl Harness for WarmWriter {
    fn id(&self) -> HarnessId {
        HarnessId::Codex
    }
    fn display_name(&self) -> &str {
        "Synthetic native writer"
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        request: RunRequest,
        controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let id = request
            .resume
            .clone()
            .unwrap_or_else(|| format!("native-{}", request.prompt));
        let active = request.prompt == ACTIVE_A;
        let stuck = request.prompt == IDLE_STUCK;
        let writer = self.home.join(format!("{id}.writer"));
        std::fs::write(&writer, b"exclusive writer")?;
        std::fs::write(
            self.home.join(format!("{id}.json")),
            serde_json::to_vec(&serde_json::json!({
                "id":id, "name":format!("[LUNA] Archive {}", request.prompt),
                "cwd":request.cwd, "status":{"type":if active {"active"} else {"idle"}},
                "archived":false, "ephemeral":false,
            }))
            .unwrap(),
        )?;
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        tokio::spawn(async move {
            let mut steering = controls.steering;
            let _ = tx
                .send(Ok(AgentEvent::SessionStarted {
                    harness: HarnessId::Codex,
                    model: "synthetic".into(),
                    tools: vec![],
                    cwd: request.cwd,
                    session_id: id.clone(),
                    assistant_message_id: format!("answer-{id}"),
                }))
                .await;
            if !active {
                let _ = tx
                    .send(Ok(AgentEvent::TextDelta {
                        text: "Synthetic complete".into(),
                    }))
                    .await;
                let _ = tx
                    .send(Ok(AgentEvent::Done {
                        status: DoneStatus::Completed,
                        result: None,
                        error: None,
                        session_id: Some(id.clone()),
                    }))
                    .await;
            }
            tokio::select! {
                _ = steering.recv() => {
                    if stuck { controls.interrupt.cancelled().await; }
                },
                _ = controls.interrupt.cancelled() => {
                    // Real adapter cancellation can report Done before child
                    // teardown. Archive must wait for graceful stream EOF.
                    let _ = tx.send(Ok(AgentEvent::Done {status:DoneStatus::Interrupted,
                        result:None, error:None, session_id:Some(id)})).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
            let _ = std::fs::remove_file(writer);
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

fn assemble(root: &Path) -> EngineCore {
    use std::os::unix::fs::PermissionsExt;
    let home = root.join("native");
    std::fs::create_dir_all(&home).unwrap();
    let executable = root.join("metadata.py");
    std::fs::write(&executable, METADATA).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(WarmWriter { home: home.clone() }));
    let accounts = AgentAccountsConfig {
        data_dir: root.join("accounts"),
        claude_config_dir: root.join("claude"),
        claude_config_file: root.join("claude.json"),
        codex_home: home.clone(),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_injected_codex_metadata_client(
        EngineProfile::development(&root.join("data"), "archive-org", "archive-user"),
        Arc::new(registry),
        HarnessId::Codex,
        None,
        accounts,
        CodexNativeClient::new(home).with_executable(executable),
    )
    .unwrap()
}

async fn start(core: &EngineCore, root: &Path, id: &str) {
    let client = luna_rpc::memory_client(core.rpc_service());
    let config = ChatConfig {
        harness: HarnessId::Codex,
        provider_account_id: None,
        model: Some("synthetic".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: vec![],
    };
    let cwd = root.canonicalize().unwrap().to_string_lossy().to_string();
    core.workspace
        .create_space("space-idle-archive", &core.device_id, &cwd, None, false)
        .expect("create project");
    client
        .call(
            methods::MUTATE,
            serde_json::json!({"op":"createChat", "chatId":id,
        "spaceId":"space-idle-archive", "cwd":cwd, "config":config}),
        )
        .await
        .unwrap();
    core.workspace
        .rename_chat(id, &format!("Archive {id}"))
        .unwrap();
    core.sessions
        .dispatch(
            id,
            HarnessId::Codex,
            RunRequest {
                input_items: Vec::new(),
                prompt: id.into(),
                harness: Some(HarnessId::Codex),
                provider_account_id: None,
                model: Some("synthetic".into()),
                reasoning: None,
                model_options: Default::default(),
                cwd,
                sandbox: SandboxLevel::WorkspaceWrite,
                auto_approve: false,
                permission_mode: PermissionMode::Ask,
                parent_permission_ceiling: None,
                environment_permission_ceiling: None,
                permission_policy: None,
                resume: None,
                attachments: vec![],
                worktree: None,
            },
            Some(format!("input-{id}")),
        )
        .await
        .unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let bound = core
                .task_identity
                .view(id)
                .and_then(|v| v.current_binding)
                .is_some_and(|b| b.provider_task_id == format!("native-{id}"));
            let ready = core.sessions.session_status(id).is_some_and(|s| {
                if id == ACTIVE_A {
                    s.status == SessionStatus::Working
                } else {
                    s.status == SessionStatus::Idle && s.last_completed_turn.is_some()
                }
            });
            if bound && ready && core.sessions.has_live_run(id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        ready.is_ok(),
        "synthetic setup: status={:?}, identity={:?}, transcript={:?}",
        core.sessions.session_status(id),
        core.task_identity.view(id),
        core.doc_host
            .open(id)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
    );
}

async fn archive(
    client: &RpcClient,
    task: &str,
    command: &str,
) -> (serde_json::Value, CommandReceipt) {
    let read: TaskCommandRead = client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({"taskId":task}),
        )
        .await
        .unwrap();
    let params = serde_json::json!({"commandId":command, "taskId":task,
        "actorId":"local-user", "clientId":"idle-archive-qa", "clientSessionId":"synthetic",
        "expectedTaskRevision":read.snapshot.task_revision,
        "expectedBindingGeneration":read.snapshot.binding_generation,
        "intent":{"kind":"setArchived", "archived":true}});
    let result = client
        .call_as(methods::EXECUTE_TASK_COMMAND, params.clone())
        .await
        .unwrap();
    (params, result)
}

fn calls(root: &Path) -> usize {
    std::fs::read_to_string(root.join("native/archive-calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .count()
}

#[tokio::test]
async fn idle_archive_waits_for_only_the_target_writer_and_preserves_identity_on_restart() {
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path());
    start(&core, root.path(), IDLE_A).await;
    start(&core, root.path(), IDLE_B).await;
    let binding = core
        .task_identity
        .view(IDLE_A)
        .unwrap()
        .current_binding
        .unwrap();
    let transcript = core
        .doc_host
        .open(IDLE_A)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    let (command, receipt) = archive(&client, IDLE_A, "archive-idle-a").await;
    assert_eq!(receipt.status, CommandReceiptStatus::Applied, "{receipt:?}");
    assert!(!writer(root.path(), IDLE_A).exists());
    assert!(writer(root.path(), IDLE_B).exists());
    assert!(!core.sessions.has_live_run(IDLE_A));
    assert!(core.sessions.has_live_run(IDLE_B));
    assert!(core.workspace.chat(IDLE_A).unwrap().unwrap().archived);
    assert_eq!(
        core.doc_host
            .open(IDLE_A)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap(),
        transcript
    );
    let after = core
        .task_identity
        .view(IDLE_A)
        .unwrap()
        .current_binding
        .unwrap();
    assert_eq!(after.binding_id, binding.binding_id);
    assert_eq!(after.provider_task_id, binding.provider_task_id);
    assert_eq!(after.generation, binding.generation);
    assert_eq!(calls(root.path()), 1);
    drop(client);
    core.shutdown().await;
    drop(core);
    let reopened = assemble(root.path());
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let replay: CommandReceipt = client
        .call_as(methods::EXECUTE_TASK_COMMAND, command)
        .await
        .unwrap();
    assert_eq!(replay.status, CommandReceiptStatus::Applied);
    assert_eq!(
        calls(root.path()),
        1,
        "restart/retry must not repeat archive"
    );
    assert!(reopened.workspace.chat(IDLE_A).unwrap().unwrap().archived);
    let after = reopened
        .task_identity
        .view(IDLE_A)
        .unwrap()
        .current_binding
        .unwrap();
    assert_eq!(after.binding_id, binding.binding_id);
    assert_eq!(after.provider_task_id, binding.provider_task_id);
    reopened.shutdown().await;
}

#[tokio::test]
async fn active_archive_preserves_both_the_active_turn_and_other_warm_writer() {
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path());
    start(&core, root.path(), ACTIVE_A).await;
    start(&core, root.path(), IDLE_B).await;
    let turn = core.sessions.active_turn_id(ACTIVE_A).unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    let (_, receipt) = archive(&client, ACTIVE_A, "archive-active-a").await;
    assert_eq!(receipt.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        receipt.reason.unwrap().code,
        luna_proto::CommandReasonCode::ActiveTurnConflict
    );
    assert_eq!(
        core.sessions.active_turn_id(ACTIVE_A).as_deref(),
        Some(turn.as_str())
    );
    assert!(writer(root.path(), ACTIVE_A).exists());
    assert!(writer(root.path(), IDLE_B).exists());
    assert_eq!(calls(root.path()), 0);
    assert!(!core.workspace.chat(ACTIVE_A).unwrap().unwrap().archived);
    core.shutdown().await;
}

#[tokio::test]
async fn queued_dispatch_waiting_on_archive_rechecks_the_committed_state() {
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path());
    start(&core, root.path(), IDLE_A).await;
    let transcript = core
        .doc_host
        .open(IDLE_A)
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    std::fs::write(
        root.path().join("native/archive-block"),
        b"pause metadata write",
    )
    .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    let archiving =
        tokio::spawn(async move { archive(&client, IDLE_A, "archive-with-waiting-queue").await });
    tokio::time::timeout(Duration::from_secs(4), async {
        while !root.path().join("native/archive-entered").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("archive reaches provider only after writer teardown");
    assert!(!core.sessions.has_live_run(IDLE_A));
    assert!(!writer(root.path(), IDLE_A).exists());

    // The real automatic drain freezes a request and claims this row before
    // SessionsEngine obtains its task transition gate. The archive holds it.
    let queue_id = core
        .doc_host
        .queue_message(IDLE_A, "RACE_FOLLOW_UP", vec![])
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if core
                .doc_host
                .queued_message(IDLE_A, &queue_id)
                .unwrap()
                .is_some_and(|row| {
                    matches!(row.status, luna_doc::QueueMessageStatus::Claimed { .. })
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("queue drain is waiting after its durable claim");
    std::fs::write(
        root.path().join("native/archive-release"),
        b"commit archive",
    )
    .unwrap();
    let (_, receipt) = archiving.await.unwrap();
    assert_eq!(receipt.status, CommandReceiptStatus::Applied, "{receipt:?}");
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let held = core
                .doc_host
                .queued_message(IDLE_A, &queue_id)
                .unwrap()
                .is_some_and(|row| {
                    matches!(
                        row.status,
                        luna_doc::QueueMessageStatus::DeliveryUnknown { .. }
                    )
                });
            if held || core.sessions.has_live_run(IDLE_A) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("waiting dispatch observes archive result");
    assert!(
        !core.sessions.has_live_run(IDLE_A),
        "a request prepared before archive started afterward"
    );
    assert!(!writer(root.path(), IDLE_A).exists());
    assert!(core.workspace.chat(IDLE_A).unwrap().unwrap().archived);
    assert_eq!(
        core.doc_host
            .open(IDLE_A)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap(),
        transcript
    );
    assert!(matches!(
        core.doc_host
            .queued_message(IDLE_A, &queue_id)
            .unwrap()
            .unwrap()
            .status,
        luna_doc::QueueMessageStatus::DeliveryUnknown { .. }
    ));
    core.shutdown().await;
}

#[tokio::test]
async fn unreleased_writer_blocks_archive_before_any_native_mutation() {
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path());
    start(&core, root.path(), IDLE_STUCK).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let (_, receipt) = tokio::time::timeout(
        Duration::from_secs(8),
        archive(&client, IDLE_STUCK, "archive-stuck"),
    )
    .await
    .expect("bounded retirement");
    assert_ne!(receipt.status, CommandReceiptStatus::Applied);
    assert!(
        receipt.reason.is_some(),
        "retirement failure must be visible"
    );
    assert_eq!(
        calls(root.path()),
        0,
        "archive must wait for writer release"
    );
    assert!(!core.workspace.chat(IDLE_STUCK).unwrap().unwrap().archived);
    assert!(writer(root.path(), IDLE_STUCK).exists());
    core.shutdown().await;
}

#[tokio::test]
async fn failed_or_unknown_archive_never_commits_or_replays_the_same_command() {
    for mode in ["reject", "lost"] {
        let root = tempfile::tempdir().unwrap();
        let core = assemble(root.path());
        start(&core, root.path(), IDLE_A).await;
        let binding = core
            .task_identity
            .view(IDLE_A)
            .unwrap()
            .current_binding
            .unwrap();
        std::fs::write(root.path().join("native/archive-mode"), mode).unwrap();
        let client = luna_rpc::memory_client(core.rpc_service());
        let (command, receipt) = archive(&client, IDLE_A, "archive-uncertain").await;
        assert_eq!(
            receipt.status,
            CommandReceiptStatus::Pending,
            "{mode}: {receipt:?}"
        );
        assert_eq!(
            receipt.reason.unwrap().code,
            luna_proto::CommandReasonCode::ProviderAcceptanceUnknown
        );
        assert!(!core.workspace.chat(IDLE_A).unwrap().unwrap().archived);
        assert!(!writer(root.path(), IDLE_A).exists());
        let replay: CommandReceipt = client
            .call_as(methods::EXECUTE_TASK_COMMAND, command.clone())
            .await
            .unwrap();
        assert_eq!(replay.status, CommandReceiptStatus::Pending);
        assert_eq!(calls(root.path()), 1);
        drop(client);
        core.shutdown().await;
        drop(core);
        let reopened = assemble(root.path());
        let client = luna_rpc::memory_client(reopened.rpc_service());
        let replay: CommandReceipt = client
            .call_as(methods::EXECUTE_TASK_COMMAND, command)
            .await
            .unwrap();
        assert_eq!(replay.status, CommandReceiptStatus::Pending);
        assert_eq!(calls(root.path()), 1);
        assert!(!reopened.workspace.chat(IDLE_A).unwrap().unwrap().archived);
        let after = reopened
            .task_identity
            .view(IDLE_A)
            .unwrap()
            .current_binding
            .unwrap();
        assert_eq!(after.binding_id, binding.binding_id);
        assert_eq!(after.provider_task_id, binding.provider_task_id);
        if mode == "reject" {
            std::fs::remove_file(root.path().join("native/archive-mode")).unwrap();
            let (_, retry) = archive(&client, IDLE_A, "archive-deliberate-new-retry").await;
            assert_eq!(retry.status, CommandReceiptStatus::Applied, "{retry:?}");
            assert_eq!(calls(root.path()), 2);
        }
        reopened.shutdown().await;
    }
}

#[tokio::test]
async fn disconnected_native_archive_does_not_leave_an_unexplained_pending_receipt() {
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path());
    start(&core, root.path(), IDLE_A).await;
    std::fs::write(
        root.path().join("native/archive-block"),
        b"hold provider reply",
    )
    .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    let read: TaskCommandRead = client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({"taskId":IDLE_A}),
        )
        .await
        .unwrap();
    let command = serde_json::json!({
        "commandId":"archive-disconnected", "taskId":IDLE_A,
        "actorId":"local-user", "clientId":"idle-archive-qa", "clientSessionId":"synthetic",
        "expectedTaskRevision":read.snapshot.task_revision,
        "expectedBindingGeneration":read.snapshot.binding_generation,
        "intent":{"kind":"setArchived", "archived":true}
    });
    let dispatched = command.clone();
    let archiving = tokio::spawn(async move {
        client
            .call_as::<CommandReceipt>(methods::EXECUTE_TASK_COMMAND, dispatched)
            .await
    });
    tokio::time::timeout(Duration::from_secs(4), async {
        while !root.path().join("native/archive-entered").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("native archive reached the provider before disconnect");
    archiving.abort();
    assert!(archiving.await.unwrap_err().is_cancelled());

    let reader = luna_rpc::memory_client(core.rpc_service());
    let receipt: CommandReceipt = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let receipt: Option<CommandReceipt> = reader
                .call_as(
                    methods::GET_TASK_COMMAND_RECEIPT,
                    serde_json::json!({"commandId":"archive-disconnected"}),
                )
                .await
                .unwrap();
            if let Some(receipt) = receipt
                && receipt.reason.is_some()
            {
                break receipt;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("canceled provider archive must explain its pending receipt");
    assert_eq!(receipt.status, CommandReceiptStatus::Pending);
    assert_eq!(
        receipt.reason.unwrap().code,
        luna_proto::CommandReasonCode::ProviderAcceptanceUnknown
    );
    assert!(!core.workspace.chat(IDLE_A).unwrap().unwrap().archived);
    let replay: CommandReceipt = reader
        .call_as(methods::EXECUTE_TASK_COMMAND, command)
        .await
        .unwrap();
    assert_eq!(replay.status, CommandReceiptStatus::Pending);
    assert_eq!(
        calls(root.path()),
        1,
        "the canceled provider call must not replay"
    );
    core.shutdown().await;
}
