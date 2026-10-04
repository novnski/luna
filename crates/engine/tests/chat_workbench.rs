//! RPC/persistence E2E for the central chat workbench. Failure cases were
//! recorded before implementation in docs/design/chat-workbench/failure-cases.md.
//! These scenarios use real engine profiles, registry rows, watches, atomic
//! files, restarts and command receipts; no provider is started by layout work.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::mock::MockHarness;
use luna_proto::{ChatWorkbenchSnapshot, HarnessId};
use luna_rpc::RpcClient;
use serde_json::{Value, json};

fn assemble(root: &Path, user: &str) -> EngineCore {
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(MockHarness { script: Vec::new() }));
    let accounts = AgentAccountsConfig {
        data_dir: root.join("synthetic-accounts"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(root, "workbench-org", user),
        registry,
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap()
}

async fn seed(core: &EngineCore, client: &RpcClient, project: &Path, ids: &[String]) {
    std::fs::create_dir_all(project).unwrap();
    core.workspace
        .create_space(
            "workbench-space",
            &core.device_id,
            project.to_str().unwrap(),
            None,
            true,
        )
        .unwrap();
    for id in ids {
        client.call("Mutate", json!({"op":"createChat","chatId":id,"spaceId":"workbench-space","cwd":project,
            "config":{"harness":"mock","model":"mock-1","reasoning":"medium","permissionMode":"ask","effectivePermissionMode":"ask","sandbox":"workspace-write"}})).await.unwrap();
    }
}

async fn profile(client: &RpcClient) -> String {
    client.call("EngineInfo", json!({})).await.unwrap()["profileId"]
        .as_str()
        .unwrap()
        .into()
}

async fn get(client: &RpcClient, main: &str) -> ChatWorkbenchSnapshot {
    serde_json::from_value(
        client
            .call("GetChatWorkbench", json!({"mainChatId":main}))
            .await
            .unwrap(),
    )
    .unwrap()
}

fn request(main: &str, profile: &str, revision: u64, id: &str, command: Value) -> Value {
    json!({"mainChatId":main,"profileId":profile,"expectedRevision":revision,"commandId":id,"command":command})
}

async fn mutate(
    client: &RpcClient,
    state: &mut ChatWorkbenchSnapshot,
    profile: &str,
    id: &str,
    command: Value,
) -> Value {
    let result = client
        .call(
            "MutateChatWorkbench",
            request(&state.main_chat_id, profile, state.revision, id, command),
        )
        .await
        .unwrap();
    assert_eq!(result["conflict"], false, "{result}");
    *state = serde_json::from_value(result["snapshot"].clone()).unwrap();
    state.validate().unwrap();
    assert_eq!(
        state
            .root
            .tabs()
            .iter()
            .filter(|tab| tab.chat_id == state.main_chat_id)
            .count(),
        1
    );
    result
}

fn ids(count: usize) -> Vec<String> {
    (0..count)
        .map(|_| uuid::Uuid::new_v4().to_string())
        .collect()
}

fn evidence(name: &str, value: Value) {
    if let Ok(dir) = std::env::var("WORKBENCH_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Path::new(&dir).join(name),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

fn store_path(root: &Path, user: &str, main: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let hash: String = Sha256::digest(main.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    EngineProfile::development(root, "workbench-org", user)
        .store_root()
        .join("chat-workbenches-v1")
        .join(format!("{hash}.json"))
}

async fn stop(core: EngineCore, client: RpcClient) {
    drop(client);
    core.shutdown().await;
    drop(core);
}

/// The new-chat UI may select its stable ID before CreateChat has committed.
/// A later read/watch must succeed for that exact ID without weakening the
/// refusal for genuinely absent or deleted conversations.
#[tokio::test]
async fn optimistic_main_becomes_available_after_creation_without_recreating_deleted_chats() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), "optimistic-main");
    let client = luna_rpc::memory_client(core.rpc_service());
    let main = uuid::Uuid::new_v4().to_string();
    let before = client
        .call("GetChatWorkbench", json!({"mainChatId":main}))
        .await
        .unwrap_err()
        .to_string();
    assert!(before.contains("Main chat is unavailable"));
    assert!(core.workspace.chat(&main).unwrap().is_none());
    assert!(!store_path(dir.path(), "optimistic-main", &main).exists());
    seed(&core, &client, &dir.path().join("project"), &[main.clone()]).await;
    let ready = get(&client, &main).await;
    assert_eq!(ready.main_chat_id, main);
    assert!(ready.contains_chat(&main));
    let mut watch = client
        .subscribe("WatchChatWorkbench", json!({"mainChatId":main}))
        .await
        .unwrap();
    let published: ChatWorkbenchSnapshot = serde_json::from_value(
        tokio::time::timeout(Duration::from_secs(2), watch.recv())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(published, ready);
    drop(watch);
    client
        .call("Mutate", json!({"op":"deleteChat","chatId":main}))
        .await
        .unwrap();
    let deleted = client
        .call("GetChatWorkbench", json!({"mainChatId":main}))
        .await
        .unwrap_err()
        .to_string();
    assert!(deleted.contains("Main chat is unavailable"));
    assert!(core.workspace.chat(&main).unwrap().is_none());
    evidence(
        "optimistic-main-readiness.json",
        json!({
            "beforeCreation":before,"created":ready,"watch":published,
            "afterDeletion":deleted,"deletedChatRecreated":false
        }),
    );
    stop(core, client).await;
}

#[tokio::test]
async fn balance_panes_preserves_mixed_layouts_and_equalizes_leaf_area() {
    // Failure cases 28-29 were recorded before implementation. Exercise the
    // actual RPC, revision ledger and persisted tree; never invoke a reducer
    // directly or start a provider just to measure pane layout.
    fn without_ratios(state: &ChatWorkbenchSnapshot) -> Value {
        fn strip(value: &mut Value) {
            if let Value::Object(object) = value {
                object.remove("ratio");
                for child in object.values_mut() {
                    strip(child);
                }
            }
        }
        let mut value = serde_json::to_value(state).unwrap();
        value["revision"] = json!(0);
        strip(&mut value);
        value
    }
    fn areas(node: &luna_proto::ChatWorkbenchNode, area: f64, output: &mut Vec<f64>) {
        match node {
            luna_proto::ChatWorkbenchNode::Pane { .. } => output.push(area),
            luna_proto::ChatWorkbenchNode::Split {
                ratio,
                first,
                second,
                ..
            } => {
                areas(first, area * ratio, output);
                areas(second, area * (1.0 - ratio), output);
            }
        }
    }
    fn equal_areas(state: &ChatWorkbenchSnapshot, count: usize) -> Vec<f64> {
        let mut result = Vec::new();
        areas(&state.root, 1.0, &mut result);
        assert_eq!(result.len(), count);
        for area in &result {
            assert!((area - 1.0 / count as f64).abs() < 1e-12, "{result:?}");
        }
        result
    }
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), "balance");
    let client = luna_rpc::memory_client(core.rpc_service());
    let ids = ids(8);
    let main = &ids[0];
    seed(&core, &client, &dir.path().join("project"), &ids).await;
    let profile = profile(&client).await;
    let chats_before = serde_json::to_value(core.workspace.read_chats().unwrap()).unwrap();
    let mut state = get(&client, main).await;
    let one = state.clone();
    let single = mutate(
        &client,
        &mut state,
        &profile,
        "balance-single",
        json!({"op":"balancePanes"}),
    )
    .await;
    assert_eq!(single["applied"], false);
    assert_eq!(state, one);
    // Two tabs in Main's pane still represent one cell; closed history also
    // remains untouched by the later balancing operation.
    mutate(
        &client,
        &mut state,
        &profile,
        "extra-tab",
        json!({"op":"openTab","tab":{"chatId":ids[6]}}),
    )
    .await;
    mutate(
        &client,
        &mut state,
        &profile,
        "temporary-tab",
        json!({"op":"openTab","tab":{"chatId":ids[7]}}),
    )
    .await;
    mutate(
        &client,
        &mut state,
        &profile,
        "close-temporary",
        json!({"op":"closeTab","chatId":ids[7]}),
    )
    .await;
    mutate(&client, &mut state, &profile, "stack-left", json!({"op":"openTab","tab":{"chatId":ids[1]},"destination":{"kind":"split","nodeId":"main-pane","edge":"bottom","newPaneId":"pane-a","splitId":"left-stack"}})).await;
    mutate(&client, &mut state, &profile, "single-right", json!({"op":"openTab","tab":{"chatId":ids[2]},"destination":{"kind":"split","nodeId":"left-stack","edge":"right","newPaneId":"pane-b","splitId":"outer"}})).await;
    mutate(
        &client,
        &mut state,
        &profile,
        "skew-outer",
        json!({"op":"resizeSplit","splitId":"outer","ratio":0.42}),
    )
    .await;
    mutate(
        &client,
        &mut state,
        &profile,
        "skew-stack",
        json!({"op":"resizeSplit","splitId":"left-stack","ratio":0.72}),
    )
    .await;
    let before_three = state.clone();
    let command = request(
        main,
        &profile,
        state.revision,
        "balance-three",
        json!({"op":"balancePanes"}),
    );
    let balanced = mutate(
        &client,
        &mut state,
        &profile,
        "balance-three",
        json!({"op":"balancePanes"}),
    )
    .await;
    assert_eq!(balanced["applied"], true);
    assert_eq!(state.revision, before_three.revision + 1);
    assert_eq!(without_ratios(&state), without_ratios(&before_three));
    let three_areas = equal_areas(&state, 3);
    let three = state.clone();
    assert_eq!(
        serde_json::to_value(&state.root).unwrap()["ratio"],
        json!(2.0 / 3.0)
    );
    let replay = client.call("MutateChatWorkbench", command).await.unwrap();
    assert_eq!(replay["applied"], false);
    assert_eq!(replay["snapshot"], balanced["snapshot"]);
    let again = mutate(
        &client,
        &mut state,
        &profile,
        "balance-three-again",
        json!({"op":"balancePanes"}),
    )
    .await;
    assert_eq!(again["applied"], false);
    assert_eq!(state, three);
    let conflict = client
        .call(
            "MutateChatWorkbench",
            request(
                main,
                &profile,
                before_three.revision,
                "stale-balance",
                json!({"op":"balancePanes"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(conflict["conflict"], true);
    assert_eq!(get(&client, main).await, three);
    for (n, node, edge, pane, split) in [
        (3, "main-pane", "right", "pane-c", "upper-left"),
        (4, "pane-a", "top", "pane-d", "lower-left"),
        (5, "pane-b", "bottom", "pane-e", "right-stack"),
    ] {
        mutate(&client, &mut state, &profile, split, json!({"op":"openTab","tab":{"chatId":ids[n]},"destination":{"kind":"split","nodeId":node,"edge":edge,"newPaneId":pane,"splitId":split}})).await;
    }
    mutate(
        &client,
        &mut state,
        &profile,
        "skew-six",
        json!({"op":"resizeSplit","splitId":"outer","ratio":0.37}),
    )
    .await;
    let before_six = state.clone();
    mutate(
        &client,
        &mut state,
        &profile,
        "balance-six",
        json!({"op":"balancePanes"}),
    )
    .await;
    assert_eq!(without_ratios(&state), without_ratios(&before_six));
    assert_eq!(state.revision, before_six.revision + 1);
    let six_areas = equal_areas(&state, 6);
    let six = state.clone();
    assert!(core.sessions.watch_sessions().borrow().is_empty());
    stop(core, client).await;
    let core = assemble(dir.path(), "balance");
    let client = luna_rpc::memory_client(core.rpc_service());
    assert_eq!(get(&client, main).await, six);
    // Empty cells have the same weight. The sixteen-cell extreme must obey
    // the existing ratio guard even when exact equal area needs 1/16.
    for n in 6..luna_proto::WORKBENCH_MAX_PANES {
        let root = state.root.id().to_string();
        mutate(&client, &mut state, &profile, &format!("empty-{n}"), json!({"op":"splitPane","nodeId":root,"edge":"left","newPaneId":format!("empty-pane-{n}"),"splitId":format!("empty-split-{n}")})).await;
    }
    let before_extreme = state.clone();
    mutate(
        &client,
        &mut state,
        &profile,
        "balance-sixteen",
        json!({"op":"balancePanes"}),
    )
    .await;
    assert_eq!(without_ratios(&state), without_ratios(&before_extreme));
    assert_eq!(
        serde_json::to_value(&state.root).unwrap()["ratio"],
        json!(0.1)
    );
    state.validate().unwrap();
    let mut extreme_areas = Vec::new();
    areas(&state.root, 1.0, &mut extreme_areas);
    assert_eq!(extreme_areas.len(), 16);
    assert!(core.sessions.watch_sessions().borrow().is_empty());
    assert_eq!(
        serde_json::to_value(core.workspace.read_chats().unwrap()).unwrap(),
        chats_before
    );
    evidence(
        "balanced-panes.json",
        json!({"three":three,"threeAreas":three_areas,"six":six,"sixAreas":six_areas,"sixRestoredAfterRestart":true,"boundedSixteen":state,"providerStarts":0,"chatRecordsUnchanged":true,"replay":replay,"conflict":conflict}),
    );
    stop(core, client).await;
}

#[tokio::test]
async fn empty_pane_focus_survives_restart_and_guides_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), "empty-focus");
    let client = luna_rpc::memory_client(core.rpc_service());
    let ids = ids(2);
    let main = &ids[0];
    let other = &ids[1];
    seed(&core, &client, &dir.path().join("project"), &ids).await;
    let profile = profile(&client).await;
    let before_chats = serde_json::to_value(core.workspace.read_chats().unwrap()).unwrap();
    let mut state = get(&client, main).await;
    mutate(&client, &mut state, &profile, "open-original", json!({"op":"openTab","tab":{"chatId":other},"destination":{"kind":"split","nodeId":"main-pane","edge":"right","newPaneId":"original-pane","splitId":"original-split"}})).await;
    mutate(&client, &mut state, &profile, "add-empty", json!({"op":"splitPane","nodeId":"main-pane","edge":"bottom","newPaneId":"empty-pane","splitId":"empty-split"})).await;
    mutate(
        &client,
        &mut state,
        &profile,
        "close-original",
        json!({"op":"closeTab","chatId":other}),
    )
    .await;
    assert!(state.pane("original-pane").is_none());
    let root_before_focus = state.root.clone();
    let closed_before_focus = state.closed_tabs.clone();
    mutate(
        &client,
        &mut state,
        &profile,
        "focus-main",
        json!({"op":"focusPane","paneId":"main-pane"}),
    )
    .await;
    assert_eq!(state.focused_chat_id(), Some(main.as_str()));
    mutate(
        &client,
        &mut state,
        &profile,
        "focus-empty",
        json!({"op":"focusPane","paneId":"empty-pane"}),
    )
    .await;
    assert_eq!(state.focused_pane_id, "empty-pane");
    assert_eq!(state.focused_chat_id(), None);
    assert_eq!(state.root, root_before_focus);
    assert_eq!(state.closed_tabs, closed_before_focus);
    let revision = state.revision;
    let repeated = mutate(
        &client,
        &mut state,
        &profile,
        "focus-empty-again",
        json!({"op":"focusPane","paneId":"empty-pane"}),
    )
    .await;
    assert_eq!(repeated["applied"], false);
    assert_eq!(state.revision, revision);
    for pane in ["missing-pane", "empty-split", "../foreign"] {
        assert!(
            client
                .call(
                    "MutateChatWorkbench",
                    request(
                        main,
                        &profile,
                        state.revision,
                        &format!("invalid-focus-{}", pane.len()),
                        json!({"op":"focusPane","paneId":pane})
                    )
                )
                .await
                .is_err()
        );
        assert_eq!(get(&client, main).await, state);
    }
    for id in &ids {
        assert!(core.sessions.session_status(id).is_none());
    }
    stop(core, client).await;
    let core = assemble(dir.path(), "empty-focus");
    let client = luna_rpc::memory_client(core.rpc_service());
    assert_eq!(get(&client, main).await, state);
    let focused_empty = state.clone();
    mutate(
        &client,
        &mut state,
        &profile,
        "reopen-in-empty",
        json!({"op":"reopenClosed","chatId":other}),
    )
    .await;
    assert_eq!(state.focused_pane_id, "empty-pane");
    assert_eq!(state.focused_chat_id(), Some(other.as_str()));
    assert_eq!(state.pane_for_chat(other).unwrap().id(), "empty-pane");
    assert_eq!(
        serde_json::to_value(core.workspace.read_chats().unwrap()).unwrap(),
        before_chats
    );
    for id in &ids {
        assert!(core.sessions.session_status(id).is_none());
    }
    evidence(
        "empty-pane-focus.json",
        json!({"focusedEmptyBeforeRestart":focused_empty,"reopenedAfterRestart":state,"chatRecordsUnchanged":true,"providerStarts":0}),
    );
    stop(core, client).await;
}

#[tokio::test]
async fn nested_tabs_moves_reopen_watches_and_restart_use_the_same_chats() {
    let dir = tempfile::tempdir().unwrap();
    let core = assemble(dir.path(), "layout");
    let client = luna_rpc::memory_client(core.rpc_service());
    let ids = ids(4);
    let main = &ids[0];
    let a = &ids[1];
    let b = &ids[2];
    let c = &ids[3];
    seed(&core, &client, &dir.path().join("project"), &ids).await;
    let profile = profile(&client).await;
    let mut state = get(&client, main).await;
    let before_chats = serde_json::to_value(core.workspace.read_chats().unwrap()).unwrap();
    let mut watch = client
        .subscribe("WatchChatWorkbench", json!({"mainChatId":main}))
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), watch.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first["revision"], 0);
    for (n, id) in [a, b, c].into_iter().enumerate() {
        mutate(
            &client,
            &mut state,
            &profile,
            &format!("open-{n}"),
            json!({"op":"openTab","tab":{"chatId":id}}),
        )
        .await;
    }
    let update = tokio::time::timeout(Duration::from_secs(2), watch.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(update["revision"].as_u64().unwrap() > 0);
    mutate(&client,&mut state,&profile,"a-right",json!({"op":"moveTab","chatId":a,"destination":{"kind":"split","nodeId":"main-pane","edge":"right","newPaneId":"right","splitId":"horizontal"}})).await;
    mutate(&client,&mut state,&profile,"b-bottom",json!({"op":"moveTab","chatId":b,"destination":{"kind":"split","nodeId":"right","edge":"bottom","newPaneId":"bottom-right","splitId":"vertical"}})).await;
    mutate(
        &client,
        &mut state,
        &profile,
        "resize-h",
        json!({"op":"resizeSplit","splitId":"horizontal","ratio":0.62}),
    )
    .await;
    mutate(
        &client,
        &mut state,
        &profile,
        "resize-v",
        json!({"op":"resizeSplit","splitId":"vertical","ratio":0.35}),
    )
    .await;
    let nested = state.clone();
    assert_eq!(state.pane_for_chat(a).unwrap().id(), "right");
    assert_eq!(state.pane_for_chat(b).unwrap().id(), "bottom-right");
    // Open means focus: specifying another destination never duplicates an existing chat.
    mutate(&client,&mut state,&profile,"focus-existing-a",json!({"op":"openTab","tab":{"chatId":a},"destination":{"kind":"pane","paneId":"main-pane","index":0}})).await;
    assert_eq!(state.focused_pane_id, "right");
    assert_eq!(state.root.tabs().len(), 4);
    mutate(
        &client,
        &mut state,
        &profile,
        "close-a",
        json!({"op":"closeTab","chatId":a}),
    )
    .await;
    assert!(state.pane("right").is_none());
    assert!(state.root.node("vertical").is_none());
    mutate(
        &client,
        &mut state,
        &profile,
        "reopen-a",
        json!({"op":"reopenClosed"}),
    )
    .await;
    assert!(state.contains_chat(a));
    mutate(&client,&mut state,&profile,"b-join-main",json!({"op":"moveTab","chatId":b,"destination":{"kind":"pane","paneId":"main-pane","index":1}})).await;
    mutate(&client,&mut state,&profile,"main-bottom",json!({"op":"moveTab","chatId":main,"destination":{"kind":"split","nodeId":"main-pane","edge":"bottom","newPaneId":"protected-main","splitId":"main-bottom"}})).await;
    assert_eq!(state.pane_for_chat(main).unwrap().id(), "protected-main");
    for (n, command) in [
        json!({"op":"closeTab","chatId":main}),
        json!({"op":"closePane","paneId":"protected-main"}),
    ]
    .into_iter()
    .enumerate()
    {
        let result = client
            .call(
                "MutateChatWorkbench",
                request(
                    main,
                    &profile,
                    state.revision,
                    &format!("reject-main-{n}"),
                    command,
                ),
            )
            .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("protectedMainChat")
        );
        assert_eq!(get(&client, main).await, state);
    }
    mutate(&client,&mut state,&profile,"empty-left",json!({"op":"splitPane","nodeId":"protected-main","edge":"left","newPaneId":"empty","splitId":"empty-split"})).await;
    assert_eq!(state.focused_chat_id(), None);
    mutate(
        &client,
        &mut state,
        &profile,
        "close-empty",
        json!({"op":"closePane","paneId":"empty"}),
    )
    .await;
    assert!(state.pane("empty").is_none());
    // Metadata/lifecycle proof: layout work has not edited any chat or started a provider.
    assert_eq!(
        serde_json::to_value(core.workspace.read_chats().unwrap()).unwrap(),
        before_chats
    );
    assert!(core.sessions.watch_sessions().borrow().is_empty());
    let persisted = state.clone();
    drop(watch);
    stop(core, client).await;
    let core = assemble(dir.path(), "layout");
    let client = luna_rpc::memory_client(core.rpc_service());
    let restored = get(&client, main).await;
    assert_eq!(restored, persisted);
    assert!(core.sessions.watch_sessions().borrow().is_empty());
    evidence(
        "layout-restart.json",
        json!({"nested":nested,"beforeRestart":persisted,"afterRestart":restored,"chatCount":4,"providerRuns":0}),
    );
    stop(core, client).await;
}

