//! Read-only native quota acceptance. No model prompt, GUI or provider-store
//! mutation is sent. The engine owns a disposable data directory; normal CLI
//! profiles remain provider-owned. Optional `--watch` verifies a second native
//! observation arrives with no intervening usage request.
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile};
use luna_proto::HarnessId;
use luna_rpc::{RpcReply, RpcService, methods};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

async fn read(rpc: &dyn RpcService) -> (Value, u128) {
    let started = Instant::now();
    let reply = rpc
        .handle(
            methods::GET_USAGE_SNAPSHOT,
            json!({"days":7,"includeHistory":false,"background":true}),
        )
        .await
        .unwrap();
    let RpcReply::Value(value) = reply else {
        panic!("usage must return a snapshot")
    };
    (value, started.elapsed().as_millis())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--journal-scan") {
        let root = PathBuf::from(std::env::args().nth(2).ok_or("missing fixture root")?);
        let value = luna_engine::usage::snapshot_live(
            luna_proto::UsageRequest {
                days: 7,
                period_starts_ms: vec![],
                until_ms: None,
                include_limits: false,
                price_overrides: Default::default(),
            },
            Some(root.join("codex")),
            root.join("unused-claude"),
            root.join("data"),
            vec![luna_proto::UsageProvider::Codex],
        );
        println!("{}", serde_json::to_string(&value)?);
        return Ok(());
    }
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("usage_cache_readback <absolute-output.json> [--watch]")?,
    );
    assert!(output.is_absolute());
    if std::env::args().any(|arg| arg == "--journal-check") {
        return journal_check(output);
    }
    let temp = tempfile::tempdir()?;
    let config = AgentAccountsConfig::detect(temp.path());
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config(
        EngineProfile::development(temp.path(), "usage-native-qa", "usage-native-qa"),
        Arc::new(luna_engine::registry::default_registry()),
        HarnessId::Codex,
        None,
        config,
    )?;
    let rpc = core.rpc_service();
    // No usage request before this: the host's background bootstrap does it.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let first_requested_at = chrono::Utc::now().timestamp_millis();
    let (first, first_ms) = read(rpc.as_ref()).await;
    let (warm, warm_ms) = read(rpc.as_ref()).await;
    let mut result = json!({"firstRequestedAtMs":first_requested_at,"firstMs":first_ms,"warmMs":warm_ms,"first":first,"warm":warm});
    assert!(
        first_ms < 200 && warm_ms < 200,
        "cached quota reads must be fast"
    );
    assert!(
        result["first"]["limits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["availability"] == "ready"
                && l["checkedAtMs"]
                    .as_i64()
                    .is_some_and(|at| at < first_requested_at)),
        "native quota must be warmed without a UI request"
    );
    if std::env::args().any(|arg| arg == "--watch") {
        tokio::time::sleep(Duration::from_secs(82)).await;
        let next_requested_at = chrono::Utc::now().timestamp_millis();
        let (next, next_ms) = read(rpc.as_ref()).await;
        result["nextRequestedAtMs"] = json!(next_requested_at);
        result["nextMs"] = json!(next_ms);
        let changed = next["limits"].as_array().unwrap().iter().any(|limit| {
            let previous = result["first"]["limits"]
                .as_array()
                .unwrap()
                .iter()
                .find(|old| old["provider"] == limit["provider"]);
            limit["checkedAtMs"].as_i64().is_some_and(|at| {
                at < next_requested_at
                    && previous
                        .and_then(|p| p["checkedAtMs"].as_i64())
                        .is_some_and(|old| at > old)
            })
        });
        result["next"] = next;
        result["backgroundAdvancedWithoutReads"] = json!(changed);
        assert!(
            changed,
            "background refresh must continue while no view requests usage"
        );
    }
    assert!(core.shutdown().await);
    result["passed"] = json!(true);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output, serde_json::to_vec_pretty(&result)?)?;
    println!(
        "{}",
        json!({"passed":true,"firstMs":first_ms,"warmMs":warm_ms,"evidence":output})
    );
    Ok(())
}

/// Public-scanner E2E across separate processes: tiny appends must leave the
/// checkpoint untouched; a torn UTF-8 journal tail must not hide later writes;
/// restart must retain history after the source file is no longer available.
fn journal_check(output: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    std::fs::create_dir_all(root.join("codex/sessions"))?;
    std::fs::create_dir_all(root.join("data"))?;
    let file = root.join("codex/sessions/rollout.jsonl");
    let timestamp = chrono::Utc::now().to_rfc3339();
    std::fs::write(
        &file,
        format!(
            "{}\n{}\n",
            json!({"type":"session_meta","timestamp":timestamp,"payload":{"id":"usage-journal-e2e"}}),
            json!({"type":"turn_context","timestamp":timestamp,"payload":{"model":"synthetic-usage-model"}})
        ),
    )?;
    let append = |total: u64| -> std::io::Result<()> {
        writeln!(
            std::fs::OpenOptions::new().append(true).open(&file)?,
            "{}",
            json!({"type":"event_msg","timestamp":timestamp,"payload":{"type":"token_count","info":{
                "last_token_usage":{"input_tokens":10,"output_tokens":10},
                "total_token_usage":{"input_tokens":total,"output_tokens":total}}}})
        )
    };
    let scan = || -> Result<Value, Box<dyn std::error::Error>> {
        let result = std::process::Command::new(std::env::current_exe()?)
            .arg("--journal-scan")
            .arg(root)
            .output()?;
        assert!(result.status.success(), "scanner child failed");
        Ok(serde_json::from_slice(&result.stdout)?)
    };
    let tokens = |value: &Value| -> u64 {
        serde_json::from_value::<luna_proto::UsageSnapshot>(value.clone())
            .unwrap()
            .summary
            .buckets
            .iter()
            .map(|b| b.tokens.total())
            .sum()
    };
    append(10)?;
    let first = scan()?;
    assert_eq!(tokens(&first), 20);
    let base_path = root.join("data/usage-scan-v2.json");
    let base = std::fs::read(&base_path)?;
    append(20)?;
    let second = scan()?;
    assert_eq!(tokens(&second), 40);
    assert_eq!(
        std::fs::read(&base_path)?,
        base,
        "small append rewrote checkpoint"
    );
    let journal = base_path.with_extension("updates.jsonl");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&journal)?
        .write_all(b"\xff{\"torn\":")?;
    append(30)?;
    let after_torn_write = scan()?;
    assert_eq!(tokens(&after_torn_write), 60);
    std::fs::remove_file(&file)?;
    let retained_after_restart = scan()?;
    assert_eq!(
        tokens(&retained_after_restart),
        60,
        "journal did not replay without source"
    );
    let report = json!({"passed":true,"checkpointUnchanged":true,"tornUtf8Recovered":true,
        "retainedAfterSourceRemoval":true,"first":first,"second":second,
        "afterTornWrite":after_torn_write,"retainedAfterRestart":retained_after_restart});
    std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", json!({"passed":true,"evidence":output}));
    Ok(())
}
