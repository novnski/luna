//! RM-04 at the engine boundary: a paired device session's task commands
//! (`ExecuteRemoteTaskCommand`) through the shipped per-connection remote
//! guard (`RemoteRpcService`, the one every direct-gateway socket runs) over
//! the real engine, its durable receipt store, queue drain and project
//! setup. Provider runs reach a recording harness at the real provider
//! boundary; every profile, folder, repository and credential is synthetic.
//! Failure contract: docs/pending-tasks/control-this-mac/rm-04/README.md.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile};
use luna_proto::remote_command::{
    RemoteFollowUp, RemoteNewWorktree, RemotePermissionMode, RemoteTaskCommand, RemoteTaskIntent,
    remote_actor_id,
};
use luna_proto::{
    CommandReasonCode, CommandReceipt, CommandReceiptStatus, HarnessId, PermissionMode,
    ProjectAction, ProjectCommandRun, ProjectCommandStatus, TaskCommandRead,
};
use luna_rpc::remote_auth::{
    AuthenticateRemote, IssuedPairing, IssuedSession, RedeemPairing, remote_command_identity,
};
use luna_rpc::{RpcClient, methods};
use serde_json::{Value, json};

mod remote_provider;
use remote_provider::{ALL, Provider, Seen};

struct Host {
    core: EngineCore,
    local: RpcClient,
    provider: Provider,
}

impl Host {
    fn open(data: &Path, hold: bool) -> Self {
        Self::open_with(data, hold, HarnessId::Mock)
    }

    /// With `provider` as the engine's provider for new tasks.
    fn open_with(data: &Path, hold: bool, provider_id: HarnessId) -> Self {
        let provider = Provider::new();
        let accounts = AgentAccountsConfig {
            data_dir: data.join("provider-state"),
            claude_config_dir: data.join("synthetic-claude"),
            claude_config_file: data.join("synthetic-claude.json"),
            codex_home: data.join("synthetic-codex"),
        };
        accounts
            .disable_claude_keychain_fallback()
            .expect("seal synthetic Claude home");
        let core =
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                EngineProfile::development(&data.join("profile"), "rm04-org", "rm04-user"),
                provider.registry(hold),
                provider_id,
                None,
                accounts,
            )
            .expect("assemble the RM-04 profile");
        let local = luna_rpc::memory_client(core.rpc_service());
        Self {
            core,
            local,
            provider,
        }
    }

    /// End the held turn of `prompt` (now or whenever it starts).
    fn finish(&self, prompt: &str) {
        self.provider.finish(prompt);
    }

    /// One project of this host on `folder`.
    fn project(&self, id: &str, folder: &Path, git: bool) {
        self.core
            .workspace
            .create_space(
                id,
                &self.core.device_id,
                folder.to_str().unwrap(),
                None,
                git,
            )
            .expect("create the project");
    }

    /// A desktop-created task in `project`.
    async fn desktop_task(&self, task: &str, project: &str) {
        self.local
            .call(
                methods::MUTATE,
                json!({"op": "createChat", "chatId": task, "spaceId": project}),
            )
            .await
            .expect("the desktop creates a task");
        self.core
            .workspace
            .rename_chat(task, "RM04 desktop")
            .unwrap();
    }

    /// A paired, authenticated remote connection with `scopes`.
    async fn pair(&self, label: &str, scopes: &[&str]) -> Phone {
        let issued: IssuedPairing = self
            .local
            .call_as(
                methods::CREATE_REMOTE_PAIRING,
                json!({"label": label, "scopes": scopes,
                    "pairingTtlSecs": 300, "sessionTtlSecs": 3600}),
            )
            .await
            .expect("the host issues a pairing");
        let rpc =
            luna_rpc::memory_client(self.core.remote_rpc_service().expect("remote authority"));
        let session: IssuedSession = rpc
            .redeem_remote_pairing(RedeemPairing {
                target: issued.target.clone(),
                credential: issued.credential.clone(),
                client_label: label.into(),
            })
            .await
            .expect("the phone redeems its pairing");
        let phone = Phone { rpc, session };
        phone.authenticate().await;
        phone
    }

    /// A new connection for an already paired session (a reconnect).
    async fn connect(&self, session: &IssuedSession) -> Phone {
        let rpc =
            luna_rpc::memory_client(self.core.remote_rpc_service().expect("remote authority"));
        let phone = Phone {
            rpc,
            session: session.clone(),
        };
        phone.authenticate().await;
        phone
    }

    fn receipt(&self, command_id: &str) -> Option<CommandReceipt> {
        self.core
            .doc_host
            .task_command_receipt(command_id)
            .expect("the receipt store reads")
    }

    fn runs_of(&self, prompt: &str) -> Vec<Seen> {
        self.provider.runs_of(prompt)
    }

    async fn ran(&self, prompt: &str) -> Seen {
        self.provider.ran(prompt).await
    }

    fn user_entry_ids(&self, task_id: &str) -> Vec<String> {
        self.core
            .doc_host
            .open(task_id)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
            .into_iter()
            .filter(|entry| entry.role == luna_doc::MessageRole::User)
            .map(|entry| entry.id)
            .collect()
    }

    async fn close(self) {
        self.finish(ALL);
        drop(self.local);
        self.core.shutdown().await;
    }
}

