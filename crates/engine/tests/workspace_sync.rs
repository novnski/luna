//! M4a integration: two `EngineCore`s (distinct data dirs + device ids) sharing one
//! per-org workspace doc.
//!
//! The in-memory bridge below stands in for the edge room: it cross-imports Loro
//! updates (`export(updates)`) between the two engines' workspace docs on a timer,
//! which is exactly what `RoomClient` + the SessionRoom DO do over the wire. A live
//! variant against a real edge runs behind `#[ignore]` (LUNA_EDGE_WS, like
//! luna-sync's edge_convergence test).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;

use luna_doc::{SessionCommandPayload, SessionCommandStatus};
use luna_engine::{EngineCore, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{
    AgentEvent, ChatConfig, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest, SandboxLevel,
    SessionStatus, SteeringMode,
};
use luna_rpc::methods;

/// Scripted harness: emits SessionStarted + text + Done with a per-event delay (so
/// `Working` is observable across the bridge).
struct ScriptedHarness {
    id: HarnessId,
    text: &'static str,
    step_delay: Duration,
}

#[async_trait]
impl Harness for ScriptedHarness {
    fn id(&self) -> HarnessId {
        self.id
    }
    fn display_name(&self) -> &str {
        "Scripted"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        _request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentEvent, HarnessError>>(16);
        let harness = self.id;
        let text = self.text;
        let delay = self.step_delay;
        tokio::spawn(async move {
            let script = vec![
                AgentEvent::SessionStarted {
                    harness,
                    model: "scripted-1".into(),
                    tools: vec![],
                    cwd: "/tmp".into(),
                    session_id: "hs-1".into(),
                    assistant_message_id: "a-1".into(),
                },
                AgentEvent::TextDelta { text: text.into() },
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: Some("hs-1".into()),
                },
            ];
            for event in script {
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
                tokio::time::sleep(delay).await;
            }
        });
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed())
    }
}

fn registry() -> Arc<HarnessRegistry> {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(ScriptedHarness {
        id: HarnessId::Mock,
        text: "Hello",
        step_delay: Duration::from_millis(60),
    }));
    registry.register(Arc::new(ScriptedHarness {
        id: HarnessId::Opencode,
        text: "From opencode",
        step_delay: Duration::from_millis(10),
    }));
    Arc::new(registry)
}

/// Assemble an engine with a fixed device id under its own data dir (offline).
fn assemble(dir: &std::path::Path, device_id: &str) -> EngineCore {
    std::fs::create_dir_all(dir).expect("create data dir");
    std::fs::write(dir.join("device-id"), device_id).expect("write device id");
    EngineCore::assemble(dir, registry(), HarnessId::Mock, None).expect("engine core assembles")
}

/// The in-process room: an in-memory registry server speaking the DO's JSON
/// WS protocol (what the RegistryRoom DO does over the wire), with both
/// engines' hosts wired to it via the test seam.
async fn bridge(
    a: &EngineCore,
    b: &EngineCore,
) -> luna_sync::registry::mock_server::MockRegistryServer {
    let server = luna_sync::registry::mock_server::MockRegistryServer::start().await;
    a.workspace.connect_registry_url(&server.url());
    b.workspace.connect_registry_url(&server.url());
    server
}

