//! Failure-first RPC/persistence scenarios for agent workspace controls.
//! See docs/design/agent-workspace-controls/README.md. All provider homes are
//! synthetic; evidence is emitted when AGENT_WORKSPACE_EVIDENCE_DIR is set.
use std::{path::Path, sync::Arc};

use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::CodexHarness;
use luna_proto::HarnessId;
use serde_json::{Value, json};

fn engine(root: &Path) -> EngineCore {
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(CodexHarness::new().with_executable(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-codex.sh"),
    )));
    let accounts = AgentAccountsConfig {
        data_dir: root.join("accounts"),
        claude_config_dir: root.join("claude"),
        claude_config_file: root.join("claude.json"),
        codex_home: root.join("codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("engine"), "workspace-qa", "workspace-qa"),
        registry,
        HarnessId::Codex,
        None,
        accounts,
    )
    .unwrap()
}

fn evidence(name: &str, value: Value) {
    if let Ok(dir) = std::env::var("AGENT_WORKSPACE_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Path::new(&dir).join(name),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

async fn seed(core: &EngineCore, rpc: &luna_rpc::RpcClient, root: &Path) -> (String, String) {
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    core.workspace
        .create_space(
            "project",
            &core.device_id,
            project.to_str().unwrap(),
            None,
            false,
        )
        .unwrap();
    let parent = uuid::Uuid::new_v4().to_string();
    let other = uuid::Uuid::new_v4().to_string();
    for id in [&parent, &other] {
        rpc.call("Mutate", json!({"op":"createChat","chatId":id,"spaceId":"project","cwd":project,
            "config":{"harness":"codex","model":"gpt-5.6-terra","reasoning":"low","modelOptions":{},"permissionMode":"ask","effectivePermissionMode":"ask","sandbox":"read-only"}})).await.unwrap();
    }
    (parent, other)
}

#[tokio::test]
async fn provider_defaults_persist_reject_stale_or_unsupported_choices_and_resolve_children() {
    let root = tempfile::tempdir().unwrap();
    let core = engine(root.path());
    let service = core.rpc_service();
    let rpc = luna_rpc::memory_client(service.clone());
    let (parent, _) = seed(&core, &rpc, root.path()).await;
    let before = rpc.call("GetProviderDefaults", json!({})).await.unwrap();
    assert_eq!(before, json!({"revision":0,"providers":{}}));
    let selection =
        json!({"model":"gpt-6-astra","reasoning":"high","modelOptions":{"serviceTier":"fast"}});
    let saved = rpc
        .call(
            "SetProviderDefaults",
            json!({"expectedRevision":0,"provider":"codex","defaults":selection}),
        )
        .await
        .unwrap();
    assert_eq!(saved["revision"], 1);
    assert_eq!(saved["providers"]["codex"], selection);
    assert!(
        rpc.call(
            "SetProviderDefaults",
            json!({"expectedRevision":0,"provider":"codex","defaults":null})
        )
        .await
        .is_err()
    );
    for bad in [
        json!({"model":"missing-model","reasoning":"high","modelOptions":{}}),
        json!({"model":"gpt-5.6-terra","reasoning":"ultra","modelOptions":{}}),
        json!({"model":"gpt-6-astra","reasoning":"high","modelOptions":{"serviceTier":"invented"}}),
        json!({"model":"gpt-6-astra","reasoning":"high","modelOptions":{"lunaRuntimeContract":"injected"}}),
    ] {
        assert!(
            rpc.call(
                "SetProviderDefaults",
                json!({"expectedRevision":1,"provider":"codex","defaults":bad})
            )
            .await
            .is_err()
        );
    }
    assert_eq!(
        rpc.call("GetProviderDefaults", json!({})).await.unwrap(),
        saved
    );
    let child = service.seed_child_for_qa(parent.clone(), serde_json::from_value(json!({
        "prompt":"Return a short review.","idempotencyKey":"defaults-child","reportResults":false,"continueParent":false
    })).unwrap()).await.unwrap();
    let child_config = core
        .workspace
        .chat(&child.task_id)
        .unwrap()
        .unwrap()
        .config
        .unwrap();
    assert_eq!(child_config.model.as_deref(), Some("gpt-6-astra"));
    assert_eq!(
        child_config.reasoning,
        Some(luna_proto::ReasoningLevel::High)
    );
    assert_eq!(
        child_config.model_options.get("serviceTier"),
        Some(&json!("fast"))
    );
    assert_eq!(
        core.workspace
            .chat(&parent)
            .unwrap()
            .unwrap()
            .config
            .unwrap()
            .model
            .as_deref(),
        Some("gpt-5.6-terra")
    );
    let explicit = service.seed_child_for_qa(parent, serde_json::from_value(json!({
        "prompt":"Return a short review.","idempotencyKey":"explicit-child","model":"gpt-5.6-terra","reasoning":"low","reportResults":false
    })).unwrap()).await.unwrap();
    let explicit_config = core
        .workspace
        .chat(&explicit.task_id)
        .unwrap()
        .unwrap()
        .config
        .unwrap();
    assert_eq!(explicit_config.model.as_deref(), Some("gpt-5.6-terra"));
    assert_ne!(
        explicit_config.model_options.get("serviceTier"),
        Some(&json!("fast")),
        "different explicit model must not inherit saved fast speed"
    );
    evidence(
        "provider-defaults.json",
        json!({"before":before,"saved":saved,"child":child,"explicit":explicit}),
    );
    drop(rpc);
    drop(service);
    core.shutdown().await;
    drop(core);
    let reopened = engine(root.path());
    let rpc = luna_rpc::memory_client(reopened.rpc_service());
    assert_eq!(
        rpc.call("GetProviderDefaults", json!({})).await.unwrap(),
        saved
    );
    let cleared = rpc
        .call(
            "SetProviderDefaults",
            json!({"expectedRevision":1,"provider":"codex","defaults":null}),
        )
        .await
        .unwrap();
    assert_eq!(cleared, json!({"revision":2,"providers":{}}));
    drop(rpc);
    reopened.shutdown().await;
}

#[tokio::test]
async fn background_open_preserves_selection_and_close_preserves_conversation() {
    let root = tempfile::tempdir().unwrap();
    let core = engine(root.path());
    let rpc = luna_rpc::memory_client(core.rpc_service());
    let (main, other) = seed(&core, &rpc, root.path()).await;
    let profile = rpc.call("EngineInfo", json!({})).await.unwrap()["profileId"].clone();
    let before = rpc
        .call("GetChatWorkbench", json!({"mainChatId":main}))
        .await
        .unwrap();
    let request = json!({"mainChatId":main,"profileId":profile,"expectedRevision":before["revision"],"commandId":"background-open",
        "command":{"op":"openBackgroundTab","tab":{"chatId":other}}});
    let opened = rpc
        .call("MutateChatWorkbench", request.clone())
        .await
        .unwrap();
    assert_eq!(opened["conflict"], false);
    let layout = &opened["snapshot"];
    assert_eq!(layout["focusedPaneId"], before["focusedPaneId"]);
    assert_eq!(
        layout["root"]["activeChatId"],
        before["root"]["activeChatId"]
    );
    assert_eq!(layout["root"]["tabs"].as_array().unwrap().len(), 2);
    let replay = rpc.call("MutateChatWorkbench", request).await.unwrap();
    assert_eq!(replay["snapshot"], *layout);
    let closed = rpc.call("MutateChatWorkbench", json!({"mainChatId":main,"profileId":profile,"expectedRevision":layout["revision"],"commandId":"background-close",
        "command":{"op":"closeTab","chatId":other}})).await.unwrap();
    assert_eq!(
        closed["snapshot"]["root"]["tabs"].as_array().unwrap().len(),
        1
    );
    assert!(!core.workspace.chat(&other).unwrap().unwrap().archived);
    evidence(
        "background-tab.json",
        json!({"before":before,"opened":opened,"replayed":replay,"closed":closed}),
    );
    drop(rpc);
    core.shutdown().await;
}

#[tokio::test]
async fn agent_catalog_exposes_bound_workspace_and_rejects_unrelated_targets() {
    use luna_engine::cross_task_tools::{CrossTaskAuthority, CrossTaskBackend, CrossTaskTools};
    let root = tempfile::tempdir().unwrap();
    let core = engine(root.path());
    let service = core.rpc_service();
    let rpc = luna_rpc::memory_client(service.clone());
    let (main, unrelated) = seed(&core, &rpc, root.path()).await;
    let child = service.seed_child_for_qa(main.clone(), serde_json::from_value(json!({
        "prompt":"A separate fresh brief", "idempotencyKey":"workspace-child", "reportResults":false
    })).unwrap()).await.unwrap();
    let authority = CrossTaskAuthority {
        caller_task_id: main.clone(),
        local_environment_id: core.device_id.clone(),
        visible_project_ids: std::collections::HashSet::from(["project".into()]),
        allow_projectless: false,
        granted_scopes: std::collections::HashSet::from(["threads.read".into()]),
        explicit_user_request: false,
        delivery: luna_proto::RuntimeDelivery::CodexMcpTools,
    };
    let tools = CrossTaskTools::new(service.clone());
    let (catalog, _) = tools.catalog(&authority);
    assert!(catalog.iter().any(|tool| tool.name == "get_workspace"));
    assert!(catalog.iter().any(|tool| tool.name == "manage_workspace"));
    let state = tools
        .invoke(
            &authority,
            "get_workspace",
            json!({}),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(state["mainChatId"], main);
    assert_eq!(state["callerTaskId"], main);
    assert!(
        state["relatedTaskIds"]
            .as_array()
            .unwrap()
            .contains(&json!(child.task_id))
    );
    assert!(
        !state["relatedTaskIds"]
            .as_array()
            .unwrap()
            .contains(&json!(unrelated))
    );
    assert_eq!(state["layout"]["root"]["activeChatId"], main);
    assert!(
        state["layout"]["root"]["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tab| tab["chatId"] == child.task_id),
        "created helper gets a background tab"
    );
    for target in [&main, &unrelated] {
        let request = serde_json::from_value(
            json!({"action":"archiveThreads","taskIds":[target],"idempotencyKey":"unsafe-archive"}),
        )
        .unwrap();
        assert!(
            service
                .prepare_workspace(request, &authority)
                .await
                .is_err()
        );
    }
    let request = serde_json::from_value(json!({"action":"closeTabs","taskIds":[child.task_id],"idempotencyKey":"close-child","expectedLayoutRevision":state["layout"]["revision"]})).unwrap();
    let prepared = service
        .prepare_workspace(request, &authority)
        .await
        .unwrap();
    assert_eq!(prepared.main_chat_id, main);
    let listed = tools
        .invoke(
            &authority,
            "list_threads",
            json!({}),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let row = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["taskId"] == child.task_id)
        .unwrap();
    assert_eq!(row["agent"]["provider"], "codex");
    assert_eq!(row["agent"]["lineage"]["parentTaskId"], main);
    evidence(
        "agent-workspace-catalog.json",
        json!({"catalog":catalog,"workspace":state,"threads":listed}),
    );
    drop(rpc);
    drop(service);
    core.shutdown().await;
}

#[tokio::test]
async fn moving_an_existing_background_tab_into_a_split_preserves_focus_and_is_replayable() {
    let root = tempfile::tempdir().unwrap();
    let core = engine(root.path());
    let rpc = luna_rpc::memory_client(core.rpc_service());
    let (main, other) = seed(&core, &rpc, root.path()).await;
    let profile = rpc.call("EngineInfo", json!({})).await.unwrap()["profileId"].clone();
    let initial = rpc
        .call("GetChatWorkbench", json!({"mainChatId":main}))
        .await
        .unwrap();
    let opened = rpc.call("MutateChatWorkbench", json!({"mainChatId":main,"profileId":profile,"expectedRevision":initial["revision"],"commandId":"split-initial",
        "command":{"op":"openBackgroundTab","tab":{"chatId":other}}})).await.unwrap()["snapshot"].clone();
    let request = json!({"mainChatId":main,"profileId":profile,"expectedRevision":opened["revision"],"commandId":"split-existing",
        "command":{"op":"openBackgroundTab","tab":{"chatId":other},"destination":{"kind":"split","nodeId":opened["root"]["id"],"edge":"right","newPaneId":uuid::Uuid::new_v4(),"splitId":uuid::Uuid::new_v4()}}});
    let split = rpc
        .call("MutateChatWorkbench", request.clone())
        .await
        .unwrap();
    assert_eq!(split["snapshot"]["root"]["kind"], "split");
    assert_eq!(split["snapshot"]["focusedPaneId"], opened["focusedPaneId"]);
    assert_eq!(split["snapshot"]["root"]["first"]["activeChatId"], main);
    assert_eq!(split["snapshot"]["root"]["second"]["activeChatId"], other);
    assert_eq!(
        rpc.call("MutateChatWorkbench", request).await.unwrap()["snapshot"],
        split["snapshot"]
    );
    let closed = rpc.call("MutateChatWorkbench", json!({"mainChatId":main,"profileId":profile,"expectedRevision":split["snapshot"]["revision"],"commandId":"close-exact-set",
        "command":{"op":"closeTabs","chatIds":[other]}})).await.unwrap();
    assert!(!core.workspace.chat(&other).unwrap().unwrap().archived);
    evidence(
        "background-split.json",
        json!({"before":opened,"split":split,"closed":closed}),
    );
    drop(rpc);
    core.shutdown().await;
}
