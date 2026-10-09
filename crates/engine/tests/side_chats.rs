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
        core.workspace
            .create_space("space-side", &core.device_id, "/tmp", None, false)
            .expect("create project");
        client
            .call(
                "Mutate",
                json!({"op":"createChat","chatId":"parent","spaceId":"space-side"}),
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

fn fork_evidence(name: &str, value: Value) {
    if let Ok(dir) = std::env::var("FORK_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join(name),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn persistent_forks_migrate_legacy_states_without_recreating_identity() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let mut saved = Vec::new();
    for state in [
        "ended",
        "expired",
        "unloaded",
        "closing",
        "closeFailed",
        "recovered",
        "preparing",
    ] {
        let created = client.call("CreateSideChat", json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string(),"forceSnapshot":true})).await.unwrap();
        let id = created["meta"]["id"].as_str().unwrap().to_owned();
        core.doc_host
            .open(&id)
            .unwrap()
            .write_user_message("saved-before-migration", &format!("SAVED_{state}"), 1)
            .unwrap();
        saved.push((
            id.clone(),
            state.to_owned(),
            serde_json::to_value(core.task_identity.view(&id).unwrap()).unwrap(),
        ));
    }
    let db_path = core.side_chats.database_path().to_owned();
    drop(client);
    core.shutdown().await;
    drop(core);
    let db = rusqlite::Connection::open(&db_path).unwrap();
    for (id, state, _) in &saved {
        db.execute("UPDATE side_chats SET state=?2,expires_at_ms=1,detail='legacy close marker' WHERE id=?1", rusqlite::params![id,state]).unwrap();
    }
    drop(db);
    let mut evidence = Vec::new();
    for restart in 0..2 {
        let core = setup(dir.path(), recorder.clone()).await;
        let client = luna_rpc::memory_client(core.rpc_service());
        for (id, state, identity) in &saved {
            let page = client.call("ReadSideChat", json!({"id":id})).await.unwrap();
            assert_eq!(page["meta"]["state"], "active", "{state}: {page}");
            assert_eq!(page["meta"]["expiresAtMs"], 0);
            assert!(page.to_string().contains(&format!("SAVED_{state}")));
            let current = serde_json::to_value(core.task_identity.view(id).unwrap()).unwrap();
            assert_eq!(current["task"]["taskId"], identity["task"]["taskId"]);
            assert_eq!(current["currentBinding"], identity["currentBinding"]);
            assert_eq!(current["task"]["lineage"], identity["task"]["lineage"]);
            client
                .call("GetTaskIdentity", json!({"taskId":id}))
                .await
                .unwrap();
            client
                .call("GetChatWorkbench", json!({"mainChatId":id}))
                .await
                .unwrap();
            evidence.push(json!({"restart":restart,"legacyState":state,"id":id,"identity":current,"page":page}));
        }
        assert!(
            recorder.0.lock().unwrap().is_empty(),
            "migration and reads never start a provider"
        );
        drop(client);
        core.shutdown().await;
        drop(core);
    }
    fork_evidence("legacy-migration.json", json!(evidence));
}