struct Phone {
    rpc: RpcClient,
    session: IssuedSession,
}

impl Phone {
    async fn authenticate(&self) {
        self.rpc
            .authenticate_remote(AuthenticateRemote {
                target: self.session.target.clone(),
                session_token: self.session.session_token.clone(),
            })
            .await
            .expect("the paired session authenticates");
    }

    fn actor(&self) -> String {
        remote_actor_id(&self.session.session.id)
    }

    async fn execute(&self, command: &RemoteTaskCommand) -> Result<CommandReceipt, String> {
        self.rpc
            .call_as(
                methods::EXECUTE_REMOTE_TASK_COMMAND,
                serde_json::to_value(command).unwrap(),
            )
            .await
            .map_err(|error| error.to_string())
    }

    async fn execute_raw(&self, params: Value) -> Result<Value, String> {
        self.rpc
            .call(methods::EXECUTE_REMOTE_TASK_COMMAND, params)
            .await
            .map_err(|error| error.to_string())
    }

    async fn receipt(&self, command_id: &str) -> Option<CommandReceipt> {
        self.rpc
            .call_as(
                methods::GET_TASK_COMMAND_RECEIPT,
                json!({"commandId": command_id}),
            )
            .await
            .expect("tasks-read reads receipts")
    }

    async fn snapshot(&self, task_id: &str) -> TaskCommandRead {
        self.rpc
            .call_as(
                methods::GET_TASK_COMMAND_SNAPSHOT,
                json!({"taskId": task_id}),
            )
            .await
            .expect("tasks-read reads snapshots")
    }

    /// A send fenced as the phone's core fences one just before its first
    /// send: by the task's provider binding generation, if it has one.
    async fn send(&self, task_id: &str, text: &str) -> RemoteTaskCommand {
        let current = self.snapshot(task_id).await.snapshot;
        let mut command = send(task_id, text);
        command.expected_binding_generation = current.binding_generation;
        command
    }

    /// A control fenced from the task's current snapshot, as the phone's
    /// core fences a live control when the user acts.
    async fn fenced(&self, task_id: &str, intent: RemoteTaskIntent) -> RemoteTaskCommand {
        let current = self.snapshot(task_id).await.snapshot;
        let mut command = command(task_id, intent);
        command.expected_binding_generation = current.binding_generation;
        command.expected_turn_id = current.active_turn_id;
        command
    }
}

const OPERATE: &[&str] = &["environment-read", "tasks-read", "tasks-operate"];
const READ_ONLY: &[&str] = &["environment-read", "tasks-read"];

fn command(task_id: &str, intent: RemoteTaskIntent) -> RemoteTaskCommand {
    RemoteTaskCommand {
        command_id: uuid::Uuid::new_v4().to_string(),
        task_id: task_id.into(),
        expected_task_revision: None,
        expected_binding_generation: None,
        expected_turn_id: None,
        issued_at_ms: None,
        intent,
    }
}

fn create(project: &str) -> RemoteTaskCommand {
    create_with(project, None)
}

fn create_with(project: &str, mode: Option<RemotePermissionMode>) -> RemoteTaskCommand {
    command(
        &uuid::Uuid::new_v4().to_string(),
        RemoteTaskIntent::CreateTask {
            project_id: project.into(),
            permission_mode: mode,
        },
    )
}

/// A plain send; its message id is its command id, as the phone mints it.
fn send(task_id: &str, text: &str) -> RemoteTaskCommand {
    let id = uuid::Uuid::new_v4().to_string();
    RemoteTaskCommand {
        command_id: id.clone(),
        task_id: task_id.into(),
        expected_task_revision: None,
        expected_binding_generation: None,
        expected_turn_id: None,
        issued_at_ms: None,
        intent: RemoteTaskIntent::Send {
            message_id: id,
            text: text.into(),
            follow_up: RemoteFollowUp::Queue,
            new_worktree: None,
        },
    }
}

fn send_in_new_worktree(task_id: &str, text: &str) -> RemoteTaskCommand {
    let mut command = send(task_id, text);
    if let RemoteTaskIntent::Send { new_worktree, .. } = &mut command.intent {
        *new_worktree = Some(RemoteNewWorktree { base: None });
    }
    command
}