#[tokio::test]
async fn command_receipts_revision_conflicts_and_account_profiles_are_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let ids = ids(2);
    let main = &ids[0];
    let other = &ids[1];
    let core = assemble(dir.path(), "account-a");
    let client = luna_rpc::memory_client(core.rpc_service());
    seed(&core, &client, &dir.path().join("project"), &ids).await;
    let profile_a = profile(&client).await;
    let initial = get(&client, main).await;
    let command = request(
        main,
        &profile_a,
        0,
        "stable-open",
        json!({"op":"openTab","tab":{"chatId":other}}),
    );
    let accepted = client
        .call("MutateChatWorkbench", command.clone())
        .await
        .unwrap();
    let replay = client
        .call("MutateChatWorkbench", command.clone())
        .await
        .unwrap();
    assert_eq!(accepted["snapshot"], replay["snapshot"]);
    assert_eq!(replay["applied"], false);
    let conflict = client
        .call(
            "MutateChatWorkbench",
            request(
                main,
                &profile_a,
                initial.revision,
                "stale-close",
                json!({"op":"closeTab","chatId":other}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(conflict["conflict"], true);
    assert_eq!(conflict["snapshot"], accepted["snapshot"]);
    let reused = client
        .call(
            "MutateChatWorkbench",
            request(
                main,
                &profile_a,
                0,
                "stable-open",
                json!({"op":"focusTab","chatId":main}),
            ),
        )
        .await
        .unwrap_err();
    assert!(reused.to_string().contains("already used"));
    let stale_client = luna_rpc::memory_client(core.rpc_service());
    stop(core, client).await;
    let retired = stale_client
        .call(
            "MutateChatWorkbench",
            request(
                main,
                &profile_a,
                accepted["snapshot"]["revision"].as_u64().unwrap(),
                "retired-writer",
                json!({"op":"closeTab","chatId":other}),
            ),
        )
        .await
        .unwrap_err();
    assert!(retired.to_string().contains("storage is unavailable"));
    drop(stale_client);
    let core = assemble(dir.path(), "account-b");
    let client = luna_rpc::memory_client(core.rpc_service());
    seed(&core, &client, &dir.path().join("other-project"), &ids).await;
    let profile_b = profile(&client).await;
    assert_ne!(profile_a, profile_b);
    let b = get(&client, main).await;
    assert_eq!(b.revision, 0);
    assert_eq!(b.root.tabs().len(), 1);
    assert!(
        client
            .call("MutateChatWorkbench", command.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("profile changed")
    );
    assert_eq!(get(&client, main).await, b);
    stop(core, client).await;
    let core = assemble(dir.path(), "account-a");
    let client = luna_rpc::memory_client(core.rpc_service());
    let replay_after_restart = client.call("MutateChatWorkbench", command).await.unwrap();
    assert_eq!(replay_after_restart["snapshot"], accepted["snapshot"]);
    evidence(
        "layout-command-fences.json",
        json!({"accepted":accepted,"replay":replay,"conflict":conflict,"profileA":profile_a,"profileB":profile_b,"otherAccount":b,"replayAfterRestart":replay_after_restart}),
    );
    stop(core, client).await;
}

#[tokio::test]
async fn malformed_targets_limits_and_missing_chats_do_not_corrupt_layouts() {
    let dir = tempfile::tempdir().unwrap();
    let ids = ids(4);
    let main = &ids[0];
    let a = &ids[1];
    let b = &ids[2];
    let core = assemble(dir.path(), "invalid");
    let client = luna_rpc::memory_client(core.rpc_service());
    seed(&core, &client, &dir.path().join("project"), &ids).await;
    let profile = profile(&client).await;
    let mut state = get(&client, main).await;
    mutate(
        &client,
        &mut state,
        &profile,
        "open-a",
        json!({"op":"openTab","tab":{"chatId":a}}),
    )
    .await;
    let mut errors = Vec::new();
    for (n,command) in [
        json!({"op":"closeTab","chatId":main}),
        json!({"op":"openTab","tab":{"chatId":"unknown"}}),
        json!({"op":"openTab","tab":{"chatId":b,"sideParentId":main}}),
        json!({"op":"moveTab","chatId":a,"destination":{"kind":"pane","paneId":"gone"}}),
        json!({"op":"moveTab","chatId":a,"destination":{"kind":"pane","paneId":"main-pane","index":999}}),
        json!({"op":"splitPane","nodeId":"main-pane","edge":"right","newPaneId":"main-pane","splitId":"s"}),
        json!({"op":"splitPane","nodeId":"main-pane","edge":"right","newPaneId":"x","splitId":"x"}),
        json!({"op":"splitPane","nodeId":"gone","edge":"left","newPaneId":"x","splitId":"s"}),
        json!({"op":"resizeSplit","splitId":"main-pane","ratio":0.5}),
        json!({"op":"reopenClosed","chatId":"absent"}),
    ].into_iter().enumerate() {
        errors.push(client.call("MutateChatWorkbench",request(main,&profile,state.revision,&format!("invalid-{n}"),command)).await.unwrap_err().to_string());
        assert_eq!(get(&client,main).await,state);
    }
    mutate(
        &client,
        &mut state,
        &profile,
        "open-b",
        json!({"op":"openTab","tab":{"chatId":b}}),
    )
    .await;
    mutate(
        &client,
        &mut state,
        &profile,
        "close-others",
        json!({"op":"closeOtherTabs","paneId":"main-pane","keepChatId":a}),
    )
    .await;
    assert!(state.contains_chat(main));
    assert!(state.contains_chat(a));
    assert!(!state.contains_chat(b));
    // A real deletion updates the watch before the next user mutation. Otherwise
    // pruning during OpenTab creates a stale revision and loses that action.
    let mut changes = client
        .subscribe("WatchChatWorkbench", json!({"mainChatId":main}))
        .await
        .unwrap();
    let initial: ChatWorkbenchSnapshot =
        serde_json::from_value(changes.recv().await.unwrap()).unwrap();
    assert!(initial.contains_chat(a));
    client
        .call("Mutate", json!({"op":"deleteChat","chatId":a}))
        .await
        .unwrap();
    let reconciled = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let update: ChatWorkbenchSnapshot =
                serde_json::from_value(changes.recv().await.expect("layout watch stays open"))
                    .unwrap();
            if !update.contains_chat(a) {
                break update;
            }
        }
    })
    .await
    .expect("deletion must publish a pruned layout without a Get or mutation");
    drop(changes);
    assert!(!reconciled.contains_chat(a));
    assert!(
        !reconciled
            .closed_tabs
            .iter()
            .any(|tab| tab.tab.chat_id == *a)
    );
    assert!(core.workspace.chat(a).unwrap().is_none());
    state = reconciled;
    for n in 1..luna_proto::WORKBENCH_MAX_PANES {
        mutate(&client,&mut state,&profile,&format!("split-{n}"),json!({"op":"splitPane","nodeId":"main-pane","edge":"right","newPaneId":format!("pane-{n}"),"splitId":format!("split-{n}")})).await;
    }
    let overflow=client.call("MutateChatWorkbench",request(main,&profile,state.revision,"overflow",json!({"op":"splitPane","nodeId":"main-pane","edge":"right","newPaneId":"one-too-many","splitId":"overflow-split"}))).await.unwrap_err();
    assert!(overflow.to_string().contains("Too many split panes"));
    assert_eq!(get(&client, main).await, state);
    for (n, ratio) in [json!(0.0), json!(0.99), Value::Null]
        .into_iter()
        .enumerate()
    {
        assert!(
            client
                .call(
                    "MutateChatWorkbench",
                    request(
                        main,
                        &profile,
                        state.revision,
                        &format!("bad-ratio-{n}"),
                        json!({"op":"resizeSplit","splitId":"split-1","ratio":ratio})
                    )
                )
                .await
                .is_err()
        );
        assert_eq!(get(&client, main).await, state);
    }
    evidence(
        "layout-invalid-and-deletion.json",
        json!({"errors":errors,"final":state,"deletedTabAbsent":true,"paneLimitEnforced":true}),
    );
    stop(core, client).await;
}

