//! Product-boundary regressions, authored before the feature implementation.
//! Uses the real engine RPC, durable store and session pipeline with a recording
//! provider. Real-provider/native UI evidence is recorded separately.
use async_trait::async_trait;
use futures::{StreamExt, stream, stream::BoxStream};
use luna_engine::{EngineCore, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{AgentEvent, DoneStatus, HarnessId, Model, ReasoningLevel, RunRequest};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Recorder(Mutex<Vec<RunRequest>>);
#[async_trait]
impl Harness for Recorder {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Sidebar fixture"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> luna_proto::SteeringMode {
        luna_proto::SteeringMode::TurnBoundary
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
        _: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        let hold = request.prompt.contains("HOLD_SIDE_RUNTIME");
        self.0.lock().unwrap().push(request);
        if hold {
            return Ok(stream::iter(vec![Ok(AgentEvent::TextDelta {
                text: "SIDE_PARTIAL_BEFORE_STOP".into(),
            })])
            .chain(stream::pending())
            .boxed());
        }
        Ok(stream::iter(vec![
            Ok(AgentEvent::TextDelta {
                text: "SIDE_CHAT_ACK".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            }),
        ])
        .boxed())
    }
}

async fn setup(root: &std::path::Path, recorder: Arc<Recorder>) -> EngineCore {
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(recorder);
    let core = EngineCore::assemble(root, registry, HarnessId::Mock, None).unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    if core.workspace.chat("parent").unwrap().is_none() {
        client
            .call(
                "Mutate",
                json!({"op":"createChat","chatId":"parent","deviceId":core.device_id}),
            )
            .await
            .unwrap();
        core.workspace
            .rename_chat("parent", "Parent fixture")
            .unwrap();
        core.workspace
            .set_chat_config(
                "parent",
                &serde_json::from_value(json!({
                    "harness":"mock", "model":"fixture", "reasoning":null,
                    "sandbox":"read-only", "permissionMode":"ask", "effectivePermissionMode":"ask"
                }))
                .unwrap(),
            )
            .unwrap();
        core.doc_host
            .open("parent")
            .unwrap()
            .write_user_message(
                "parent-context",
                "PARENT_CONTEXT_SENTINEL",
                chrono::Utc::now().timestamp_millis(),
            )
            .unwrap();
    }
    core
}

