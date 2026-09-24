//! End to end provider scope and retired history cleanup through the real
//! engine, workspace document, task command, and local snapshot store.

use std::sync::Arc;
use std::time::Duration;

use luna_engine::registry::HarnessDescriptor;
use luna_engine::{
    AgentAccounts, AgentAccountsConfig, CODEX_NATIVE_ACCOUNT_ID, EngineCore, EngineProfile,
    HarnessRegistry, default_registry,
};
use luna_proto::{ChatConfig, HarnessId, SteeringMode};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_three_providers_run_and_retired_luna_history_is_deleted() {
    let root = tempfile::tempdir().expect("isolated provider fixture");
    let registry = default_registry();
    let ids: Vec<_> = registry
        .descriptors()
        .into_iter()
        .filter(|descriptor| descriptor.id != HarnessId::Mock)
        .map(|descriptor| descriptor.id)
        .collect();
    assert_eq!(
        ids,
        vec![HarnessId::ClaudeCode, HarnessId::Codex, HarnessId::Opencode]
    );

    let toggles = HarnessRegistry::new();
    toggles.register_lazy(
        HarnessDescriptor {
            id: HarnessId::ClaudeCode,
            name: "Fixture Claude".into(),
            supports_steering: false,
            steering_mode: SteeringMode::TurnBoundary,
            reasoning_levels: vec![],
            installed: true,
            enabled: None,
        },
        Box::new(|| true),
        Box::new(|| Err(luna_harness::HarnessError::NotInstalled("fixture".into()))),
    );
    toggles.load_prefs(root.path());
    assert_eq!(toggles.enabled_set(), vec![HarnessId::ClaudeCode]);
    toggles.set_enabled(HarnessId::ClaudeCode, false).unwrap();
    assert!(toggles.enabled_set().is_empty());
    toggles.set_enabled(HarnessId::ClaudeCode, true).unwrap();
    assert_eq!(toggles.enabled_set(), vec![HarnessId::ClaudeCode]);

    let data = root.path().join("device");
    let providers = root.path().join("provider-homes");
    let accounts = AgentAccountsConfig {
        data_dir: data.clone(),
        claude_config_dir: providers.join("claude"),
        claude_config_file: providers.join("claude.json"),
        codex_home: providers.join("codex"),
    };
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&data, "synthetic-org", "synthetic-user"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts.clone(),
    )
    .expect("engine starts with isolated data");
    let retired: ChatConfig = serde_json::from_value(serde_json::json!({
        "harness": "cursor", "sandbox": "workspace-write"
    }))
    .expect("retired task config");
    let kept: ChatConfig = serde_json::from_value(serde_json::json!({
        "harness": "codex", "sandbox": "workspace-write"
    }))
    .expect("supported task config");
    core.workspace
        .create_chat(
            "retired-cursor-task",
            None,
            Some(&core.device_id),
            Some(retired),
            None,
        )
        .expect("seed retired task");
    core.workspace
        .create_chat(
            "kept-codex-task",
            None,
            Some(&core.device_id),
            Some(kept),
            None,
        )
        .expect("seed supported task");
    let opened = core
        .doc_host
        .open("retired-cursor-task")
        .expect("open retired task doc");
    core.doc_host.flush_all();
    drop(opened);
    assert!(core.store.has_snapshot("retired-cursor-task").unwrap());

    let _rpc = core.rpc_service(); // starts the real cleanup maintenance worker
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if core
                .workspace
                .chat("retired-cursor-task")
                .unwrap()
                .is_none()
                && !core.store.has_snapshot("retired-cursor-task").unwrap()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("retired LUNA task and snapshot were removed");
    assert!(core.workspace.chat("kept-codex-task").unwrap().is_some());

    core.agent_accounts
        .forget(HarnessId::Codex, CODEX_NATIVE_ACCOUNT_ID)
        .await
        .expect("disconnect Codex inside LUNA");
    assert!(
        core.agent_accounts
            .resolve_execution(HarnessId::Codex, None)
            .is_err()
    );
    let reopened_accounts = AgentAccounts::new(accounts);
    assert!(
        reopened_accounts
            .resolve_execution(HarnessId::Codex, None)
            .is_err()
    );
    reopened_accounts
        .activate(HarnessId::Codex, CODEX_NATIVE_ACCOUNT_ID)
        .await
        .expect("reconnect the native Codex account");
    assert!(
        reopened_accounts
            .resolve_execution(HarnessId::Codex, None)
            .is_ok()
    );

    let report = serde_json::json!({
        "runnableProviders": ["claude-code", "codex", "opencode"],
        "retiredTaskRemoved": true,
        "retiredSnapshotRemoved": true,
        "supportedTaskPreserved": true,
        "codexDisconnectPersisted": true,
        "lastProviderCanBeDisabledAndRestored": true,
        "providerNativeHomesUntouched": !providers.exists(),
    });
    if let Ok(path) = std::env::var("LUNA_PROVIDER_E2E_ARTIFACT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    println!("LUNA_PROVIDER_E2E={report}");
    core.shutdown().await;
}