async fn wait_for<F>(mut predicate: F, what: &str)
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn run_request(prompt: &str) -> RunRequest {
    RunRequest {
        review: None,
        prompt: prompt.into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: "/tmp".into(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: true,
        permission_mode: luna_proto::PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: Vec::new(),
        worktree: None,
        resume: None,
    }
}

/// Queue a locally authorized run command with a deterministic command id.
fn queue_run(core: &EngineCore, chat_id: &str, command_id: &str, message_id: &str) {
    queue_run_with(
        core,
        chat_id,
        command_id,
        message_id,
        run_request("go do it"),
    );
}

fn queue_run_with(
    core: &EngineCore,
    chat_id: &str,
    command_id: &str,
    message_id: &str,
    request: RunRequest,
) {
    core.doc_host
        .queue_command_with_id_and_transfers(
            chat_id,
            command_id,
            SessionCommandPayload::Run {
                request,
                message_id: message_id.into(),
            },
            Vec::new(),
        )
        .expect("queue command");
}

#[tokio::test]
async fn two_engines_share_a_workspace() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = assemble(dir_a.path(), "dev-a");
    let b = assemble(dir_b.path(), "dev-b");
    let link = bridge(&a, &b).await;

    // Device rows from BOTH engines appear on both sides.
    for core in [&a, &b] {
        wait_for(
            || {
                let ids: Vec<String> = core
                    .workspace
                    .read_devices()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|d| d.id)
                    .collect();
                ids == ["dev-a", "dev-b"]
            },
            "both device rows",
        )
        .await;
    }

    // CreateSpace + CreateChat on A (Mutate over the real RPC surface), hosted
    // by dev-a via the space.
    let client_a = luna_rpc::memory_client(a.rpc_service());
    let client_b = luna_rpc::memory_client(b.rpc_service());
    client_a
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createSpace", "spaceId": "space-1", "deviceId": "dev-a", "path": "/tmp"
            }),
        )
        .await
        .expect("create space");
    client_a
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createChat", "chatId": "chat-1", "spaceId": "space-1"
            }),
        )
        .await
        .expect("create chat");
    // The space row crosses to B alongside the chat row.
    wait_for(
        || {
            b.workspace
                .read_spaces()
                .unwrap_or_default()
                .iter()
                .any(|s| s.id == "space-1" && s.device_id == "dev-a" && s.path == "/tmp")
        },
        "space row on B",
    )
    .await;
    wait_for(
        || b.workspace.chat("chat-1").ok().flatten().is_some(),
        "chat row on B",
    )
    .await;

    // Run on A: B's workspace view shows the session Working, then Idle.
    queue_run(&a, "chat-1", "cmd-run-1", "m-1");
    let b_status = |wanted: SessionStatus| {
        let ws = b.workspace.clone();
        move || {
            ws.read_sessions()
                .unwrap_or_default()
                .iter()
                .any(|s| s.chat_id == "chat-1" && s.device_id == "dev-a" && s.status == wanted)
        }
    };
    wait_for(b_status(SessionStatus::Working), "Working on B").await;
    wait_for(b_status(SessionStatus::Idle), "Idle on B").await;

    // Sidebar freshness crossed too: the chat row's preview settles on the
    // assistant's final text (first-120-chars policy).
    wait_for(
        || {
            b.workspace
                .chat("chat-1")
                .ok()
                .flatten()
                .and_then(|c| c.last_message_preview)
                .as_deref()
                == Some("Hello")
        },
        "assistant preview on B",
    )
    .await;

    // Rename + archive from B (LWW from any device) become visible on A.
    client_b
        .call(
            methods::MUTATE,
            serde_json::json!({ "op": "renameChat", "chatId": "chat-1", "title": "Renamed from B" }),
        )
        .await
        .expect("rename chat");
    client_b
        .call(
            methods::MUTATE,
            serde_json::json!({ "op": "setChatArchived", "chatId": "chat-1", "archived": true }),
        )
        .await
        .expect("archive chat");
    wait_for(
        || {
            a.workspace
                .chat("chat-1")
                .ok()
                .flatten()
                .is_some_and(|c| c.title.as_deref() == Some("Renamed from B") && c.archived)
        },
        "rename + archive on A",
    )
    .await;

    // Device rename from B visible on A.
    client_b
        .call(
            methods::MUTATE,
            serde_json::json!({ "op": "renameDevice", "deviceId": "dev-b", "name": "B's VPS" }),
        )
        .await
        .expect("rename device");
    wait_for(
        || {
            a.workspace
                .read_devices()
                .unwrap_or_default()
                .iter()
                .any(|d| d.id == "dev-b" && d.name == "B's VPS")
        },
        "device rename on A",
    )
    .await;

    drop(link);
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn claim_on_first_command_creates_the_chat_row() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = assemble(dir_a.path(), "dev-a");
    let b = assemble(dir_b.path(), "dev-b");
    let link = bridge(&a, &b).await;

    // No CreateChat: the first run command claims the chat under A's device id.
    queue_run(&a, "chat-claimed", "cmd-claim-1", "m-1");
    wait_for(
        || {
            b.workspace
                .chat("chat-claimed")
                .ok()
                .flatten()
                .is_some_and(|c| c.device_id == "dev-a" && c.cwd.as_deref() == Some("/tmp"))
        },
        "claimed chat row on B",
    )
    .await;

    drop(link);
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn offline_project_claim_syncs_and_survives_viewer_restart() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = assemble(dir_a.path(), "dev-a");
    let b = assemble(dir_b.path(), "dev-b");
    let request = run_request("project task while offline");
    a.doc_host
        .queue_command(
            "project-offline",
            SessionCommandPayload::Run {
                request,
                message_id: "project-msg".into(),
            },
        )
        .unwrap();
    wait_for(
        || a.workspace.chat("project-offline").unwrap().is_some(),
        "offline claim",
    )
    .await;
    let link = bridge(&a, &b).await;
    wait_for(
        || b.workspace.chat("project-offline").unwrap().is_some(),
        "project sync",
    )
    .await;
    let row = b.workspace.chat("project-offline").unwrap().unwrap();
    assert_eq!(row.device_id, "dev-a");
    assert!(row.space_id.is_some());
    assert_eq!(row.cwd.as_deref(), Some("/tmp"));
    assert_eq!(a.workspace.read_spaces().unwrap().len(), 1);
    assert_eq!(b.workspace.read_spaces().unwrap().len(), 1);
    b.shutdown().await;
    drop(b);
    let b = assemble(dir_b.path(), "dev-b");
    let row = b.workspace.chat("project-offline").unwrap().unwrap();
    assert!(row.space_id.is_some());
    assert_eq!(row.device_id, "dev-a");
    assert_eq!(b.workspace.read_chats().unwrap().len(), 1);
    assert_eq!(b.workspace.read_spaces().unwrap().len(), 1);
    drop(link);
    a.shutdown().await;
    b.shutdown().await;
}