#[tokio::test(flavor = "multi_thread")]
async fn side_chat_lifecycle_is_durable_isolated_and_history_does_not_run() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let create = json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string()});
    let created: Value = client.call("CreateSideChat", create.clone()).await.unwrap();
    let repeated: Value = client.call("CreateSideChat", create).await.unwrap();
    assert_eq!(created["meta"]["id"], repeated["meta"]["id"]);
    let id = created["meta"]["id"].as_str().unwrap();
    let lease = created["lease"].as_str().unwrap();
    assert_eq!(
        created["meta"]["expiresAtMs"].as_i64().unwrap()
            - created["meta"]["createdAtMs"].as_i64().unwrap(),
        7_200_000
    );
    let send = json!({"id":id,"lease":lease,"messageId":uuid::Uuid::new_v4().to_string(),"text":"SIDE_QUESTION"});
    client.call("SendSideChat", send.clone()).await.unwrap();
    client.call("SendSideChat", send).await.unwrap();
    let page = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let page = client
                .call("ReadSideChat", json!({"id":id,"lease":lease}))
                .await
                .unwrap();
            if page.to_string().contains("SIDE_CHAT_ACK") {
                break page;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(page["entries"].as_array().unwrap().len() <= 50);
    assert_eq!(
        recorder.0.lock().unwrap().len(),
        1,
        "duplicate sends never replay"
    );
    let request = recorder.0.lock().unwrap()[0].clone();
    assert!(request.prompt.contains("PARENT_CONTEXT_SENTINEL"));
    assert!(request.resume.is_none());
    assert_eq!(request.sandbox, luna_proto::SandboxLevel::ReadOnly);
    assert_eq!(
        request.parent_permission_ceiling,
        Some(luna_proto::PermissionMode::Ask)
    );
    assert!(
        !core
            .doc_host
            .open("parent")
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
            .iter()
            .any(|entry| serde_json::to_string(entry)
                .unwrap()
                .contains("SIDE_QUESTION"))
    );
    client
        .call("UnloadSideChat", json!({"id":id,"lease":lease}))
        .await
        .unwrap();
    assert!(!core.sessions.has_live_run(id));
    assert!(
        client
            .call(
                "SendSideChat",
                json!({"id":id,"lease":lease,"messageId":"late","text":"late"})
            )
            .await
            .is_err()
    );
    let history = client.call("ReadSideChat", json!({"id":id})).await.unwrap();
    assert_ne!(history["meta"]["state"], "active");
    assert!(history.to_string().contains("SIDE_CHAT_ACK"));
    let list = client
        .call("ListSideChats", json!({"parentId":"parent"}))
        .await
        .unwrap();
    assert!(
        !list.to_string().contains("SIDE_CHAT_ACK"),
        "metadata-only history listing"
    );
    assert_eq!(recorder.0.lock().unwrap().len(), 1);
    let saved_id = id.to_string();
    drop(client);
    core.shutdown().await;
    drop(core);
    let reopened = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let history = client
        .call("ReadSideChat", json!({"id":saved_id}))
        .await
        .unwrap();
    assert!(history.to_string().contains("SIDE_CHAT_ACK"));
    assert_eq!(
        recorder.0.lock().unwrap().len(),
        1,
        "history must not start a provider"
    );
    assert!(!reopened.sessions.any_active());
}

