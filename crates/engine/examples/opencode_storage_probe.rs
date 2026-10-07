//! QA isolation probe for LUNA's OpenCode readers (OCS-01). It assembles an
//! engine in a temporary data directory with only the OpenCode harness and
//! calls the app's own RPCs, so the readers run exactly as in the desktop
//! app, under THIS process's environment:
//!
//! - `GetUsageSnapshot {days, includeLimits: true}`: the OpenCode history
//!   source, its per-model token totals and the OpenCode Go limits row;
//! - `GetIntegrationInventory {provider: "opencode"}`: the global skill
//!   library under the OpenCode config root.
//!
//! It also prints the storage environment the readers derived
//! (`OpencodeStorage`: executable, wrappers, data/config/database paths).
//! Only paths, names, counts and tokens are printed; no message text, no
//! credential value.
//!
//!     OPENCODE_EXECUTABLE=<profile>/bin/opencode HOME=<sandbox> \
//!       cargo run -p luna-engine --example opencode_storage_probe -- [--days 1]

use std::collections::BTreeMap;
use std::sync::Arc;

use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_harness::OpencodeHarness;
use luna_harness::opencode::OpencodeStorage;
use luna_proto::HarnessId;
use serde_json::{Value, json};

fn path_or_reason(
    value: Result<Option<std::path::PathBuf>, luna_harness::opencode::StorageIndeterminate>,
) -> Value {
    match value {
        Ok(Some(path)) => json!(path),
        Ok(None) => Value::Null,
        Err(error) => json!(format!("indeterminate: {error}")),
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
    let mut days = 1;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--days" => days = args.next().ok_or("--days needs a value")?.parse()?,
            other => return Err(format!("unknown argument {other}").into()),
        }
    }

    let storage = OpencodeStorage::current();
    let derived = json!({
        "executable": storage.executable(),
        "wrappers": storage.wrappers(),
        "home": path_or_reason(storage.home()),
        "dataDir": path_or_reason(storage.data_dir()),
        "configDir": path_or_reason(storage.config_dir()),
        "globalConfigDir": path_or_reason(storage.global_config_dir()),
        "database": path_or_reason(storage.database()),
    });

    let root = tempfile::tempdir()?;
    let data = root.path().join("data");
    let accounts_dir = data.join("provider-state");
    std::fs::create_dir_all(&accounts_dir)?;
    // A fresh, empty price cache keeps the Usage RPC off the network.
    let rates = json!({"fetched_at_ms": chrono::Utc::now().timestamp_millis(), "rates": {}});
    for dir in [&data, &accounts_dir] {
        std::fs::write(dir.join("usage-model-rates.json"), rates.to_string())?;
    }
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(OpencodeHarness::new()));
    let accounts = AgentAccountsConfig {
        data_dir: accounts_dir,
        claude_config_dir: data.join("unused-claude"),
        claude_config_file: data.join("unused-claude.json"),
        codex_home: data.join("unused-codex"),
    };
    accounts.disable_claude_keychain_fallback()?;
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&data, "qa-org", "qa-user"),
        Arc::new(registry),
        HarnessId::Opencode,
        None,
        accounts,
    )?;
    let client = luna_rpc::memory_client(core.rpc_service());

    let snapshot = client
        .call(
            luna_rpc::methods::GET_USAGE_SNAPSHOT,
            json!({"days": days, "includeLimits": true}),
        )
        .await?;
    let source = snapshot["summary"]["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|source| source["provider"] == "openCodeGo")
        .cloned();
    let mut models: BTreeMap<String, Value> = BTreeMap::new();
    for bucket in snapshot["summary"]["buckets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|bucket| bucket["provider"] == "openCodeGo")
    {
        let model = bucket["model"].as_str().unwrap_or_default().to_owned();
        let entry = models.entry(model).or_insert_with(|| {
            json!({"records": 0, "uncachedInput": 0, "cachedInput": 0, "cacheCreation": 0,
                   "output": 0, "reasoning": 0})
        });
        for (key, value) in [
            ("records", &bucket["records"]),
            ("uncachedInput", &bucket["tokens"]["uncachedInput"]),
            ("cachedInput", &bucket["tokens"]["cachedInput"]),
            ("cacheCreation", &bucket["tokens"]["cacheCreation"]),
            ("output", &bucket["tokens"]["output"]),
            ("reasoning", &bucket["tokens"]["reasoning"]),
        ] {
            entry[key] = json!(entry[key].as_u64().unwrap_or(0) + value.as_u64().unwrap_or(0));
        }
    }
    let limits: Vec<Value> = snapshot["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|limit| limit["provider"] == "openCodeGo")
        .map(|limit| {
            json!({
                "availability": limit["availability"],
                "detail": limit["detail"],
                "windows": limit["windows"].as_array().map_or(0, Vec::len),
                "verifiedAccount": !limit["verifiedAccountId"].is_null(),
            })
        })
        .collect();

    let inventory = client
        .call(
            luna_rpc::methods::GET_INTEGRATION_INVENTORY,
            json!({"provider": "opencode"}),
        )
        .await;
    let skills = match inventory {
        Ok(view) => json!({
            "unavailableReason": view["unavailableReason"],
            "items": view["items"].as_array().into_iter().flatten().map(|item| json!({
                "name": item["name"], "scope": item["scope"], "source": item["source"],
            })).collect::<Vec<_>>(),
        }),
        Err(error) => json!({"error": error.to_string()}),
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "derivedStorage": derived,
            "usage": {
                "days": days,
                "source": source.map(|source| json!({
                    "status": source["status"],
                    "detail": source["detail"],
                    "distinctSessions": source["distinctSessions"],
                    "filesScanned": source["filesScanned"],
                })),
                "models": models,
                "limits": limits,
            },
            "skills": skills,
        }))?
    );
    Ok(())
}