/// A first command whose cwd is a linked WORKTREE must attribute the chat to
/// the parent checkout's space — claiming at the worktree path minted a
/// phantom sidebar space named after the worktree folder.
#[tokio::test]
async fn claim_resolves_a_worktree_cwd_to_the_repo_root_space() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), "dev-a");
    let client = luna_rpc::memory_client(core.rpc_service());

    // A checkout with a linked worktree — fs layout only; the claim path
    // reads `.git` without spawning git.
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path().join("proj");
    let wt = repo.path().join("clever-ember");
    std::fs::create_dir_all(root.join(".git/worktrees/clever-ember")).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(
        wt.join(".git"),
        format!(
            "gitdir: {}\n",
            root.join(".git/worktrees/clever-ember").display()
        ),
    )
    .unwrap();

    // The project's space already exists (the normal state — sessions are
    // created FROM a space).
    client
        .call(
            methods::MUTATE,
            serde_json::json!({
                "op": "createSpace", "spaceId": "space-proj", "deviceId": "dev-a",
                "path": root.to_string_lossy(),
            }),
        )
        .await
        .expect("create space");

    let request = RunRequest {
        cwd: wt.to_string_lossy().into_owned(),
        ..run_request("go do it")
    };
    queue_run_with(&core, "chat-wt", "cmd-wt-1", "m-1", request);
    wait_for(
        || {
            core.workspace
                .chat("chat-wt")
                .ok()
                .flatten()
                .is_some_and(|c| c.space_id.as_deref() == Some("space-proj"))
        },
        "worktree chat attributed to the project space",
    )
    .await;
    let spaces = core.workspace.read_spaces().unwrap_or_default();
    assert_eq!(
        spaces.len(),
        1,
        "no phantom space for the worktree: {spaces:?}"
    );
    core.shutdown().await;
}

/// The claimed row records the harness the run actually dispatched on (the
/// request carries the picked harness) — without it the sidebar renders no
/// harness glyph and later dispatches silently fall back to the default.
#[tokio::test]
async fn claimed_chat_row_records_the_run_harness() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), "dev-a");

    let request = RunRequest {
        harness: Some(HarnessId::Opencode),
        ..run_request("go do it")
    };
    queue_run_with(&core, "chat-glyph", "cmd-glyph-1", "m-1", request);
    wait_for(
        || {
            core.workspace
                .chat("chat-glyph")
                .ok()
                .flatten()
                .and_then(|c| c.config)
                .is_some_and(|c| c.harness == HarnessId::Opencode)
        },
        "claimed row carries the dispatched harness",
    )
    .await;
    core.shutdown().await;
}

