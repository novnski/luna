//! Public RPC E2E, real fixture files and cache persistence. Failure cases:
//! cached quota reads wait behind history; readers duplicate/discard refresh;
//! history-only clears quotas; transient failures erase values; cached values
//! cross account/provider boundaries; restart loses the last complete view.
//! Provider fixtures are explicit and never access real credentials or network.
use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::stream::BoxStream;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{AgentEvent, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode};
use luna_rpc::{RpcReply, RpcService, methods};
use serde_json::{Value, json};

struct FixtureProvider(HarnessId);
#[async_trait]
impl Harness for FixtureProvider {
    fn id(&self) -> HarnessId {
        self.0
    }
    fn display_name(&self) -> &str {
        "Isolated usage fixture"
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
        _: RunRequest,
        _: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        panic!("usage must never start a model turn")
    }
}

async fn read(rpc: &dyn RpcService, request: Value) -> Value {
    match rpc
        .handle(methods::GET_USAGE_SNAPSHOT, request)
        .await
        .unwrap()
    {
        RpcReply::Value(value) => value,
        _ => panic!("usage is a snapshot"),
    }
}

fn limits(percent: f64) -> Value {
    json!([{"provider":"claude","verifiedAccountId":"fixture-claude", "accountLabel":"Fixture", "plan":"Max",
        "checkedAtMs":chrono::Utc::now().timestamp_millis(),"availability":"ready",
        "windows":[{"id":"five_hour","label":"Five-hour","usedPercent":percent,"durationMinutes":300,
            "resetsAtMs":chrono::Utc::now().timestamp_millis()+3_600_000}]}])
}

#[test]
fn usage_cache_background_lanes_failure_and_restart() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        for name in ["codex", "claude", "cache"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join(".luna-usage-synthetic"), "fixture").unwrap();
        }
        std::fs::write(root.join(".luna-usage-synthetic"), "fixture").unwrap();
        std::fs::write(root.join("prices.json"), "{}").unwrap();
        let limit_file = root.join("limits.json");
        std::fs::write(&limit_file, limits(20.0).to_string()).unwrap();
        let variables = [
            ("LUNA_USAGE_SYNTHETIC_CODEX_HOME", root.join("codex")),
            ("LUNA_USAGE_SYNTHETIC_CLAUDE_HOME", root.join("claude")),
            ("LUNA_USAGE_SYNTHETIC_CACHE_DIR", root.join("cache")),
            ("LUNA_USAGE_SYNTHETIC_PRICES_JSON", root.join("prices.json")),
            ("LUNA_USAGE_SYNTHETIC_LIMITS_JSON", limit_file.clone()),
        ];
        for (name, path) in &variables { unsafe { std::env::set_var(name, path); } }
        let timestamp = chrono::Utc::now().to_rfc3339();
        std::fs::write(root.join("codex/history.jsonl"), format!("{}\n{}\n{}\n",
            json!({"type":"session_meta","timestamp":timestamp,"payload":{"id":"usage-fixture"}}),
            json!({"type":"turn_context","timestamp":timestamp,"payload":{"model":"fixture"}}),
            json!({"type":"event_msg","timestamp":timestamp,"payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":5},"total_token_usage":{"input_tokens":10,"output_tokens":5}}}}))).unwrap();
        let config = AgentAccountsConfig { data_dir:root.join("data"), codex_home:root.join("native-codex"), claude_config_dir:root.join("native-claude"), claude_config_file:root.join("native-claude.json") };
        config.disable_claude_keychain_fallback().unwrap();
        let build = || {
            let registry = Arc::new(HarnessRegistry::new());
            for id in [HarnessId::Codex, HarnessId::ClaudeCode, HarnessId::Opencode] { registry.register(Arc::new(FixtureProvider(id))); }
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                EngineProfile::development(&config.data_dir,"usage-qa","usage-qa"), registry,
                HarnessId::Codex,None,config.clone()).unwrap()
        };
        let core = build();
        let rpc = core.rpc_service();
        let request = json!({"days":7,"includeHistory":false,"includeLimits":true,"background":true});
        let first = read(rpc.as_ref(),request.clone()).await;
        assert!(first.get("refreshing").is_some(), "cached snapshot must report background progress");
        let warm = tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                let value = read(rpc.as_ref(),request.clone()).await;
                if value["limits"].as_array().unwrap().iter().any(|l| l["provider"]=="claude" && l["windows"][0]["usedPercent"]==20.0) { break value; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.unwrap();
        assert!(warm["summary"]["buckets"].as_array().unwrap().is_empty(),"quota-only reads must not scan history");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || { let _=started_tx.send(()); let _=release_rx.recv_timeout(Duration::from_secs(5)); });
        started_rx.await.unwrap();
        let fast = tokio::time::timeout(Duration::from_millis(200),read(rpc.as_ref(),request.clone())).await;
        release_tx.send(()).unwrap(); blocker.await.unwrap();
        let fast = fast.expect("cached quota read must not wait for a blocked history worker");
        assert_eq!(fast["limits"],warm["limits"]);
        std::fs::write(&limit_file,limits(42.0).to_string()).unwrap();
        let updated = read(rpc.as_ref(),json!({"days":7,"includeHistory":false,"refresh":true})).await;
        assert_eq!(updated["limits"].as_array().unwrap().iter().find(|l| l["provider"]=="claude").unwrap()["windows"][0]["usedPercent"],42.0);
        std::fs::write(&limit_file,"not valid JSON").unwrap();
        let retained = read(rpc.as_ref(),json!({"days":7,"includeHistory":false,"refresh":true})).await;
        let claude = retained["limits"].as_array().unwrap().iter().find(|l| l["provider"]=="claude").unwrap();
        assert_eq!(claude["windows"][0]["usedPercent"],42.0);
        assert_eq!(claude["availability"],"probeFailed");
        let history = read(rpc.as_ref(),json!({"days":7,"includeLimits":false})).await;
        assert!(history["limits"].as_array().unwrap().is_empty());
        assert!(!history["summary"]["buckets"].as_array().unwrap().is_empty());
        let again = read(rpc.as_ref(),request.clone()).await;
        assert_eq!(again["limits"],retained["limits"],"history reads must not clear quota cache");
        assert!(core.shutdown().await); drop(rpc); drop(core);
        let next = build(); let rpc = next.rpc_service();
        let restarted = read(rpc.as_ref(),request).await;
        assert_eq!(restarted["limits"],retained["limits"],"restart must retain the scoped last-known values");
        if let Ok(output) = std::env::var("LUNA_USAGE_E2E_DIR") {
            let output = std::path::PathBuf::from(output); std::fs::create_dir_all(&output).unwrap();
            std::fs::write(output.join("usage-refresh.json"),serde_json::to_vec_pretty(&json!({"passed":true,"cold":first,"warm":warm,"refreshed":updated,"retained":retained,"history":history,"restarted":restarted})).unwrap()).unwrap();
        }
        assert!(next.shutdown().await);
        for (name,_) in &variables { unsafe { std::env::remove_var(name); } }
    });
}