#[cfg(unix)]
#[tokio::test]
async fn failed_storage_and_corrupt_files_preserve_last_good_state() {
    let dir = tempfile::tempdir().unwrap();
    let ids = ids(2);
    let main = &ids[0];
    let other = &ids[1];
    let core = assemble(dir.path(), "storage");
    let client = luna_rpc::memory_client(core.rpc_service());
    seed(&core, &client, &dir.path().join("project"), &ids).await;
    let profile = profile(&client).await;
    let mut state = get(&client, main).await;
    mutate(
        &client,
        &mut state,
        &profile,
        "save",
        json!({"op":"openTab","tab":{"chatId":other}}),
    )
    .await;
    let path = store_path(dir.path(), "storage", main);
    let original = std::fs::read(&path).unwrap();
    let saved = path.with_extension("saved");
    let sentinel = dir.path().join("sentinel");
    std::fs::write(&sentinel, b"unchanged").unwrap();
    std::fs::rename(&path, &saved).unwrap();
    std::os::unix::fs::symlink(&sentinel, &path).unwrap();
    let failed = client
        .call(
            "MutateChatWorkbench",
            request(
                main,
                &profile,
                state.revision,
                "failed-close",
                json!({"op":"closeTab","chatId":other}),
            ),
        )
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("regular file"));
    assert_eq!(get(&client, main).await, state);
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&saved, &path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    stop(core, client).await;
    let invalid = b"{ broken layout";
    std::fs::write(&path, invalid).unwrap();
    let core = assemble(dir.path(), "storage");
    let client = luna_rpc::memory_client(core.rpc_service());
    let failure = client
        .call("GetChatWorkbench", json!({"mainChatId":main}))
        .await
        .unwrap_err();
    assert!(failure.to_string().contains("preserved"));
    assert_eq!(std::fs::read(&path).unwrap(), invalid);
    evidence(
        "layout-storage-failures.json",
        json!({"lastGood":state,"writeFailure":failed.to_string(),"readFailure":failure.to_string(),"sentinelUnchanged":true,"corruptFilePreserved":true}),
    );
    stop(core, client).await;
}