#[tokio::test]
async fn non_host_engine_leaves_remote_chats_commands_alone() {
    let dir_a = tempfile::tempdir().unwrap();
    let a = assemble(dir_a.path(), "dev-a");

    // The workspace says dev-b hosts this chat (via its dev-b space); a run
    // command in A's local copy of the session doc must NOT execute on A
    // (is_host gating).
    a.workspace
        .create_space("space-remote", "dev-b", "/tmp/remote", None, false)
        .expect("create remote space row");
    a.workspace
        .create_chat("chat-remote", Some("space-remote"), None, None, None)
        .expect("create remote-hosted chat row");
    queue_run(&a, "chat-remote", "cmd-remote-1", "m-1");

    tokio::time::sleep(Duration::from_millis(400)).await;
    let handle = a.doc_host.open("chat-remote").expect("open chat");
    let commands = handle.doc().read_commands().expect("read commands");
    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0].status,
        SessionCommandStatus::Pending,
        "command must stay pending"
    );
    let entries = handle.doc().read_entries().expect("read entries");
    assert!(
        entries.is_empty(),
        "non-host must not write entries: {entries:#?}"
    );
    assert!(a.sessions.session_status("chat-remote").is_none());

    a.shutdown().await;
}

#[tokio::test]
async fn chat_config_selects_the_run_harness() {
    let dir_a = tempfile::tempdir().unwrap();
    let a = assemble(dir_a.path(), "dev-a"); // default harness = Mock ("Hello")

    a.workspace
        .create_space("space-cfg", "dev-a", "/tmp/cfg", None, false)
        .expect("create space");
    a.workspace
        .create_chat(
            "chat-cfg",
            Some("space-cfg"),
            None,
            Some(ChatConfig {
                harness: HarnessId::Opencode,
                provider_account_id: None,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
                permission_mode: luna_proto::PermissionMode::Ask,
                effective_permission_mode: luna_proto::PermissionMode::Ask,
                permission_ceiling_source: None,
                permission_degradation_reason: None,
                permission_audit: Vec::new(),
            }),
            None,
        )
        .expect("create configured chat");
    queue_run(&a, "chat-cfg", "cmd-cfg-1", "m-1");

    // The configured harness (OpenCode) ran instead of the default Mock.
    let handle = a.doc_host.open("chat-cfg").expect("open chat");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let entries = handle.doc().read_entries().unwrap_or_default();
        if entries.iter().any(|e| {
            e.parts.iter().any(
                |p| matches!(p, luna_doc::MessagePart::Text { text, .. } if text == "From opencode"),
            )
        }) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "configured harness did not run; commands={:?}, entries={entries:?}",
            handle.doc().read_commands().unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    a.shutdown().await;
}