#[tokio::test(flavor = "multi_thread")]
async fn deadline_and_restart_seal_writable_chats_without_replay() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let created = client
        .call(
            "CreateSideChat",
            json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap();
    let id = created["meta"]["id"].as_str().unwrap().to_string();
    let lease = created["lease"].as_str().unwrap();
    // Expiry fixture changes persisted wall time, not an alternate product path.
    let db = rusqlite::Connection::open(core.side_chats.database_path()).unwrap();
    db.execute("UPDATE side_chats SET expires_at_ms=0 WHERE id=?1", [&id])
        .unwrap();
    assert!(
        client
            .call(
                "SendSideChat",
                json!({"id":id,"lease":lease,"messageId":"expired","text":"must not run"})
            )
            .await
            .is_err()
    );
    client
        .call("UnloadSideChat", json!({"id":id,"lease":lease}))
        .await
        .unwrap();
    let history = client.call("ReadSideChat", json!({"id":id})).await.unwrap();
    assert_eq!(history["meta"]["state"], "expired");
    assert!(recorder.0.lock().unwrap().is_empty());
    let second = client
        .call(
            "CreateSideChat",
            json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap();
    drop(db);
    drop(client);
    core.shutdown().await;
    let db = rusqlite::Connection::open(core.side_chats.database_path()).unwrap();
    db.execute(
        "UPDATE side_chats SET state='active' WHERE id=?1",
        [second["meta"]["id"].as_str().unwrap()],
    )
    .unwrap();
    drop(db);
    drop(core);
    let reopened = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(reopened.rpc_service());
    let page = client
        .call("ReadSideChat", json!({"id":second["meta"]["id"]}))
        .await
        .unwrap();
    assert_eq!(page["meta"]["state"], "recovered");
    assert!(recorder.0.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn anchored_side_chat_preserves_origin_without_mutating_parent() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let before = core
        .doc_host
        .open("parent")
        .unwrap()
        .doc()
        .read_entries()
        .unwrap();
    assert!(client.call("CreateSideChat", json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string(),"originMessageId":"missing","anchorText":"quote"})).await.is_err());
    let created = client.call("CreateSideChat", json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string(),"originMessageId":"parent-context","anchorText":"CONTEXT_SENTINEL"})).await.unwrap();
    assert_eq!(created["meta"]["originMessageId"], "parent-context");
    assert_eq!(created["meta"]["anchorText"], "CONTEXT_SENTINEL");
    client
        .call(
            "UnloadSideChat",
            json!({"id":created["meta"]["id"],"lease":created["lease"]}),
        )
        .await
        .unwrap();
    let history = client
        .call("ReadSideChat", json!({"id":created["meta"]["id"]}))
        .await
        .unwrap();
    assert_eq!(history["meta"]["anchorText"], "CONTEXT_SENTINEL");
    assert_eq!(
        before,
        core.doc_host
            .open("parent")
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
    );
    assert!(recorder.0.lock().unwrap().is_empty());
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn creation_replay_rejects_changed_anchor_text() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let request = uuid::Uuid::new_v4().to_string();
    let original = json!({"parentId":"parent","requestId":request,"originMessageId":"parent-context","anchorText":"CONTEXT_SENTINEL"});
    let created = client
        .call("CreateSideChat", original.clone())
        .await
        .unwrap();
    let mut changed = original.clone();
    changed["anchorText"] = json!("DIFFERENT_QUOTE");
    assert!(client.call("CreateSideChat", changed).await.is_err());
    assert_eq!(
        client.call("CreateSideChat", original).await.unwrap()["meta"]["id"],
        created["meta"]["id"]
    );
    assert!(recorder.0.lock().unwrap().is_empty());
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn paged_history_stays_cold_on_restart_and_releases_loaded_documents() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let mut ids = Vec::new();
    for index in 0..53 {
        let created = client
            .call(
                "CreateSideChat",
                json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string()}),
            )
            .await
            .unwrap();
        let id = created["meta"]["id"].as_str().unwrap().to_string();
        if index == 0 {
            let doc = core.doc_host.open(&id).unwrap();
            for message in 0..120 {
                doc.write_user_message(
                    &format!("fixture-{message}"),
                    &format!("SAVED_PAGE_MESSAGE_{message:03}"),
                    message,
                )
                .unwrap();
            }
        }
        client
            .call("UnloadSideChat", json!({"id":id,"lease":created["lease"]}))
            .await
            .unwrap();
        assert!(
            !core
                .doc_host
                .sync_statuses()
                .iter()
                .any(|(open, _)| open == &id)
        );
        assert!(core.sessions.session_status(&id).is_none());
        ids.push(id);
    }
    let list = client
        .call("ListSideChats", json!({"parentId":"parent"}))
        .await
        .unwrap();
    assert_eq!(list["items"].as_array().unwrap().len(), 50);
    assert!(!list.to_string().contains("SAVED_PAGE_MESSAGE"));
    let earlier = client
        .call(
            "ListSideChats",
            json!({"parentId":"parent","before":list["before"]}),
        )
        .await
        .unwrap();
    assert_eq!(earlier["items"].as_array().unwrap().len(), 3);
    let page = client
        .call("ReadSideChat", json!({"id":ids[0]}))
        .await
        .unwrap();
    assert_eq!(page["entries"].as_array().unwrap().len(), 50);
    assert!(page.to_string().contains("SAVED_PAGE_MESSAGE_119"));
    let older = client
        .call("ReadSideChat", json!({"id":ids[0],"before":page["before"]}))
        .await
        .unwrap();
    assert_eq!(older["entries"].as_array().unwrap().len(), 50);
    let oldest = client
        .call(
            "ReadSideChat",
            json!({"id":ids[0],"before":older["before"]}),
        )
        .await
        .unwrap();
    assert_eq!(oldest["entries"].as_array().unwrap().len(), 20);
    assert!(
        !core
            .doc_host
            .sync_statuses()
            .iter()
            .any(|(id, _)| ids.contains(id))
    );
    core.shutdown().await;
    drop(client);
    drop(core);
    let reopened = setup(dir.path(), recorder.clone()).await;
    // Covers the existing five-second startup transcript salvage task too.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        !reopened
            .doc_host
            .sync_statuses()
            .iter()
            .any(|(id, _)| ids.contains(id)),
        "startup must not hydrate hidden side documents"
    );
    assert!(recorder.0.lock().unwrap().is_empty());
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn parent_deletion_seals_and_retires_owned_side_history() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let created = client
        .call(
            "CreateSideChat",
            json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap();
    let id = created["meta"]["id"].as_str().unwrap();
    client
        .call("Mutate", json!({"op":"deleteChat","chatId":"parent"}))
        .await
        .unwrap();
    assert!(core.workspace.chat("parent").unwrap().is_none());
    assert!(core.workspace.chat(id).unwrap().is_none());
    assert!(client.call("ReadSideChat", json!({"id":id})).await.is_err());
    assert!(client.call("SendSideChat", json!({"id":id,"lease":created["lease"],"messageId":"after-delete","text":"must not run"})).await.is_err());
    assert!(recorder.0.lock().unwrap().is_empty());
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn side_chat_permission_ceiling_preserves_legacy_and_full_boundaries() {
    use luna_proto::{PermissionMode, SandboxLevel};
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    for (index, (sandbox, mode, expected_mode)) in [
        (
            SandboxLevel::ReadOnly,
            PermissionMode::Ask,
            PermissionMode::Ask,
        ),
        (
            SandboxLevel::WorkspaceWrite,
            PermissionMode::Full,
            PermissionMode::Auto,
        ),
        (
            SandboxLevel::DangerFullAccess,
            PermissionMode::Full,
            PermissionMode::Full,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut config = core
            .workspace
            .chat("parent")
            .unwrap()
            .unwrap()
            .config
            .unwrap();
        config.sandbox = sandbox;
        config.permission_mode = mode;
        config.effective_permission_mode = mode;
        core.workspace.set_chat_config("parent", &config).unwrap();
        let created = client
            .call(
                "CreateSideChat",
                json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string()}),
            )
            .await
            .unwrap();
        client.call("SendSideChat", json!({"id":created["meta"]["id"],"lease":created["lease"],"messageId":uuid::Uuid::new_v4().to_string(),"text":"PERMISSION_BOUNDARY_ONLY"})).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while recorder.0.lock().unwrap().len() <= index {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let request = recorder.0.lock().unwrap()[index].clone();
        assert_eq!(request.sandbox, sandbox);
        assert_eq!(
            request.permission_policy.as_ref().unwrap().effective_mode,
            expected_mode
        );
        assert_eq!(request.auto_approve, expected_mode == PermissionMode::Full);
        client
            .call(
                "UnloadSideChat",
                json!({"id":created["meta"]["id"],"lease":created["lease"]}),
            )
            .await
            .unwrap();
    }
    let created = client
        .call(
            "CreateSideChat",
            json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap();
    let mut config = core
        .workspace
        .chat("parent")
        .unwrap()
        .unwrap()
        .config
        .unwrap();
    config.sandbox = SandboxLevel::ReadOnly;
    config.permission_mode = PermissionMode::Ask;
    config.effective_permission_mode = PermissionMode::Ask;
    core.workspace.set_chat_config("parent", &config).unwrap();
    client.call("SendSideChat", json!({"id":created["meta"]["id"],"lease":created["lease"],"messageId":uuid::Uuid::new_v4().to_string(),"text":"PARENT_TIGHTENED"})).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while recorder.0.lock().unwrap().len() < 4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        recorder.0.lock().unwrap()[3].sandbox,
        SandboxLevel::ReadOnly
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn side_creation_cannot_claim_an_existing_regular_task_id() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let other = uuid::Uuid::new_v4().to_string();
    client
        .call(
            "Mutate",
            json!({"op":"createChat","chatId":other,"deviceId":core.device_id}),
        )
        .await
        .unwrap();
    core.workspace
        .rename_chat(&other, "Unrelated task")
        .unwrap();
    assert!(
        client
            .call(
                "CreateSideChat",
                json!({"parentId":"parent","requestId":other})
            )
            .await
            .is_err()
    );
    assert_eq!(
        core.workspace
            .chat(&other)
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Unrelated task")
    );
    assert!(!core.side_chats.contains(&other));
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn side_documents_and_tool_blobs_do_not_export_to_configured_edge() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let observed = observed.clone();
            tokio::spawn(async move {
                let mut buffer = [0_u8; 4096];
                if let Ok(Ok(count)) =
                    tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buffer)).await
                {
                    let request = String::from_utf8_lossy(&buffer[..count]);
                    if let Some(path) = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                    {
                        observed.lock().unwrap().push(path.to_string());
                    }
                    let _ = socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                }
            });
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(recorder);
    let core = EngineCore::assemble(
        dir.path(),
        registry,
        HarnessId::Mock,
        Some(luna_engine::EdgeConfig::with_static_token(
            &base,
            "synthetic-token",
        )),
    )
    .unwrap();
    let client = luna_rpc::memory_client(core.rpc_service());
    client.call("Mutate", json!({"op":"createChat","chatId":"export-parent","deviceId":core.device_id,
        "config":{"harness":"mock","model":"fixture","reasoning":null,"sandbox":"workspace-write","permissionMode":"ask","effectivePermissionMode":"ask"}})).await.unwrap();
    core.workspace
        .rename_chat("export-parent", "Export fixture parent")
        .unwrap();
    let created = client
        .call(
            "CreateSideChat",
            json!({"parentId":"export-parent","requestId":uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap();
    let child = created["meta"]["id"].as_str().unwrap();
    client.call("SendSideChat", json!({"id":child,"lease":created["lease"],"messageId":"private-side-question","text":"PRIVATE_SIDE_SENTINEL"})).await.unwrap();
    let subdoc = format!("{child}--sub--fixture");
    core.doc_host
        .open(&subdoc)
        .unwrap()
        .write_user_message("sub-message", "PRIVATE_SUBDOC", 1)
        .unwrap();
    core.doc_host.upload_tool_sidecar(
        child,
        luna_doc::SidecarPayload {
            part_id: "private-tool".into(),
            output: Some("PRIVATE_TOOL_OUTPUT".into()),
            diff: None,
        },
    );
    assert!(
        core.doc_host
            .fetch_tool_blob(&format!("{child}/private-tool"))
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains("export-parent")),
        "configured edge control path was exercised"
    );
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains(child)),
        "side document or blob escaped to Edge"
    );
    assert!(!core.store.chat_outbox_initialized(child).unwrap());
    assert!(!core.store.chat_outbox_initialized(&subdoc).unwrap());
    assert!(
        core.workspace
            .chat(child)
            .unwrap()
            .unwrap()
            .last_message_preview
            .is_none(),
        "side content must not leak through registry previews"
    );
    // A restored pre-suppression outbox must remain local too. Keep it intact
    // for inspection instead of silently acknowledging or deleting its bytes.
    let side_update = core
        .doc_host
        .open(child)
        .unwrap()
        .doc()
        .doc()
        .export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .unwrap();
    let sub_update = core
        .doc_host
        .open(&subdoc)
        .unwrap()
        .doc()
        .doc()
        .export(loro::ExportMode::updates(&loro::VersionVector::default()))
        .unwrap();
    core.store.initialize_chat_outbox(child, &[]).unwrap();
    core.store
        .enqueue_chat_update(child, "restored-side-batch", &side_update)
        .unwrap();
    core.store.initialize_chat_outbox(&subdoc, &[]).unwrap();
    core.store
        .enqueue_chat_update(&subdoc, "restored-sub-batch", &sub_update)
        .unwrap();
    core.shutdown().await;
    drop(client);
    drop(core);
    requests.lock().unwrap().clear();
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(Recorder::default()));
    let reopened = EngineCore::assemble(
        dir.path(),
        registry,
        HarnessId::Mock,
        Some(luna_engine::EdgeConfig::with_static_token(
            &base,
            "synthetic-token",
        )),
    )
    .unwrap();
    reopened.doc_host.open(child).unwrap();
    reopened.doc_host.open(&subdoc).unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains(child)),
        "restored side outbox attempted to leave the owning profile"
    );
    assert!(
        reopened
            .store
            .pending_chat_updates(child)
            .unwrap()
            .iter()
            .any(|(id, bytes)| id == "restored-side-batch" && bytes == &side_update)
    );
    assert!(
        reopened
            .store
            .pending_chat_updates(&subdoc)
            .unwrap()
            .iter()
            .any(|(id, bytes)| id == "restored-sub-batch" && bytes == &sub_update)
    );
    reopened.shutdown().await;
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn fallback_side_chat_registers_managed_identity_without_starting_provider() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let created = client
        .call(
            "CreateSideChat",
            json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap();
    let id = created["meta"]["id"].as_str().unwrap();
    let identity = core
        .task_identity
        .view(id)
        .expect("fallback must retain normal managed task authority");
    assert_eq!(
        identity.task.lineage.source_task_id.as_deref(),
        Some("parent")
    );
    assert!(recorder.0.lock().unwrap().is_empty());
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn abandoned_creation_reservation_expires_without_runtime_or_replay() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let request = uuid::Uuid::new_v4().to_string();
    let reserved = core
        .side_chats
        .prepare("parent", &request, false, None, None, false)
        .await
        .unwrap();
    assert_eq!(reserved.meta.state, "preparing");
    let db = rusqlite::Connection::open(core.side_chats.database_path()).unwrap();
    db.execute(
        "UPDATE side_chats SET expires_at_ms=0 WHERE id=?1",
        [&reserved.meta.id],
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let page = core.side_chats.list("parent", None).unwrap();
            if page.items[0].state == "creationFailed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        core.side_chats
            .prepare("parent", &request, false, None, None, false)
            .await
            .is_err()
    );
    assert!(recorder.0.lock().unwrap().is_empty());
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_unload_storage_still_stops_generation_and_can_be_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let created = client
        .call(
            "CreateSideChat",
            json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string()}),
        )
        .await
        .unwrap();
    let id = created["meta"]["id"].as_str().unwrap();
    client.call("SendSideChat", json!({"id":id,"lease":created["lease"],"messageId":"hold-runtime","text":"HOLD_SIDE_RUNTIME"})).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while recorder.0.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let db = rusqlite::Connection::open(core.side_chats.database_path()).unwrap();
    db.execute_batch("CREATE TRIGGER reject_side_close BEFORE UPDATE OF state ON side_chats WHEN NEW.state='closing' BEGIN SELECT RAISE(ABORT, 'synthetic metadata write failure'); END;").unwrap();
    assert!(
        client
            .call("UnloadSideChat", json!({"id":id,"lease":created["lease"]}))
            .await
            .is_err()
    );
    assert!(
        !core.sessions.has_live_run(id),
        "metadata failure must not keep generation running"
    );
    assert!(client.call("SendSideChat", json!({"id":id,"lease":created["lease"],"messageId":"after-failed-unload","text":"must not run"})).await.is_err());
    db.execute_batch("DROP TRIGGER reject_side_close").unwrap();
    // Reopening saved history after a failed close must finish the stopped
    // transcript projection, without requiring a lost view's lease token.
    let history = client.call("ReadSideChat", json!({"id":id})).await.unwrap();
    assert_eq!(history["meta"]["state"], "unloaded");
    assert!(
        !core
            .doc_host
            .sync_statuses()
            .iter()
            .any(|(chat, _)| chat == id)
    );
    assert!(history.to_string().contains("SIDE_PARTIAL_BEFORE_STOP"));
    assert_eq!(recorder.0.lock().unwrap().len(), 1);
    core.shutdown().await;
}