/// Polls a state the engine reaches by itself (a receipt settling, a run
/// starting); fails with `what` after the bound.
async fn until<T>(what: &str, within: Duration, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn settled(host: &Host, command_id: &str) -> CommandReceipt {
    until(
        &format!("receipt {command_id} to settle"),
        Duration::from_secs(20),
        || {
            host.receipt(command_id)
                .filter(|receipt| receipt.status.is_terminal())
        },
    )
    .await
}

fn folder(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    path.canonicalize().unwrap()
}

/// F1–F9, F11, F21 (RP-10, RP-11, RP-12): admission is typed, scoped and
/// bound to the authenticated principal and payload before any provider
/// work. A read-only session, the local owner's IPC, forged actor fields,
/// engine intents outside the closed set and targets this host does not list
/// are refused with no receipt. A created task and a sent message carry the
/// session as actor and the payload identity; the same command again (a lost
/// reply, a reconnect, resends) is the original receipt; identical first
/// sends racing each other are one receipt; each runs once. The same id with
/// another payload is `commandIdConflict`; another session cannot claim the
/// id; a revoked session is refused before admission. A created task's
/// permission mode is never legacy Ask: Auto by default where the provider
/// offers it, Full on request, explicit Ask malformed, and no default (a
/// typed `permissionModeRequired` receipt) for a provider without Auto.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paired_session_admits_typed_commands_bound_to_its_principal_and_payload() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::open(&root.path().join("data"), true);
    let project = folder(root.path(), "rm04-project");
    let hidden = folder(root.path(), "rm04-removed-project");
    host.project("rm04-space", &project, false);
    host.project("rm04-removed-space", &hidden, false);
    host.desktop_task("rm04-hidden-task", "rm04-removed-space")
        .await;
    assert!(
        host.core
            .workspace
            .remove_space("rm04-removed-space")
            .unwrap()
    );
    let reader = host.pair("rm04 reader", READ_ONLY).await;
    let phone = host.pair("rm04 phone", OPERATE).await;
    let other = host.pair("rm04 other phone", OPERATE).await;

    // F1 (RP-12): a read-only grant cannot mutate, and nothing is recorded.
    let refused = create("rm04-space");
    let denied = reader.execute(&refused).await.unwrap_err();
    assert!(denied.contains("lacks required scope"), "{denied}");
    assert!(host.receipt(&refused.command_id).is_none());

    // F2: the local owner's IPC carries no principal.
    let local = host
        .local
        .call(
            methods::EXECUTE_REMOTE_TASK_COMMAND,
            serde_json::to_value(&refused).unwrap(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        local.contains("only for a paired device session"),
        "{local}"
    );
    assert!(host.receipt(&refused.command_id).is_none());

    // F3 (RP-12): forged actor fields and engine intents outside the closed
    // set are refused as malformed, before admission.
    let mut forged = serde_json::to_value(create("rm04-space")).unwrap();
    forged["actorId"] = json!("local-user");
    forged["clientId"] = json!("local-ui");
    let forged_id = forged["commandId"].as_str().unwrap().to_owned();
    let error = phone.execute_raw(forged).await.unwrap_err();
    assert!(error.contains("unknown field"), "{error}");
    assert!(host.receipt(&forged_id).is_none());
    for intent in [
        json!({"kind": "rename", "title": "taken over"}),
        json!({"kind": "setPermission", "mode": "full"}),
        json!({"kind": "delete"}),
        json!({"kind": "createChildChat", "parentTaskId": "x", "request": {}}),
        json!({"kind": "session", "command": {"kind": "interrupt"}}),
    ] {
        let id = uuid::Uuid::new_v4().to_string();
        let error = phone
            .execute_raw(json!({"commandId": id, "taskId": "rm04-any", "intent": intent}))
            .await
            .unwrap_err();
        assert!(error.contains("bad params"), "{intent}: {error}");
        assert!(host.receipt(&id).is_none(), "{intent}");
    }

    // F4: only this host's listed projects and tasks.
    for project in ["rm04-unknown-space", "rm04-removed-space"] {
        let wrong = create(project);
        let error = phone.execute(&wrong).await.unwrap_err();
        assert!(
            error.contains("project is not on this Mac"),
            "{project}: {error}"
        );
        assert!(host.receipt(&wrong.command_id).is_none());
    }
    for task in ["rm04-unknown-task", "rm04-hidden-task"] {
        let wrong = send(task, "rm04: never sent");
        let error = phone.execute(&wrong).await.unwrap_err();
        assert!(error.contains("task is not on this Mac"), "{task}: {error}");
        assert!(host.receipt(&wrong.command_id).is_none());
    }
    host.desktop_task("rm04-desktop-task", "rm04-space").await;
    let mut taken = create("rm04-space");
    taken.task_id = "rm04-desktop-task".into();
    let error = phone.execute(&taken).await.unwrap_err();
    assert!(
        error.contains("UUID") || error.contains("already exists"),
        "{error}"
    );
    assert!(host.receipt(&taken.command_id).is_none());
    assert!(host.runs_of("rm04: never sent").is_empty());

    // A created task: the session is its actor, the payload its identity.
    let created = create("rm04-space");
    let task = created.task_id.clone();
    let receipt = phone.execute(&created).await.unwrap();
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
    assert_eq!(receipt.intent, "createChat");
    assert_eq!(receipt.actor_id, phone.actor());
    assert_eq!(receipt.client_id, "luna-remote");
    assert_eq!(receipt.client_session_id, phone.session.session.id);
    assert_eq!(receipt.payload_hash, remote_command_identity(&created));
    let chat = host
        .core
        .workspace
        .chat(&task)
        .unwrap()
        .expect("the task exists");
    assert_eq!(chat.space_id.as_deref(), Some("rm04-space"));
    assert_eq!(chat.cwd.as_deref(), project.to_str());
    // F24: without a mode, a provider that offers Auto gets Auto, recorded in
    // the task's configuration for the desktop; never legacy Ask.
    let config = chat
        .config
        .expect("a phone-created task records its config");
    assert_eq!(
        (config.harness, config.permission_mode),
        (HarnessId::Mock, PermissionMode::Auto)
    );
    host.core
        .workspace
        .rename_chat(&task, "RM04 titled")
        .unwrap();
    let full = create_with("rm04-space", Some(RemotePermissionMode::Full));
    assert_eq!(
        phone.execute(&full).await.unwrap().status,
        CommandReceiptStatus::Applied
    );
    let full_config = host
        .core
        .workspace
        .chat(&full.task_id)
        .unwrap()
        .unwrap()
        .config
        .unwrap();
    assert_eq!(full_config.permission_mode, PermissionMode::Full);
    // An explicit legacy Ask is malformed: refused, nothing recorded.
    let ask = uuid::Uuid::new_v4().to_string();
    let error = phone
        .execute_raw(
            json!({"commandId": ask, "taskId": uuid::Uuid::new_v4().to_string(),
            "intent": {"kind": "createTask", "projectId": "rm04-space", "permissionMode": "ask"}}),
        )
        .await
        .unwrap_err();
    assert!(error.contains("bad params"), "{error}");
    assert!(host.receipt(&ask).is_none());

    // F6/F9 (RP-10): a retried create is its original receipt, though the
    // task now exists; a lost reply reads back by id.
    let again = phone.execute(&created).await.unwrap();
    assert_eq!(again, host.receipt(&created.command_id).unwrap());
    assert_eq!(again.status, CommandReceiptStatus::Applied);
    assert_eq!(
        phone.receipt(&created.command_id).await,
        Some(again.clone())
    );

    // F7: the same id with another payload is refused; the original stays.
    let mut changed = created.clone();
    changed.intent = RemoteTaskIntent::CreateTask {
        project_id: "rm04-other-space".into(),
        permission_mode: None,
    };
    let conflict = phone.execute(&changed).await.unwrap();
    assert_eq!(conflict.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        conflict.reason.as_ref().map(|reason| reason.code),
        Some(CommandReasonCode::CommandIdConflict)
    );
    assert_eq!(host.receipt(&created.command_id).unwrap(), again);

    // F8: another session cannot claim this session's command id.
    let claimed = other.execute(&created).await.unwrap_err();
    assert!(claimed.contains("belongs to another client"), "{claimed}");

    // RP-11 (second half): a paired send is its own authorized receipt and
    // runs; to an idle task it starts the next turn, as a desktop send does,
    // and its message id becomes the transcript message id.
    let first = phone.send(&task, "rm04: first").await;
    let started = phone.execute(&first).await.unwrap();
    assert_eq!(started.intent, "run");
    assert_eq!(started.actor_id, phone.actor());
    assert_eq!(started.payload_hash, remote_command_identity(&first));
    host.ran("rm04: first").await;
    assert_eq!(
        settled(&host, &first.command_id).await.status,
        CommandReceiptStatus::Applied
    );

    // F6 (RP-10): resends of the same command, on this and a new connection,
    // are its one receipt.
    let reconnected = host.connect(&phone.session).await;
    for resent in [
        phone.execute(&first).await,
        reconnected.execute(&first).await,
    ] {
        let resent = resent.unwrap();
        assert_eq!(resent, host.receipt(&first.command_id).unwrap());
        assert_eq!(resent.status, CommandReceiptStatus::Applied);
    }

    // F11: four identical first sends racing each other are one receipt. The
    // first turn runs, so each is queued behind it.
    let raced = phone.send(&task, "rm04: raced").await;
    let racing = futures::future::join_all([
        phone.execute(&raced),
        reconnected.execute(&raced),
        phone.execute(&raced),
        reconnected.execute(&raced),
    ])
    .await;
    for receipt in racing {
        let receipt = receipt.unwrap();
        assert_eq!(receipt.command_id, raced.command_id);
        assert_eq!(receipt.intent, "queueMessage");
        assert_eq!(receipt.status, CommandReceiptStatus::Applied);
        assert_eq!(receipt.actor_id, phone.actor());
    }

    // The queue runs FIFO behind the held first turn: once `second` runs, a
    // duplicate queued by any resend or racer would already have run.
    let second = reconnected.send(&task, "rm04: second").await;
    reconnected.execute(&second).await.unwrap();
    host.finish("rm04: first");
    host.ran("rm04: raced").await;
    host.finish("rm04: raced");
    host.ran("rm04: second").await;
    for prompt in ["rm04: first", "rm04: raced", "rm04: second"] {
        assert_eq!(
            host.runs_of(prompt).len(),
            1,
            "one provider run of {prompt}"
        );
    }
    let users = host.user_entry_ids(&task);
    for sent in [&first, &raced, &second] {
        assert_eq!(
            users.iter().filter(|id| **id == sent.command_id).count(),
            1,
            "one transcript message per command: {users:?}"
        );
    }

    // F5: a revoked grant is refused before any receipt exists.
    host.local
        .call(
            methods::REVOKE_REMOTE_SESSION,
            json!({"id": phone.session.session.id}),
        )
        .await
        .unwrap();
    let late = send(&task, "rm04: after revoke");
    let error = reconnected.execute(&late).await.unwrap_err();
    assert!(error.contains("revoked or expired"), "{error}");
    assert!(host.receipt(&late.command_id).is_none());
    host.close().await;

    // F24: a provider without an automatic reviewer (OpenCode) has no safe
    // default. A create without a mode is refused with a typed receipt
    // before anything is created or run; Full access is accepted.
    let no_auto = Host::open_with(&root.path().join("no-auto"), true, HarnessId::Opencode);
    no_auto.project("rm04-space", &project, false);
    let phone = no_auto.pair("rm04 phone", OPERATE).await;
    let unset = create("rm04-space");
    let refused = phone.execute(&unset).await.unwrap();
    assert_eq!(refused.status, CommandReceiptStatus::Rejected);
    assert_eq!(
        refused.reason.as_ref().map(|reason| reason.code),
        Some(CommandReasonCode::PermissionModeRequired)
    );
    assert!(
        no_auto
            .core
            .workspace
            .chat(&unset.task_id)
            .unwrap()
            .is_none()
    );
    let again = phone.execute(&unset).await.unwrap();
    assert_eq!(
        (again.status, again.reason.map(|reason| reason.code)),
        (
            CommandReceiptStatus::Rejected,
            Some(CommandReasonCode::PermissionModeRequired)
        ),
        "the same answer again"
    );
    let full = create_with("rm04-space", Some(RemotePermissionMode::Full));
    assert_eq!(
        phone.execute(&full).await.unwrap().status,
        CommandReceiptStatus::Applied
    );
    let config = no_auto
        .core
        .workspace
        .chat(&full.task_id)
        .unwrap()
        .unwrap()
        .config
        .unwrap();
    assert_eq!(
        (config.harness, config.permission_mode),
        (HarnessId::Opencode, PermissionMode::Full)
    );
    assert!(no_auto.provider.seen().is_empty(), "no provider work");
    no_auto.close().await;
}

/// F10, F14–F16 (RP-13, RP-14, RP-15): accepted work follows the host's policy,
/// not the phone's connection. A command whose connection closed mid-call
/// either never reached admission or completes: its receipt settles by
/// itself, and the same id resent is that one receipt, run once. Messages
/// accepted before a revocation still run, in order, after it. A stop fenced
/// by an earlier turn is refused as stale and the running turn continues; a
/// queue removal at a stale revision is an explicit conflict; a stop of the
/// current turn ends it while another task's run continues.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_work_survives_phone_loss_and_stale_stops_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::open(&root.path().join("data"), true);
    let project = folder(root.path(), "rm04-project");
    host.project("rm04-space", &project, false);
    let phone = host.pair("rm04 phone", OPERATE).await;
    let created = create("rm04-space");
    phone.execute(&created).await.unwrap();
    let task = created.task_id.clone();
    host.core
        .workspace
        .rename_chat(&task, "RM04 phone task")
        .unwrap();
    // Unrelated work: a task the desktop started, still running throughout.
    host.desktop_task("rm04-desktop-task", "rm04-space").await;
    host.core
        .doc_host
        .queue_message("rm04-desktop-task", "rm04: desktop turn", Vec::new())
        .unwrap();
    host.ran("rm04: desktop turn").await;

    // T1 runs on the phone's task.
    phone
        .execute(&phone.send(&task, "rm04: one").await)
        .await
        .unwrap();
    host.ran("rm04: one").await;
    let t1 = until("the first turn", Duration::from_secs(10), || {
        host.core.sessions.active_turn_id(&task)
    })
    .await;
    assert_eq!(
        phone.snapshot(&task).await.snapshot.active_turn_id,
        Some(t1.clone())
    );

    // F10 (RP-15): the connection closes while a send is in flight.
    let in_flight = phone.send(&task, "rm04: in flight").await;
    let Phone { rpc, session } = phone;
    let call = rpc.call(
        methods::EXECUTE_REMOTE_TASK_COMMAND,
        serde_json::to_value(&in_flight).unwrap(),
    );
    let _ = tokio::time::timeout(Duration::from_millis(2), call).await;
    drop(rpc);
    let phone = host.connect(&session).await;
    let reached = tokio::time::timeout(Duration::from_secs(2), async {
        while host.receipt(&in_flight.command_id).is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok();
    // Admitted, it settles by itself: never a half-admitted pending row.
    // Never admitted, the same id (the outbox's resend after its read-back)
    // admits it once.
    let receipt = if reached {
        settled(&host, &in_flight.command_id).await
    } else {
        phone.execute(&in_flight).await.unwrap()
    };
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
    assert_eq!(receipt.actor_id, phone.actor());
    let resent = phone.execute(&in_flight).await.unwrap();
    assert_eq!(
        (resent.status, resent.payload_hash.as_str()),
        (receipt.status, receipt.payload_hash.as_str())
    );

    // RP-15: messages accepted before a revocation run in order after it.
    let two = phone.send(&task, "rm04: two").await;
    phone.execute(&two).await.unwrap();
    host.local
        .call(
            methods::REVOKE_REMOTE_SESSION,
            json!({"id": phone.session.session.id}),
        )
        .await
        .unwrap();
    assert!(phone.execute(&send(&task, "rm04: refused")).await.is_err());
    let phone = host.pair("rm04 phone again", OPERATE).await;
    host.finish("rm04: one");
    host.ran("rm04: in flight").await;
    let t2 = until(
        "the turn of the in-flight message",
        Duration::from_secs(10),
        || {
            host.core
                .sessions
                .active_turn_id(&task)
                .filter(|turn| *turn != t1)
        },
    )
    .await;
    host.finish("rm04: in flight");
    host.ran("rm04: two").await;
    let t3 = until("the turn of two", Duration::from_secs(10), || {
        host.core
            .sessions
            .active_turn_id(&task)
            .filter(|turn| *turn != t1 && *turn != t2)
    })
    .await;

    // F14 (RP-14): a stop fenced by T1, which ended, is stale; T3 runs on.
    let mut stale = phone.fenced(&task, RemoteTaskIntent::Stop {}).await;
    stale.expected_turn_id = Some(t1.clone());
    let receipt = phone.execute(&stale).await.unwrap();
    assert_eq!(receipt.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        receipt.reason.as_ref().map(|reason| reason.code),
        Some(CommandReasonCode::StaleTurn)
    );
    assert_eq!(host.core.sessions.active_turn_id(&task), Some(t3.clone()));

    // F15 (RP-13): a queue action names the row's revision. A stale one is
    // an explicit conflict that changes nothing; the current one removes the
    // row, which never runs.
    let to_remove = phone.send(&task, "rm04: to remove").await;
    phone.execute(&to_remove).await.unwrap();
    let removal = |revision| RemoteTaskIntent::RemoveQueued {
        message_id: to_remove.command_id.clone(),
        expected_queue_revision: revision,
    };
    let mut stale_removal = phone.fenced(&task, removal(2)).await;
    stale_removal.expected_turn_id = None;
    let receipt = phone.execute(&stale_removal).await.unwrap();
    assert_eq!(receipt.status, CommandReceiptStatus::Conflict);
    assert_eq!(
        receipt.reason.as_ref().map(|reason| reason.code),
        Some(CommandReasonCode::QueueRevisionConflict)
    );
    assert!(
        host.core
            .doc_host
            .queued_message(&task, &to_remove.command_id)
            .unwrap()
            .is_some()
    );
    let mut current_removal = phone.fenced(&task, removal(1)).await;
    current_removal.expected_turn_id = None;
    let receipt = phone.execute(&current_removal).await.unwrap();
    assert_eq!(receipt.status, CommandReceiptStatus::Applied);
    assert!(
        host.core
            .doc_host
            .queued_message(&task, &to_remove.command_id)
            .unwrap()
            .is_none()
    );

    // A stop of the current turn ends it; the desktop task keeps running.
    let stop = phone.fenced(&task, RemoteTaskIntent::Stop {}).await;
    assert_eq!(stop.expected_turn_id.as_deref(), Some(t3.as_str()));
    let receipt = phone.execute(&stop).await.unwrap();
    assert!(
        matches!(
            receipt.status,
            CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
        ),
        "{receipt:?}"
    );
    until(
        "the phone task's turn to stop",
        Duration::from_secs(10),
        || (host.core.sessions.active_turn_id(&task) != Some(t3.clone())).then_some(()),
    )
    .await;
    assert!(
        host.core
            .sessions
            .active_turn_id("rm04-desktop-task")
            .is_some(),
        "the unrelated task is still running"
    );
    for prompt in [
        "rm04: one",
        "rm04: in flight",
        "rm04: two",
        "rm04: desktop turn",
    ] {
        assert_eq!(host.runs_of(prompt).len(), 1, "{prompt}");
    }
    assert!(host.runs_of("rm04: refused").is_empty());
    assert!(host.runs_of("rm04: to remove").is_empty());
    host.close().await;
}

const SETUP_CASE: &str = "a_new_worktree_holds_follow_ups_behind_setup_and_after_it_fails";

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "RM04 QA")
        .env("GIT_AUTHOR_EMAIL", "qa@invalid.test")
        .env("GIT_COMMITTER_NAME", "RM04 QA")
        .env("GIT_COMMITTER_EMAIL", "qa@invalid.test")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Waits for `qa-release-setup`, then fails unless `qa-allow-setup` exists.
fn setup_action() -> ProjectAction {
    ProjectAction {
        id: "setup".into(),
        name: "RM04 setup".into(),
        command: r#"printf 'setup-attempt\n' >> qa-setup-attempts; while [ ! -f qa-release-setup ]; do sleep 0.02; done; test -f qa-allow-setup"#
            .into(),
        setup_on_worktree_create: true,
        wait_for_setup: true,
        preview_url: None,
    }
}

/// Re-run this one case in a child whose worktrees go to a disposable root
/// (the engine reads `LUNA_WORKTREES_DIR` when it assembles).
fn isolated_child() -> bool {
    if std::env::var("LUNA_RM04_SETUP_CHILD").ok().as_deref() == Some(SETUP_CASE) {
        return false;
    }
    let temp = tempfile::tempdir().unwrap();
    let worktrees = temp.path().join("managed-worktrees");
    std::fs::create_dir_all(&worktrees).unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", SETUP_CASE, "--nocapture", "--test-threads=1"])
        .env("LUNA_RM04_SETUP_CHILD", SETUP_CASE)
        .env("LUNA_WORKTREES_DIR", worktrees.canonicalize().unwrap())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the isolated setup case failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    true
}

/// The task's checkout once its new worktree exists and its setup waits.
async fn worktree_in_setup(host: &Host, task: &str, repo: &Path) -> PathBuf {
    until(
        &format!("task {task}'s worktree setup to start"),
        Duration::from_secs(20),
        || {
            let cwd = PathBuf::from(host.core.workspace.chat(task).ok()??.cwd?);
            (cwd != repo && cwd.join("qa-setup-attempts").exists()).then_some(cwd)
        },
    )
    .await
}

/// The setup run of `worktree` (setup runs belong to a checkout).
async fn setup_run(host: &Host, task: &str, worktree: &Path) -> ProjectCommandRun {
    host.core
        .project_commands
        .snapshot(task)
        .await
        .unwrap()
        .runs
        .into_iter()
        .find(|run| run.setup && Path::new(&run.cwd) == worktree)
        .expect("the worktree's setup run")
}

async fn setup_succeeded(host: &Host, task: &str, worktree: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while setup_run(host, task, worktree).await.status != ProjectCommandStatus::Succeeded {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the retried setup did not succeed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// F12, F13 (the setup race): a task's first message in a new worktree is
/// admitted and owns the next turn; a follow-up the phone sends while that
/// worktree's setup runs is accepted into the queue and waits behind it,
/// never reaching the provider. When the setup fails, or is cancelled, the
/// first message's receipt fails and the follow-up is held on the host
/// (`ExecutionUnavailable`, a rejected dispatch receipt naming the setup);
/// nothing runs in the unprepared worktree or the project folder. After a
/// successful setup retry, an explicit send-now delivers the held follow-up
/// once, in the worktree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_worktree_holds_follow_ups_behind_setup_and_after_it_fails() {
    if isolated_child() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let repo = base.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "core.hooksPath", "disabled-qa-hooks"]);
    std::fs::write(repo.join("README.md"), "RM04 setup fixture\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(
        &repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    );
    let host = Host::open(&base.join("data"), false);
    host.project("rm04-git-space", &repo, true);
    host.core
        .project_commands
        .configure("rm04-git-space", &[], vec![setup_action()])
        .unwrap();
    let phone = host.pair("rm04 phone", OPERATE).await;

    let created = create("rm04-git-space");
    phone.execute(&created).await.unwrap();
    let task = created.task_id.clone();
    host.core
        .workspace
        .rename_chat(&task, "RM04 worktree task")
        .unwrap();
    let first = send_in_new_worktree(&task, "rm04: first in worktree");
    let admitted = phone.execute(&first).await.unwrap();
    assert_eq!(admitted.intent, "run");
    assert_eq!(admitted.actor_id, phone.actor());
    let worktree = worktree_in_setup(&host, &task, &repo).await;
    // A new worktree only for a task's first message.
    let late_worktree = send_in_new_worktree(&task, "rm04: second worktree");
    let error = phone.execute(&late_worktree).await.unwrap_err();
    assert!(error.contains("only a task's first message"), "{error}");

    // F12: accepted into the queue, waiting behind the running setup.
    let follow_up = phone.send(&task, "rm04: follow-up behind setup").await;
    let queued = phone.execute(&follow_up).await.unwrap();
    assert_eq!(queued.status, CommandReceiptStatus::Applied);
    let row = host
        .core
        .doc_host
        .queued_message(&task, &follow_up.command_id)
        .unwrap()
        .expect("the follow-up waits in the host's queue");
    assert!(matches!(row.status, luna_doc::QueueMessageStatus::Held));
    assert!(host.provider.seen().is_empty(), "nothing runs before setup");

    // F13: the setup fails. The first message fails; the follow-up is held.
    std::fs::write(worktree.join("qa-release-setup"), "release\n").unwrap();
    let failed = settled(&host, &first.command_id).await;
    assert_ne!(failed.status, CommandReceiptStatus::Applied);
    let dispatch = format!("queue-dispatch:{task}:{}:1", follow_up.command_id);
    let held = settled(&host, &dispatch).await;
    assert_eq!(held.status, CommandReceiptStatus::Rejected);
    assert!(
        held.reason
            .as_ref()
            .is_some_and(|reason| reason.message.contains("Worktree setup failed")),
        "{held:?}"
    );
    let row = host
        .core
        .doc_host
        .queued_message(&task, &follow_up.command_id)
        .unwrap()
        .expect("the held follow-up stays queued");
    assert!(matches!(row.status, luna_doc::QueueMessageStatus::Held));
    assert_eq!(
        row.hold_reason,
        Some(luna_doc::QueueHoldReason::ExecutionUnavailable)
    );
    assert!(host.provider.seen().is_empty(), "nothing ran anywhere");
    // The phone reads the same held delivery.
    assert_eq!(
        phone.receipt(&dispatch).await.map(|receipt| receipt.status),
        Some(held.status)
    );

    // Recovery: retry the setup, then send the held follow-up explicitly.
    std::fs::write(worktree.join("qa-allow-setup"), "allow\n").unwrap();
    let setup = setup_run(&host, &task, &worktree).await;
    host.core
        .project_commands
        .retry(&task, &setup.id)
        .await
        .unwrap();
    setup_succeeded(&host, &task, &worktree).await;
    let now = phone
        .fenced(
            &task,
            RemoteTaskIntent::SendQueuedNow {
                message_id: follow_up.command_id.clone(),
                expected_queue_revision: row.revision,
            },
        )
        .await;
    let sent = phone.execute(&now).await.unwrap();
    assert!(
        matches!(
            sent.status,
            CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
        ),
        "{sent:?}"
    );
    let ran = host.ran("rm04: follow-up behind setup").await;
    assert_eq!(Path::new(&ran.cwd), worktree.as_path(), "in the worktree");
    assert_eq!(host.runs_of("rm04: follow-up behind setup").len(), 1);
    assert!(host.runs_of("rm04: first in worktree").is_empty());

    // A cancelled setup holds the same way.
    let created = create("rm04-git-space");
    phone.execute(&created).await.unwrap();
    let cancelled = created.task_id.clone();
    host.core
        .workspace
        .rename_chat(&cancelled, "RM04 cancelled setup")
        .unwrap();
    let first = send_in_new_worktree(&cancelled, "rm04: first before cancel");
    phone.execute(&first).await.unwrap();
    let worktree = worktree_in_setup(&host, &cancelled, &repo).await;
    let follow_up = phone
        .send(&cancelled, "rm04: follow-up before cancel")
        .await;
    phone.execute(&follow_up).await.unwrap();
    let setup = setup_run(&host, &cancelled, &worktree).await;
    host.core
        .project_commands
        .stop(&cancelled, &setup.id)
        .await
        .unwrap();
    assert_ne!(
        settled(&host, &first.command_id).await.status,
        CommandReceiptStatus::Applied
    );
    let held = settled(
        &host,
        &format!("queue-dispatch:{cancelled}:{}:1", follow_up.command_id),
    )
    .await;
    assert_eq!(held.status, CommandReceiptStatus::Rejected);
    assert!(
        held.reason
            .as_ref()
            .is_some_and(|reason| reason.message.contains("Worktree setup")),
        "{held:?}"
    );
    for prompt in ["rm04: first before cancel", "rm04: follow-up before cancel"] {
        assert!(host.runs_of(prompt).is_empty(), "{prompt}");
    }
    host.close().await;
}