/// Live-edge variant: the same convergence through a real workspace room. Requires
/// the TS edge (`wrangler dev` in `edge/` with AUTH_MODE=dev):
///
/// ```sh
/// LUNA_EDGE_WS=ws://127.0.0.1:8787 cargo test -p luna-engine -- --ignored
/// ```
#[tokio::test]
#[ignore = "requires a live edge: set LUNA_EDGE_WS (e.g. ws://127.0.0.1:8787)"]
async fn two_engines_converge_through_a_real_workspace_room() {
    use luna_engine::doc_host::EdgeConfig;

    let base = std::env::var("LUNA_EDGE_WS")
        .expect("set LUNA_EDGE_WS to the edge origin, e.g. ws://127.0.0.1:8787");
    let org = format!("org-{}", uuid::Uuid::new_v4().simple());

    let assemble_live = |dir: &std::path::Path, device_id: &str, user: &str| {
        std::fs::create_dir_all(dir).expect("create data dir");
        std::fs::write(dir.join("device-id"), device_id).expect("write device id");
        // Dev-mode bearer `user@org` carries the org claim the workspace route checks.
        let edge = Some(EdgeConfig::with_static_token(
            base.clone(),
            format!("{user}@{org}"),
        ));
        EngineCore::assemble_with_identity(dir, registry(), HarnessId::Mock, edge, &org, user)
            .expect("engine core assembles")
    };

    // Workspace docs are per-user (`ws3/{org}/{user}`): convergence is across
    // ONE user's devices — two engines, same user, different device ids.
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = assemble_live(dir_a.path(), "dev-live-a", "alice");
    let b = assemble_live(dir_b.path(), "dev-live-b", "alice");

    // Both device rows converge through the real room.
    for core in [&a, &b] {
        wait_for(
            || {
                let ids: Vec<String> = core
                    .workspace
                    .read_devices()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|d| d.id)
                    .collect();
                ids == ["dev-live-a", "dev-live-b"]
            },
            "both device rows through the edge",
        )
        .await;
    }

    // A rename from B lands on A.
    b.workspace
        .rename_device("dev-live-a", "renamed by b")
        .expect("rename");
    wait_for(
        || {
            a.workspace
                .read_devices()
                .unwrap_or_default()
                .iter()
                .any(|d| d.id == "dev-live-a" && d.name == "renamed by b")
        },
        "device rename through the edge",
    )
    .await;

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn legacy_workspace_doc_migrates_instantly_on_first_boot() {
    use luna_proto::{Chat, Device, Session, Space};

    let dir_a = tempfile::tempdir().unwrap();
    // Seed the identity-scoped store with a LEGACY Loro workspace snapshot —
    // what an updated engine finds on its first boot after the registry change.
    let org_dir = dir_a.path().join("orgs").join("dev-org").join("dev-user");
    {
        let store = luna_sync::DocsStore::open(&org_dir).expect("open store");
        let legacy = luna_doc::WorkspaceDoc::new();
        let now = chrono::Utc::now();
        legacy
            .upsert_device(&Device {
                id: "dev-a".into(),
                name: "old laptop".into(),
                platform: "linux".into(),
                last_seen_at: Some(now),
                created_at: Some(now),
                version: Some("0.1.17".into()),
                capabilities: Vec::new(),
            })
            .unwrap();
        legacy
            .upsert_space(&Space {
                execution_defaults: Default::default(),
                id: "space-legacy".into(),
                device_id: "dev-a".into(),
                path: "/tmp/legacy".into(),
                name: Some("Legacy Space".into()),
                git_detected: true,
                git_checked_at: Some(now),
                checkout_id: Some("co-1".into()),
                created_at: now,
            })
            .unwrap();
        legacy
            .upsert_chat(&Chat {
                id: "chat-legacy".into(),
                device_id: "dev-a".into(),
                title: Some("Migrated chat".into()),
                archived: false,
                settlement_override: None,
                pinned_order: None,
                cwd: Some("/tmp/legacy".into()),
                branch: Some("main".into()),
                checkout_id: None,
                source_context: None,
                config: None,
                last_message_preview: Some("old preview".into()),
                last_message_at: Some(now),
                created_at: now,
                harness_session_id: Some("hs-9".into()),
                room_gen: None,
                harness_session_cwd: Some("/tmp/legacy".into()),
                space_id: Some("space-legacy".into()),
                last_seen_at: Some(now),
                manually_unread: false,
            })
            .unwrap();
        legacy
            .upsert_session(&Session {
                last_completed_turn: None,
                chat_id: "chat-legacy".into(),
                device_id: "dev-a".into(),
                status: SessionStatus::Idle,
                started_at: Some(now),
                updated_at: now,
                recovery: None,
            })
            .unwrap();
        store
            .save_snapshot("workspace2", &legacy.export_snapshot().unwrap())
            .expect("save legacy snapshot");
    }

    // Boot: migration is instant — the full sidebar state is readable before
    // any server contact.
    let a = assemble(dir_a.path(), "dev-a");
    let chats = a.workspace.read_chats().expect("chats");
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].title.as_deref(), Some("Migrated chat"));
    assert_eq!(chats[0].harness_session_id.as_deref(), Some("hs-9"));
    assert_eq!(chats[0].space_id.as_deref(), Some("space-legacy"));
    let spaces = a.workspace.read_spaces().expect("spaces");
    assert_eq!(spaces.len(), 1);
    assert!(spaces[0].git_detected);
    // The boot-time device upsert kept the LEGACY user-set name (LWW row
    // exists), not the hostname.
    let devices = a.workspace.read_devices().expect("devices");
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "old laptop");

    // A second (fresh) device converges through the room from the migrated seed.
    let dir_b = tempfile::tempdir().unwrap();
    let b = assemble(dir_b.path(), "dev-b");
    let link = bridge(&a, &b).await;
    wait_for(
        || {
            b.workspace
                .chat("chat-legacy")
                .ok()
                .flatten()
                .is_some_and(|c| c.title.as_deref() == Some("Migrated chat"))
        },
        "migrated chat on B",
    )
    .await;

    // A live rename beats the migrated (historical-HLC) title everywhere.
    b.workspace
        .rename_chat("chat-legacy", "renamed live")
        .expect("rename");
    wait_for(
        || {
            a.workspace
                .chat("chat-legacy")
                .ok()
                .flatten()
                .is_some_and(|c| c.title.as_deref() == Some("renamed live"))
        },
        "live rename beats migration on A",
    )
    .await;

    drop(link);
    a.shutdown().await;
    b.shutdown().await;

    // The registry snapshot now exists; the legacy snapshot is kept for rollback.
    let store = luna_sync::DocsStore::open(&org_dir).expect("reopen store");
    assert!(
        store
            .load_snapshot(luna_doc::REGISTRY_DOC_ID)
            .expect("load registry snapshot")
            .is_some(),
        "registry snapshot persisted"
    );
    assert!(
        store
            .load_snapshot("workspace2")
            .expect("load legacy snapshot")
            .is_some(),
        "legacy snapshot retained for rollback"
    );
}