#[tokio::test(flavor = "multi_thread")]
async fn persistent_forks_are_nested_discoverable_and_independent_of_source() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let created = client.call("CreateSideChat", json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string(),"forceSnapshot":true})).await.unwrap();
    let id = created["meta"]["id"].as_str().unwrap();
    // Side chats no longer nest (chat-model §1): a side chat of a side chat
    // is refused with the allowed alternatives. A second side chat of the
    // same main chat carries the independence checks below.
    let refused = client.call("CreateSideChat", json!({"parentId":id,"requestId":uuid::Uuid::new_v4().to_string(),"forceSnapshot":true})).await.unwrap_err();
    let refused = match refused {
        luna_rpc::RpcError::Failed(message) => luna_proto::BoardError::from_rpc_message(&message)
            .unwrap_or_else(|| panic!("not a board error: {message}")),
        other => panic!("unexpected transport error: {other}"),
    };
    assert!(
        matches!(&refused, luna_proto::BoardError::InvalidRole { allowed, .. } if !allowed.is_empty()),
        "{refused:?}"
    );
    let nested = client.call("CreateSideChat", json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string(),"forceSnapshot":true})).await.unwrap();
    let nested_id = nested["meta"]["id"].as_str().unwrap();
    for target in [id, nested_id] {
        client
            .call("GetChatWorkbench", json!({"mainChatId":target}))
            .await
            .unwrap();
        for archived in [true, false] {
            let result = client
                .call(
                    "Mutate",
                    json!({"op":"setChatArchived","chatId":target,"archived":archived}),
                )
                .await
                .unwrap();
            assert_eq!(result["receipt"]["status"], "applied", "{result}");
        }
    }
    client
        .call("Mutate", json!({"op":"deleteChat","chatId":"parent"}))
        .await
        .unwrap();
    assert!(core.workspace.chat("parent").unwrap().is_none());
    assert!(core.workspace.chat(id).unwrap().is_some());
    assert!(core.workspace.chat(nested_id).unwrap().is_some());
    let mut watch = client.subscribe("WatchChats", json!({})).await.unwrap();
    let snapshot = watch.recv().await.unwrap();
    for target in [id, nested_id] {
        let row = snapshot
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == target)
            .unwrap();
        assert_eq!(row["spaceId"], "space-side", "{row}");
        assert!(row["sideChatParentId"].is_string());
    }
    client
        .call(
            "SendSideChat",
            json!({"id":id,"messageId":"orphan-send","text":"SOURCE_INDEPENDENT"}),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while core.sessions.turn_in_flight(id) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(recorder.0.lock().unwrap().len(), 1);
    let page = client.call("ReadSideChat", json!({"id":id})).await.unwrap();
    assert!(page.to_string().contains("SOURCE_INDEPENDENT"));
    client
        .call("Mutate", json!({"op":"deleteChat","chatId":id}))
        .await
        .unwrap();
    assert!(core.workspace.chat(id).unwrap().is_none());
    assert!(core.workspace.chat(nested_id).unwrap().is_some());
    // An explicit delete retires the fork itself: no history read, no send.
    assert!(client.call("ReadSideChat", json!({"id":id})).await.is_err());
    assert!(
        client
            .call(
                "SendSideChat",
                json!({"id":id,"messageId":"after-delete","text":"must not run"})
            )
            .await
            .is_err()
    );
    assert_eq!(recorder.0.lock().unwrap().len(), 1);
    fork_evidence(
        "source-independence.json",
        json!({"created":created,"nested":nested,"snapshot":snapshot,"page":page}),
    );
    core.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn imported_fork_history_stays_visible_without_implicit_provider_adoption() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let assemble = |profile| {
        let registry = Arc::new(HarnessRegistry::new());
        registry.register(recorder.clone());
        EngineCore::assemble_with_profile(profile, registry, HarnessId::Mock, None).unwrap()
    };
    let local = assemble(luna_engine::EngineProfile::local(root.path()).unwrap());
    local
        .workspace
        .create_space("import-fork-space", &local.device_id, "/tmp", None, false)
        .unwrap();
    let client = luna_rpc::memory_client(local.rpc_service());
    client.call("Mutate", json!({"op":"createChat","chatId":"import-fork-source","spaceId":"import-fork-space",
        "config":{"harness":"mock","model":"fixture","sandbox":"workspace-write","permissionMode":"ask","effectivePermissionMode":"ask"}})).await.unwrap();
    let created = client.call("CreateSideChat", json!({"parentId":"import-fork-source","requestId":uuid::Uuid::new_v4().to_string(),"forceSnapshot":true})).await.unwrap();
    let id = created["meta"]["id"].as_str().unwrap().to_owned();
    local
        .doc_host
        .open(&id)
        .unwrap()
        .write_user_message("import-preserved", "IMPORTED_FORK_HISTORY", 1)
        .unwrap();
    local
        .doc_host
        .open(&id)
        .unwrap()
        .doc()
        .push_message(&luna_doc::SessionMessageEntry {
            id: "import-partial".into(),
            role: luna_doc::MessageRole::Assistant,
            parts: vec![luna_doc::MessagePart::Text {
                id: "partial-text".into(),
                text: "IMPORTED_PARTIAL_HISTORY".into(),
            }],
            created_at: 2,
            device_id: local.device_id.clone(),
            status: Some(luna_doc::MessageStatus::Streaming),
            continuation_of: None,
        })
        .unwrap();
    let source_db = local.side_chats.database_path().to_owned();
    drop(client);
    local.shutdown().await;
    drop(local);
    // Model a legacy import with only its cold SQL projection. This profile
    // is closed; no live document writer can recreate the removed snapshot.
    let source = rusqlite::Connection::open(&source_db).unwrap();
    assert_eq!(
        source
            .execute("DELETE FROM snapshots WHERE doc_id=?1", [&id])
            .unwrap(),
        1
    );
    source.execute("UPDATE side_chat_entries SET entry=json_set(entry,'$.status','streaming') WHERE chat_id=?1 AND json_extract(entry,'$.id')='import-partial'", [&id]).unwrap();
    drop(source);
    let synced = assemble(luna_engine::EngineProfile::synced(
        root.path(),
        "fork-org",
        "fork-user",
    ));
    let mut imported = Vec::new();
    synced
        .local_import
        .as_ref()
        .unwrap()
        .run(|event| imported.push(event))
        .unwrap();
    assert!(synced.workspace.chat(&id).unwrap().is_some());
    assert!(
        synced.task_identity.view(&id).is_none(),
        "local import does not adopt managed provider bindings"
    );
    let client = luna_rpc::memory_client(synced.rpc_service());
    let history = client.call("ReadSideChat", json!({"id":id})).await.unwrap();
    assert!(history.to_string().contains("IMPORTED_FORK_HISTORY"));
    let mut messages = client
        .subscribe("WatchDocMessages", json!({"chatId":id}))
        .await
        .unwrap();
    let initial = messages.recv().await.unwrap();
    assert!(initial.to_string().contains("IMPORTED_PARTIAL_HISTORY"));
    drop(messages);
    client
        .call("GetChatWorkbench", json!({"mainChatId":id}))
        .await
        .unwrap();
    let rejected = client
        .call(
            "SendSideChat",
            json!({"id":id,"messageId":"import-must-not-run","text":"DO_NOT_START_REPLACEMENT"}),
        )
        .await
        .unwrap_err();
    assert!(
        rejected
            .to_string()
            .contains("managed provider identity is unavailable"),
        "{rejected}"
    );
    assert!(recorder.0.lock().unwrap().is_empty());
    assert!(
        !synced
            .doc_host
            .open(&id)
            .unwrap()
            .doc()
            .read_entries()
            .unwrap()
            .iter()
            .any(|entry| entry.id == "import-must-not-run")
    );
    fork_evidence(
        "local-import-binding-guard.json",
        json!({"id":id,"history":history,"error":rejected.to_string(),"providerRuns":0}),
    );
    drop(client);
    synced.shutdown().await;
    drop(synced);
    let synced = assemble(luna_engine::EngineProfile::synced(
        root.path(),
        "fork-org",
        "fork-user",
    ));
    let client = luna_rpc::memory_client(synced.rpc_service());
    let restarted = client.call("ReadSideChat", json!({"id":id})).await.unwrap();
    assert!(
        restarted["meta"]["detail"]
            .as_str()
            .unwrap()
            .starts_with("Imported fork:")
    );
    assert_eq!(
        restarted["entries"], history["entries"],
        "imported history is preserved exactly through ordinary open and restart"
    );
    assert_eq!(
        restarted["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == "import-partial")
            .unwrap()["status"],
        "streaming"
    );
    fork_evidence("local-import-after-restart.json", restarted);
    assert!(recorder.0.lock().unwrap().is_empty());
    drop(client);
    synced.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn side_history_projection_removes_rewound_tail_and_empty_history() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let created = client.call("CreateSideChat", json!({
        "parentId": "parent", "requestId": uuid::Uuid::new_v4().to_string(), "forceSnapshot": true,
    })).await.unwrap();
    let id = created["meta"]["id"].as_str().unwrap();
    let lease = created["lease"].clone();
    for (message, text) in [
        ("projection-a", "SIDE_PREFIX"),
        ("projection-b", "SIDE_REMOVED_TAIL"),
    ] {
        client
            .call(
                "SendSideChat",
                json!({
                    "id": id, "lease": lease, "messageId": message, "text": text,
                }),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let entries = core
                    .doc_host
                    .open(id)
                    .unwrap()
                    .doc()
                    .read_entries()
                    .unwrap();
                if entries.iter().any(|entry| entry.id == message)
                    && !core.sessions.turn_in_flight(id)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    // Read through the production RPC to populate the durable sequence projection.
    let before = client
        .call("ReadSideChat", json!({"id": id, "lease": lease}))
        .await
        .unwrap();
    assert!(before.to_string().contains("SIDE_REMOVED_TAIL"));
    let handle = core.doc_host.open(id).unwrap();
    let entries = handle.doc().read_entries().unwrap();
    let tail = entries
        .iter()
        .position(|entry| entry.id == "projection-b")
        .unwrap();
    // This is the same conversation replacement primitive used by native rewind.
    handle
        .doc()
        .replace_conversation_entries(&entries[..tail], &Default::default())
        .unwrap();
    let after = client
        .call("ReadSideChat", json!({"id": id, "lease": lease}))
        .await
        .unwrap();
    assert!(
        !after.to_string().contains("SIDE_REMOVED_TAIL"),
        "{after:#}"
    );
    handle
        .doc()
        .replace_conversation_entries(&[], &Default::default())
        .unwrap();
    let empty = client
        .call("ReadSideChat", json!({"id": id, "lease": lease}))
        .await
        .unwrap();
    assert_eq!(empty["entries"].as_array().unwrap().len(), 0, "{empty:#}");
    client
        .call("UnloadSideChat", json!({"id": id, "lease": lease}))
        .await
        .unwrap();
    let reopened = client
        .call("ReadSideChat", json!({"id": id}))
        .await
        .unwrap();
    assert_eq!(
        reopened["entries"].as_array().unwrap().len(),
        0,
        "{reopened:#}"
    );
    client
        .call("GetTaskIdentity", json!({"taskId":id}))
        .await
        .unwrap();
    assert_eq!(reopened["meta"]["state"], "active");
    core.shutdown().await;
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
    let lease = created["lease"].as_str().unwrap_or_default();
    assert_eq!(created["meta"]["expiresAtMs"].as_i64().unwrap(), 0);
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
    assert_eq!(request.parent_permission_ceiling, None);
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
    let history = client.call("ReadSideChat", json!({"id":id})).await.unwrap();
    assert_eq!(history["meta"]["state"], "active");
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
async fn deadlines_and_restart_do_not_end_side_chats_or_replay_turns() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    let mut ids = Vec::new();
    for _ in 0..6 {
        let created = client
            .call(
                "CreateSideChat",
                json!({"parentId":"parent","requestId":uuid::Uuid::new_v4().to_string()}),
            )
            .await
            .unwrap();
        assert_eq!(created["meta"]["expiresAtMs"], 0);
        assert!(created["lease"].is_null());
        ids.push(created["meta"]["id"].as_str().unwrap().to_owned());
    }
    let db = rusqlite::Connection::open(core.side_chats.database_path()).unwrap();
    db.execute("UPDATE side_chats SET expires_at_ms=1", [])
        .unwrap();
    drop(db);
    for id in &ids {
        let page = client.call("ReadSideChat", json!({"id":id})).await.unwrap();
        assert_eq!(page["meta"]["state"], "active");
    }
    assert!(
        recorder.0.lock().unwrap().is_empty(),
        "opening never dispatches"
    );
    drop(client);
    core.shutdown().await;
    drop(core);
    let reopened = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(reopened.rpc_service());
    for id in &ids {
        let page = client.call("ReadSideChat", json!({"id":id})).await.unwrap();
        assert_eq!(page["meta"]["state"], "active");
    }
    assert!(
        recorder.0.lock().unwrap().is_empty(),
        "restart never dispatches"
    );
    client
        .call(
            "SendSideChat",
            json!({"id":ids[0],"messageId":"after-restart","text":"new turn after restart"}),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while recorder.0.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    client
        .call("UnloadSideChat", json!({"id":ids[0]}))
        .await
        .unwrap();
    assert_eq!(
        client
            .call("ReadSideChat", json!({"id":ids[0]}))
            .await
            .unwrap()["meta"]["state"],
        "active"
    );
    drop(client);
    reopened.shutdown().await;
    drop(reopened);
    let reopened = setup(dir.path(), recorder.clone()).await;
    assert_eq!(
        reopened
            .side_chats
            .list("parent", None)
            .unwrap()
            .items
            .iter()
            .find(|meta| meta.id == ids[0])
            .unwrap()
            .state,
        "active"
    );
    assert_eq!(recorder.0.lock().unwrap().len(), 1);
    reopened.shutdown().await;
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
async fn paged_history_and_restart_never_start_a_provider() {
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
    core.shutdown().await;
    drop(client);
    drop(core);
    let reopened = setup(dir.path(), recorder.clone()).await;
    assert!(recorder.0.lock().unwrap().is_empty());
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fork_permissions_follow_explicit_own_configuration() {
    use luna_proto::{PermissionMode, SandboxLevel};
    let dir = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder::default());
    let core = setup(dir.path(), recorder.clone()).await;
    let client = luna_rpc::memory_client(core.rpc_service());
    // Whatever the source chat's access, the fork's own explicit grant decides
    // what the fork runs with.
    for (index, (sandbox, mode)) in [
        (SandboxLevel::ReadOnly, PermissionMode::Ask),
        (SandboxLevel::WorkspaceWrite, PermissionMode::Full),
        (SandboxLevel::DangerFullAccess, PermissionMode::Full),
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
        let side_id = created["meta"]["id"].as_str().unwrap();
        let changed = client
            .call(
                "Mutate",
                json!({
                    "op":"setThreadPermissionMode", "chatId":side_id, "mode":"full"
                }),
            )
            .await
            .unwrap();
        assert_eq!(changed["receipt"]["status"], "applied", "{changed}");
        assert_eq!(
            core.workspace
                .chat(side_id)
                .unwrap()
                .unwrap()
                .config
                .unwrap()
                .effective_permission_mode,
            PermissionMode::Full,
            "an explicit user grant changes the fork itself"
        );
        client.call("SendSideChat", json!({"id":created["meta"]["id"],"lease":created["lease"],"messageId":uuid::Uuid::new_v4().to_string(),"text":"PERMISSION_BOUNDARY_ONLY"})).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while recorder.0.lock().unwrap().len() <= index {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let request = recorder.0.lock().unwrap()[index].clone();
        assert_eq!(request.sandbox, SandboxLevel::DangerFullAccess);
        assert_eq!(
            request.permission_policy.as_ref().unwrap().effective_mode,
            PermissionMode::Full
        );
        assert!(request.auto_approve);
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
        SandboxLevel::DangerFullAccess
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
            json!({"op":"createChat","chatId":other,"spaceId":"space-side"}),
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
    core.workspace
        .create_space("space-export", &core.device_id, "/tmp", None, false)
        .expect("create project");
    let client = luna_rpc::memory_client(core.rpc_service());
    client.call("Mutate", json!({"op":"createChat","chatId":"export-parent","spaceId":"space-export",
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
    // Native-agent inspection reads the derived document through its active
    // side owner. The derived id must never become an independent writable chat.
    let mut sub_messages = client
        .subscribe_checked("WatchDocMessages", json!({"chatId":subdoc}))
        .await
        .expect("active side owner admits its native-agent transcript");
    let initial = tokio::time::timeout(Duration::from_secs(5), sub_messages.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(initial.to_string().contains("PRIVATE_SUBDOC"), "{initial}");
    drop(sub_messages);
    assert!(
        client
            .call(
                "QueueMessage",
                json!({"chatId":subdoc,"text":"forbidden derived mutation"})
            )
            .await
            .is_err()
    );
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
    client
        .call(
            "UnloadSideChat",
            json!({"id":child,"lease":created["lease"]}),
        )
        .await
        .unwrap();
    assert!(
        client
            .subscribe_checked("WatchDocMessages", json!({"chatId":subdoc}))
            .await
            .is_ok(),
        "closing a view must preserve ordinary derived transcript reads"
    );
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
async fn abandoned_creation_is_reconciled_after_restart_without_runtime_or_replay() {
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
    core.shutdown().await;
    drop(core);
    let reopened = setup(dir.path(), recorder.clone()).await;
    assert_eq!(
        reopened.side_chats.list("parent", None).unwrap().items[0].state,
        "creationFailed"
    );
    assert!(
        reopened
            .side_chats
            .prepare("parent", &request, false, None, None, false)
            .await
            .is_err()
    );
    assert!(recorder.0.lock().unwrap().is_empty());
    reopened.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_view_close_does_not_write_lifecycle_or_stop_a_turn() {
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
        .call(
            "SendSideChat",
            json!({"id":id,"messageId":"hold-runtime","text":"HOLD_SIDE_RUNTIME"}),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while recorder.0.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let db = rusqlite::Connection::open(core.side_chats.database_path()).unwrap();
    db.execute_batch("CREATE TRIGGER reject_side_close BEFORE UPDATE OF state ON side_chats BEGIN SELECT RAISE(ABORT, 'view close must not mutate lifecycle'); END;").unwrap();
    let closed = client
        .call("UnloadSideChat", json!({"id":id}))
        .await
        .unwrap();
    assert_eq!(closed["state"], "active");
    assert!(core.sessions.has_live_run(id));
    client.call("StopSideChat", json!({"id":id})).await.unwrap();
    assert!(!core.sessions.has_live_run(id));
    assert_eq!(recorder.0.lock().unwrap().len(), 1);
    let history = client.call("ReadSideChat", json!({"id":id})).await.unwrap();
    assert!(history.to_string().contains("SIDE_PARTIAL_BEFORE_STOP"));
    assert_eq!(history["meta"]["state"], "active");
    assert!(
        core.store.task_recovery(id).unwrap().is_none(),
        "legacy direct send never acquires a replay chain"
    );
    fork_evidence(
        "legacy-view-close.json",
        json!({"closed":closed,"history":history}),
    );
    db.execute_batch("DROP TRIGGER reject_side_close").unwrap();
    core.shutdown().await;
}
