//! ZI-05 measurement probe: bounded literal text search over real checkouts.
//!
//! Read-only for the measured roots: each one is registered as a plain-folder
//! space in a disposable engine data directory and searched through the real
//! `SearchWorkspaceText` RPC. Run with `GIT_OPTIONAL_LOCKS=0` so incidental
//! `git` reads never refresh a measured repository's index.
//!
//! Usage: `workspace_text_search_probe <absolute-output.json> <root>...`
//! (optional `LUNA_ZI05_QUERIES="a,b,c"`). Writes one JSON document with
//! first/warm latency, files, bytes, status and limits per root and query,
//! concurrent and superseding requests, and the process's peak RSS.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use luna_engine::{EngineCore, HarnessRegistry};
use luna_proto::HarnessId;
use luna_rpc::{RpcReply, RpcService, methods};
use serde_json::{Value, json};

const DEFAULT_QUERIES: &[&str] = &["ZI05_NO_MATCH_SENTINEL_q7", "WorkspaceFiles", "TODO", "fn "];

async fn search(rpc: &dyn RpcService, params: Value) -> (Value, u128) {
    let started = Instant::now();
    let reply = rpc
        .handle(methods::SEARCH_WORKSPACE_TEXT, params)
        .await
        .unwrap_or_else(|error| panic!("text search failed: {error}"));
    let RpcReply::Value(value) = reply else {
        panic!("text search must return one value")
    };
    (value, started.elapsed().as_millis())
}

fn summary(value: &Value, wall_ms: u128) -> Value {
    json!({
        "status": value["status"],
        "limits": value["limits"],
        "matchCount": value["matchCount"],
        "files": value["files"].as_array().map_or(0, Vec::len),
        "filesScanned": value["filesScanned"],
        "bytesScanned": value["bytesScanned"],
        "skippedBinary": value["skippedBinary"],
        "skippedLarge": value["skippedLarge"],
        "skippedUnreadable": value["skippedUnreadable"],
        "hostElapsedMs": value["elapsedMs"],
        "wallMs": wall_ms,
    })
}

fn peak_rss_bytes() -> Option<u64> {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage fills the provided struct for RUSAGE_SELF.
        let usage = unsafe {
            if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
                return None;
            }
            usage.assume_init()
        };
        let raw = u64::try_from(usage.ru_maxrss).ok()?;
        // macOS reports bytes; Linux reports KiB.
        Some(if cfg!(target_os = "macos") {
            raw
        } else {
            raw * 1024
        })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

async fn measure_root(
    core: &EngineCore,
    rpc: &dyn RpcService,
    index: usize,
    root: &Path,
    queries: &[String],
) -> Value {
    let space = format!("zi05-space-{index}");
    core.workspace
        .create_space(
            &space,
            &core.device_id,
            &root.to_string_lossy(),
            None,
            false,
        )
        .expect("register measured root");
    let mut rows = Vec::new();
    for query in queries {
        let params = |budget: Option<Value>| {
            let mut params = json!({ "spaceId": space, "query": query });
            if let Some(budget) = budget {
                params["budget"] = budget;
            }
            params
        };
        let (first, first_ms) = search(rpc, params(None)).await;
        let mut warm = Vec::new();
        for _ in 0..3 {
            let (value, wall) = search(rpc, params(None)).await;
            warm.push(summary(&value, wall));
        }
        let interactive = json!({
            "maxFiles": 25_000, "maxBytes": 128 * 1024 * 1024, "maxElapsedMs": 1_500,
        });
        let (typed, typed_ms) = search(rpc, params(Some(interactive))).await;
        rows.push(json!({
            "query": query,
            "first": summary(&first, first_ms),
            "warm": warm,
            "interactiveBudget": summary(&typed, typed_ms),
        }));
    }

    // Four sessions at once: two scan, two queue inside their budgets.
    let started = Instant::now();
    let concurrent = futures::future::join_all((0..4).map(|session| {
        let params = json!({
            "spaceId": space, "query": queries[0],
            "searchSession": format!("probe-{session}"), "generation": 1,
        });
        async move {
            let (value, wall) = search(rpc, params).await;
            summary(&value, wall)
        }
    }))
    .await;
    let concurrent_ms = started.elapsed().as_millis();

    // One session typing: the second generation supersedes the first.
    let first = json!({
        "spaceId": space, "query": queries[0], "searchSession": "probe-typing", "generation": 1,
    });
    let second = json!({
        "spaceId": space, "query": queries[0], "searchSession": "probe-typing", "generation": 2,
    });
    let ((older, older_ms), (newer, newer_ms)) =
        futures::future::join(search(rpc, first), search(rpc, second)).await;

    json!({
        "root": root,
        "queries": rows,
        "concurrent": { "requests": concurrent, "wallMs": concurrent_ms },
        "superseding": {
            "older": summary(&older, older_ms),
            "newer": summary(&newer, newer_ms),
        },
        "peakRssBytesAfter": peak_rss_bytes(),
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let output = PathBuf::from(
        args.next()
            .ok_or("workspace_text_search_probe <absolute-output.json> <root>...")?,
    );
    assert!(output.is_absolute(), "output path must be absolute");
    let roots = args
        .map(|root| std::fs::canonicalize(root).expect("measured root exists"))
        .collect::<Vec<_>>();
    assert!(!roots.is_empty(), "at least one root is required");
    let queries = std::env::var("LUNA_ZI05_QUERIES")
        .map(|value| value.split(',').map(str::to_string).collect::<Vec<_>>())
        .unwrap_or_else(|_| {
            DEFAULT_QUERIES
                .iter()
                .map(|query| query.to_string())
                .collect()
        });

    let data = tempfile::tempdir()?;
    std::fs::write(data.path().join("device-id"), "zi05-probe")?;
    let core = EngineCore::assemble(
        data.path(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )?;
    let rpc = core.rpc_service();
    let baseline_rss = peak_rss_bytes();
    let mut measured = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        measured.push(measure_root(&core, rpc.as_ref(), index, root, &queries).await);
    }
    let document = json!({
        "ticket": "ZI-05",
        "engineVersion": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "peakRssBytesBefore": baseline_rss,
        "peakRssBytesAfter": peak_rss_bytes(),
        "roots": measured,
    });
    std::fs::write(&output, serde_json::to_vec_pretty(&document)?)?;
    println!("{}", output.display());
    core.shutdown().await;
    Ok(())
}
