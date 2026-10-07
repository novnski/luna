//! Read-only OpenCode usage-history readback through the public Usage scanner.
//! Prints only aggregate counts per model (records, tokens, cost); message
//! text is never read. Point it at a COPY of an OpenCode data directory, or
//! at an isolated QA store.
//!
//!   cargo run -p luna-engine --example usage_opencode_readback -- \
//!     --data <opencode-data-dir> [--data <dir>...] [--days 1|7|30|90] \
//!     [--cache <luna-cache-dir>] [--refresh-prices]
//!
//! Without `--cache` a temporary LUNA data dir is used. Two passes run, so the
//! second shows the warm-cache path.

use std::collections::BTreeMap;
use std::path::PathBuf;

use luna_engine::usage::{refresh_live_prices, snapshot_live_with_opencode};
use luna_proto::{UsageProvider, UsageRequest, UsageSnapshot};
use serde_json::{Value, json};

fn summary(snapshot: &UsageSnapshot) -> Value {
    let mut models: BTreeMap<String, Value> = BTreeMap::new();
    for bucket in &snapshot.summary.buckets {
        let entry = models.entry(bucket.model.clone()).or_insert_with(|| {
            json!({
                "records": 0, "uncachedInput": 0, "cachedInput": 0, "cacheCreation": 0,
                "output": 0, "reasoning": 0, "total": 0, "knownCostUsd": 0.0,
                "unpricedRecords": 0, "costQualities": [],
            })
        });
        let add = |entry: &mut Value, key: &str, value: u64| {
            entry[key] = json!(entry[key].as_u64().unwrap_or(0) + value);
        };
        add(entry, "records", bucket.records);
        add(entry, "uncachedInput", bucket.tokens.uncached_input);
        add(entry, "cachedInput", bucket.tokens.cached_input);
        add(entry, "cacheCreation", bucket.tokens.cache_creation);
        add(entry, "output", bucket.tokens.output);
        add(entry, "reasoning", bucket.tokens.reasoning);
        add(entry, "total", bucket.tokens.total());
        add(
            entry,
            "unpricedRecords",
            bucket.unpriced_records.unwrap_or(0),
        );
        entry["knownCostUsd"] = json!(
            entry["knownCostUsd"].as_f64().unwrap_or(0.0) + bucket.known_cost_usd.unwrap_or(0.0)
        );
        let quality = json!(format!("{:?}", bucket.cost_quality));
        let qualities = entry["costQualities"].as_array_mut().unwrap();
        if !qualities.contains(&quality) {
            qualities.push(quality);
        }
    }
    let source = snapshot
        .summary
        .sources
        .iter()
        .find(|source| source.provider == UsageProvider::OpenCodeGo);
    json!({
        "days": snapshot.summary.days,
        "scanDurationMs": snapshot.summary.scan_duration_ms,
        "partial": snapshot.summary.partial,
        "warnings": snapshot.summary.warnings,
        "source": source.map(|source| json!({
            "status": format!("{:?}", source.status),
            "detail": source.detail,
            "distinctSessions": source.distinct_sessions,
            "filesScanned": source.files_scanned,
            "filesCached": source.files_cached,
            "failedFiles": source.failed_files,
        })),
        "buckets": snapshot.summary.buckets.len(),
        "models": models,
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut roots = Vec::new();
    let mut days = 30;
    let mut cache = None;
    let mut refresh = false;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--data" => roots.push(PathBuf::from(args.next().ok_or("--data needs a path")?)),
            "--days" => days = args.next().ok_or("--days needs a value")?.parse()?,
            "--cache" => cache = Some(PathBuf::from(args.next().ok_or("--cache needs a path")?)),
            "--refresh-prices" => refresh = true,
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if roots.is_empty() {
        return Err("pass at least one --data <opencode-data-dir>".into());
    }
    let temporary = tempfile::tempdir()?;
    let data = match cache {
        Some(path) => {
            std::fs::create_dir_all(&path)?;
            path
        }
        None => temporary.path().join("luna-data"),
    };
    std::fs::create_dir_all(&data)?;
    if refresh {
        refresh_live_prices(&data).await;
    }
    let request = UsageRequest {
        days,
        period_starts_ms: vec![],
        until_ms: None,
        include_limits: false,
        price_overrides: Default::default(),
    };
    let unused_claude = temporary.path().join("unused-claude");
    let mut passes = Vec::new();
    for _ in 0..2 {
        let snapshot = snapshot_live_with_opencode(
            request.clone(),
            None,
            unused_claude.clone(),
            roots.clone(),
            data.clone(),
            vec![UsageProvider::OpenCodeGo],
        );
        passes.push(summary(&snapshot));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({ "first": passes[0], "second": passes[1] }))?
    );
    Ok(())
}