/// A peer receives only the generic registry visibility marker, not the local
/// side-chat database. The marker must still hide the child from its main list.
#[tokio::test]
async fn side_chat_visibility_survives_registry_sync_without_loading_history() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = assemble(dir_a.path(), "side-owner");
    let b = assemble(dir_b.path(), "side-viewer");
    let link = bridge(&a, &b).await;
    let client_a = luna_rpc::memory_client(a.rpc_service());
    let client_b = luna_rpc::memory_client(b.rpc_service());
    let parent = uuid::Uuid::new_v4().to_string();
    a.workspace
        .create_space("space-side-sync", &a.device_id, "/tmp", None, false)
        .unwrap();
    client_a.call(methods::MUTATE, serde_json::json!({"op":"createChat","chatId":parent,"spaceId":"space-side-sync",
        "config":{"harness":"mock","model":"fixture","reasoning":null,"sandbox":"workspace-write","permissionMode":"ask","effectivePermissionMode":"ask"}})).await.unwrap();
    let created = client_a
        .call(
            "CreateSideChat",
            serde_json::json!({"parentId":parent,"requestId":uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap();
    let child = created["meta"]["id"].as_str().unwrap();
    wait_for(
        || b.workspace.chat(child).unwrap().is_some() && b.workspace.is_side_chat(child),
        "side child and visibility marker",
    )
    .await;
    assert!(
        !b.side_chats.contains(child),
        "peer does not receive profile-local history storage"
    );
    assert!(b.doc_host.open(child).is_err());
    assert!(
        client_b
            .call(
                methods::QUEUE_MESSAGE,
                serde_json::json!({"chatId":child,"text":"must not run"})
            )
            .await
            .is_err()
    );
    let mut chats = client_b
        .subscribe(methods::WATCH_CHATS, serde_json::json!({}))
        .await
        .unwrap();
    let visible = tokio::time::timeout(Duration::from_secs(3), chats.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        visible
            .as_array()
            .unwrap()
            .iter()
            .any(|chat| chat["id"] == parent)
    );
    assert!(
        !visible
            .as_array()
            .unwrap()
            .iter()
            .any(|chat| chat["id"] == child)
    );
    assert!(!b.doc_host.sync_statuses().iter().any(|(id, _)| id == child));
    client_a
        .call(
            "UnloadSideChat",
            serde_json::json!({"id":child,"lease":created["lease"]}),
        )
        .await
        .unwrap();
    a.shutdown().await;
    b.shutdown().await;
    drop(link);
    drop(chats);
    drop(client_b);
    drop(b);
    // A persisted peer marker still denies history and runtime after restart,
    // even though that profile has never received the local metadata tables.
    let reopened = assemble(dir_b.path(), "side-viewer");
    assert!(reopened.workspace.is_side_chat(child));
    assert!(!reopened.side_chats.contains(child));
    assert!(reopened.doc_host.open(child).is_err());
    assert!(
        reopened
            .doc_host
            .open(&format!("{child}--sub--peer"))
            .is_err()
    );
    assert!(
        !reopened
            .doc_host
            .sync_statuses()
            .iter()
            .any(|(id, _)| id == child)
    );
    assert!(!reopened.sessions.turn_in_flight(child));
    reopened.shutdown().await;
}
