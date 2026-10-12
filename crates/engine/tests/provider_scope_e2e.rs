//! E06: lossless provider history and host discovery through real engine
//! documents, restart, RPC subscriptions, account scope and the guarded cache.

use luna_engine::registry::ProviderCatalog;
use luna_harness::{Harness, HarnessError, RunControls};
use luna_rpc::methods;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

use luna_engine::registry::HarnessDescriptor;
use luna_engine::{
    AgentAccounts, AgentAccountsConfig, CODEX_NATIVE_ACCOUNT_ID, EngineCore, EngineProfile,
    HarnessRegistry,
};
use luna_proto::{ChatConfig, HarnessId, SteeringMode};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_and_retired_provider_history_survives_restart() {
    let root = tempfile::tempdir().expect("isolated provider fixture");
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
    let missing: ChatConfig = serde_json::from_value(serde_json::json!({
        "harness": "acme.provider/agent", "sandbox": "workspace-write",
        "providerAccountId": "original-account", "permissionMode": "ask", "model": "original-model"
    }))
    .expect("an absent namespaced provider must not break history decoding");
    core.workspace
        .create_chat(
            "missing-provider-task",
            None,
            Some(&core.device_id),
            Some(missing),
            None,
        )
        .unwrap();
    core.workspace.set_chat_harness_session(
        "missing-provider-task",
        "original-native-session",
        root.path().to_str().unwrap(),
    );
    let original_session = core
        .workspace
        .chat_harness_session("missing-provider-task")
        .unwrap();
    let missing_doc = core.doc_host.open("missing-provider-task").unwrap();
    missing_doc
        .write_user_message("original-message", "Keep this transcript exactly.", 100)
        .unwrap();
    let transcript = serde_json::to_vec(&missing_doc.doc().read_entries().unwrap()).unwrap();
    drop(missing_doc);
    let original_config = serde_json::to_value(
        core.workspace
            .chat("missing-provider-task")
            .unwrap()
            .unwrap()
            .config,
    )
    .unwrap();
    let retired: ChatConfig = serde_json::from_value(serde_json::json!({
        "harness": "cursor", "sandbox": "workspace-write"
    }))
    .expect("retired task config");
    let aliased: ChatConfig = serde_json::from_value(
        serde_json::json!({"harness":"luna.provider/codex", "sandbox":"workspace-write"}),
    )
    .unwrap();
    core.workspace
        .create_chat(
            "aliased-codex-task",
            None,
            Some(&core.device_id),
            Some(aliased),
            None,
        )
        .unwrap();
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

    let client = Arc::new(luna_rpc::memory_client(core.rpc_service()));
    let unavailable = client
        .call(
            luna_rpc::methods::GET_PROVIDER_MODELS,
            serde_json::json!({"harness": "acme.provider/agent"}),
        )
        .await
        .unwrap_err();
    assert!(
        unavailable.to_string().contains("binary not found"),
        "{unavailable}"
    );
    // Give actual RPC/maintenance work its event-loop turn, then drain and reopen
    // the entire engine. No timer stands in for a persistence event.
    core.shutdown().await;
    drop(client);
    drop(core);
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&data, "synthetic-org", "synthetic-user"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts.clone(),
    )
    .unwrap();
    for id in [
        "retired-cursor-task",
        "kept-codex-task",
        "missing-provider-task",
    ] {
        assert!(
            core.workspace.chat(id).unwrap().is_some(),
            "lost {id} on restart"
        );
    }
    assert!(core.store.has_snapshot("retired-cursor-task").unwrap());
    let alias = core
        .workspace
        .chat("aliased-codex-task")
        .unwrap()
        .unwrap()
        .config
        .unwrap();
    assert_eq!(alias.harness, HarnessId::Codex);
    assert_eq!(
        serde_json::to_value(&alias).unwrap()["harness"],
        "luna.provider/codex"
    );
    let config = serde_json::to_value(
        core.workspace
            .chat("missing-provider-task")
            .unwrap()
            .unwrap()
            .config,
    )
    .unwrap();
    assert_eq!(
        config, original_config,
        "provider, account, model and permission bindings changed"
    );
    assert_eq!(
        core.workspace
            .chat_harness_session("missing-provider-task")
            .unwrap(),
        original_session
    );
    let reopened_doc = core.doc_host.open("missing-provider-task").unwrap();
    assert_eq!(
        serde_json::to_vec(&reopened_doc.doc().read_entries().unwrap()).unwrap(),
        transcript
    );
    drop(reopened_doc);
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

    use sha2::{Digest, Sha256};
    let transcript_hash = format!("{:x}", Sha256::digest(&transcript));
    let config_hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&original_config).unwrap())
    );
    let report = serde_json::json!({
        "unknownProvider": "acme.provider/agent",
        "transcriptSha256": transcript_hash,
        "configSha256": config_hash,
        "nativeSessionPreserved": true,
        "retiredTaskPreserved": true,
        "retiredSnapshotPreserved": true,
        "unknownBindingsAndTranscriptPreservedThroughRestart": true,
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

// A discovery-only in-process adapter. It deliberately cannot execute a task:
// these scenarios own registry/cache publication, not native protocol delivery.
struct CatalogAdapter {
    id: HarnessId,
    source: Arc<std::sync::Mutex<String>>,
    gate: Option<(Arc<Semaphore>, Arc<Semaphore>)>,
}
impl CatalogAdapter {
    fn new(id: &str) -> Self {
        Self {
            id: HarnessId::parse(id).unwrap(),
            source: Arc::new(std::sync::Mutex::new("installation-a".into())),
            gate: None,
        }
    }
}
#[async_trait::async_trait]
impl Harness for CatalogAdapter {
    fn id(&self) -> HarnessId {
        self.id
    }
    fn display_name(&self) -> &str {
        "Discovery adapter"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[luna_proto::ReasoningLevel] {
        &[]
    }
    fn model_catalog_source(&self) -> String {
        self.source.lock().unwrap().clone()
    }
    async fn models(&self) -> Result<Vec<luna_proto::Model>, HarnessError> {
        if let Some((entered, release)) = &self.gate {
            entered.add_permits(1);
            release.acquire().await.unwrap().forget();
        }
        Ok(vec![
            serde_json::from_value(
                serde_json::json!({"id":"discovered-model", "label":"Discovered model"}),
            )
            .unwrap(),
        ])
    }
    async fn run(
        &self,
        _: luna_proto::RunRequest,
        _: RunControls,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<luna_proto::AgentEvent, HarnessError>>,
        HarnessError,
    > {
        Err(HarnessError::Protocol("discovery-only adapter".into()))
    }
}
async fn next_catalog(
    events: &mut luna_rpc::RpcSubscription,
    predicate: impl Fn(&ProviderCatalog) -> bool,
) -> ProviderCatalog {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let catalog: ProviderCatalog =
                serde_json::from_value(events.recv().await.expect("catalog watch closed")).unwrap();
            if predicate(&catalog) {
                break catalog;
            }
        }
    })
    .await
    .expect("authoritative registry change was not delivered")
}

#[tokio::test]
async fn registry_changes_are_live_collision_safe_and_preserve_missing_preferences() {
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(HarnessRegistry::new());
    registry.register(Arc::new(CatalogAdapter::new("codex")));
    let core = EngineCore::assemble(root.path(), registry.clone(), HarnessId::Mock, None).unwrap();
    let client = Arc::new(luna_rpc::memory_client(core.rpc_service()));
    let mut events = client
        .subscribe_checked(methods::WATCH_PROVIDER_CATALOG, serde_json::json!({}))
        .await
        .unwrap();
    let first = next_catalog(&mut events, |_| true).await;
    assert!(
        registry
            .try_register(Arc::new(CatalogAdapter::new("luna.provider/codex")))
            .unwrap_err()
            .to_string()
            .contains("already registered")
    );
    for id in ["acme.provider/agent", "another.provider/agent"] {
        registry
            .try_register(Arc::new(CatalogAdapter::new(id)))
            .unwrap();
    }
    let added = next_catalog(&mut events, |c| c.providers.len() == 3).await;
    assert!(added.generation > first.generation);
    assert_eq!(added.host_id, core.device_id);
    let models: luna_proto::ProviderModels = serde_json::from_value(
        client
            .call(
                methods::GET_PROVIDER_MODELS,
                serde_json::json!({"harness":"acme.provider/agent"}),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(models.provenance.registry_id, added.registry_id);
    assert_eq!(models.provenance.host_id, core.device_id);
    assert_eq!(models.provenance.executable, "installation-a");
    assert_eq!(models.models[0].id, "discovered-model");
    client
        .call(
            methods::SET_HARNESS_ENABLED,
            serde_json::json!({"harness":"acme.provider/agent", "enabled":false}),
        )
        .await
        .unwrap();
    next_catalog(&mut events, |c| {
        c.providers
            .iter()
            .any(|p| p.descriptor.id.as_str() == "acme.provider/agent" && !p.available)
    })
    .await;
    assert!(registry.unregister(HarnessId::parse("acme.provider/agent").unwrap()));
    let removed = next_catalog(&mut events, |c| c.providers.len() == 2).await;
    assert!(
        client
            .call(
                methods::GET_PROVIDER_MODELS,
                serde_json::json!({"harness":"acme.provider/agent"})
            )
            .await
            .is_err()
    );
    // A removed provider's opt-out survives restart and reinstallation.
    let prefs_before = std::fs::read(root.path().join("harness-prefs.json")).unwrap();
    drop(events);
    drop(client);
    core.shutdown().await;
    drop(core);
    let reloaded = Arc::new(HarnessRegistry::new());
    let core = EngineCore::assemble(root.path(), reloaded.clone(), HarnessId::Mock, None).unwrap();
    reloaded
        .try_register(Arc::new(CatalogAdapter::new("acme.provider/agent")))
        .unwrap();
    let catalog = reloaded.catalog(&core.device_id);
    assert_ne!(catalog.registry_id, removed.registry_id);
    assert!(!catalog.providers[0].available);
    assert_eq!(
        std::fs::read(root.path().join("harness-prefs.json")).unwrap(),
        prefs_before
    );
    core.shutdown().await;
    println!(
        "CORE04_LIVE_REGISTRY={{\"collisionRejected\":true,\"watchUpdated\":true,\"missingOptOutPreserved\":true}}"
    );
}

#[tokio::test]
async fn delayed_model_discovery_cannot_publish_after_source_or_registry_change() {
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(HarnessRegistry::new());
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let mut adapter = CatalogAdapter::new("acme.provider/agent");
    adapter.gate = Some((entered.clone(), release.clone()));
    let source = adapter.source.clone();
    registry.try_register(Arc::new(adapter)).unwrap();
    let core = EngineCore::assemble(root.path(), registry.clone(), HarnessId::Mock, None).unwrap();
    let client = Arc::new(luna_rpc::memory_client(core.rpc_service()));
    for change_source in [true, false] {
        let client = client.clone();
        let pending = tokio::spawn(async move {
            client
                .call(
                    methods::GET_PROVIDER_MODELS,
                    serde_json::json!({"harness":"acme.provider/agent", "refresh":true}),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        if change_source {
            *source.lock().unwrap() = "installation-b".into();
        } else {
            registry.unregister(HarnessId::parse("acme.provider/agent").unwrap());
        }
        release.add_permits(1);
        let error = pending.await.unwrap().unwrap_err();
        assert!(
            error.to_string().contains("changed during"),
            "wrong rejection: {error}"
        );
    }
    assert!(
        !root.path().join("model-cache").exists(),
        "stale discovery was persisted"
    );
    core.shutdown().await;
    println!(
        "CORE04_STALE_CATALOG={{\"sourceChangeRejected\":true,\"removalRejected\":true,\"staleCacheNotWritten\":true}}"
    );
}
