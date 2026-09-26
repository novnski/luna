//! Public RPC/account integration regression. Failure mode: disconnecting the
//! selected account while its history scan is queued must not publish that
//! account's history under the new selection, with or without live limits.
use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::stream::BoxStream;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::{AgentEvent, HarnessId, Model, ReasoningLevel, RunRequest, SteeringMode};
use luna_rpc::{RpcReply, RpcService, methods};
use serde_json::json;

struct SyntheticCodex;
#[async_trait]
impl Harness for SyntheticCodex {
    fn id(&self) -> HarnessId {
        HarnessId::Codex
    }
    fn display_name(&self) -> &str {
        "Synthetic account selection"
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
        unreachable!("usage refresh never starts a provider task")
    }
}

#[test]
fn account_disconnect_during_scan_rejects_stale_usage_snapshot() {
    // Occupy the only blocking worker, then poll the actual RPC until its
    // history scan queues. This gives an exact boundary without timing sleeps,
    // provider credentials, production homes, or a new application test hook.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        for include_limits in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let config = AgentAccountsConfig {
                data_dir: root.path().join("data"), codex_home: root.path().join("codex"),
                claude_config_dir: root.path().join("claude"), claude_config_file: root.path().join("claude.json"),
            };
            config.disable_claude_keychain_fallback().unwrap();
            std::fs::create_dir_all(config.codex_home.join("sessions")).unwrap();
            std::fs::create_dir_all(&config.data_dir).unwrap();
            std::fs::write(config.codex_home.join("auth.json"), r#"{"OPENAI_API_KEY":"synthetic-never-used"}"#).unwrap();
            let timestamp = chrono::Utc::now().to_rfc3339();
            let rows = [
                json!({"type":"session_meta","timestamp":timestamp,"payload":{"id":"account-a"}}),
                json!({"type":"turn_context","timestamp":timestamp,"payload":{"model":"synthetic"}}),
                json!({"type":"event_msg","timestamp":timestamp,"payload":{"type":"token_count","info":{
                    "last_token_usage":{"input_tokens":17,"output_tokens":23},
                    "total_token_usage":{"input_tokens":17,"output_tokens":23}
                }}}),
            ];
            std::fs::write(config.codex_home.join("sessions/usage.jsonl"), rows.iter().map(|row| format!("{row}\n")).collect::<String>()).unwrap();
            // Fresh local prices suppress all network access in this test.
            std::fs::write(config.data_dir.join("usage-model-rates.json"), json!({
                "fetched_at_ms":chrono::Utc::now().timestamp_millis(),
                "rates":{"synthetic":{"inputPerMillion":1.0,"cachedPerMillion":0.0,"creationPerMillion":0.0,"outputPerMillion":1.0}}
            }).to_string()).unwrap();
            let registry = Arc::new(HarnessRegistry::new());
            registry.register(Arc::new(SyntheticCodex));
            let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                EngineProfile::development(&config.data_dir, "qa", "qa"), registry, HarnessId::Codex, None, config,
            ).unwrap();
            let rpc = core.rpc_service();
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
            });
            started_rx.await.unwrap();
            let mut refresh = rpc.handle(methods::GET_USAGE_SNAPSHOT, json!({"days":7,"includeLimits":include_limits}));
            assert!(futures::poll!(&mut refresh).is_pending());
            core.agent_accounts.forget(HarnessId::Codex, "codex-native").await.unwrap();
            release_tx.send(()).unwrap();
            blocker.await.unwrap();
            let result = tokio::time::timeout(Duration::from_secs(3), refresh).await.unwrap();
            let stale_tokens = match &result {
                Ok(RpcReply::Value(value)) => value["summary"]["buckets"].as_array()
                    .map(|buckets| buckets.iter().map(|bucket| bucket["tokens"]["uncachedInput"].as_u64().unwrap_or(0) + bucket["tokens"]["output"].as_u64().unwrap_or(0)).sum::<u64>()),
                _ => None,
            };
            assert!(core.shutdown().await);
            assert!(result.is_err(), "include_limits={include_limits}: published stale account-a history, tokens={stale_tokens:?}");
            assert!(result.err().unwrap().to_string().contains("account selection changed"));
        }
    });
}
