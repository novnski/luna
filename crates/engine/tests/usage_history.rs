//! Historical usage through the public scanner, real JSONL files and its disk cache.
//! Failure cases: equal per-call counts with advancing totals, duplicate notifications,
//! cold archived history, a cached rollout moved to the archive, and overflowing
//! viewer-supplied period boundaries. No provider credentials or network are used.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use luna_engine::usage::snapshot_live;
use luna_proto::{UsageProvider, UsageRequest, UsageSnapshot};
use serde_json::json;

fn request() -> UsageRequest {
    UsageRequest {
        days: 7,
        period_starts_ms: vec![],
        until_ms: None,
        include_limits: false,
        price_overrides: HashMap::new(),
    }
}

fn fixture(root: &Path, directory: &str, calls: &[u64]) -> PathBuf {
    let home = root.join("codex");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::create_dir_all(home.join(directory)).unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    let timestamp = chrono::Utc::now().to_rfc3339();
    let mut lines = vec![
        json!({"type":"session_meta", "timestamp":timestamp,
            "payload":{"id":"synthetic-usage-session"}}),
        json!({"type":"turn_context", "timestamp":timestamp,
            "payload":{"model":"synthetic-usage-model"}}),
    ];
    for total in calls {
        lines.push(
            json!({"type":"event_msg", "timestamp":timestamp, "payload":{
                "type":"token_count", "info":{
                    "last_token_usage":{"input_tokens":10,"output_tokens":10},
                    "total_token_usage":{"input_tokens":total,"output_tokens":total}
                }
            }}),
        );
    }
    let file = home.join(directory).join("rollout.jsonl");
    std::fs::write(
        &file,
        lines
            .into_iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>(),
    )
    .unwrap();
    file
}

fn snapshot(root: &Path, request: UsageRequest) -> UsageSnapshot {
    snapshot_live(
        request,
        Some(root.join("codex")),
        root.join("unused-claude"),
        root.join("data"),
        vec![UsageProvider::Codex],
    )
}

fn total(snapshot: &UsageSnapshot) -> u64 {
    snapshot
        .summary
        .buckets
        .iter()
        .map(|bucket| bucket.tokens.total())
        .sum()
}

#[test]
fn equal_calls_count_separately_but_repeated_notifications_do_not() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path(), "sessions", &[10, 10, 20, 20]);
    let cold = snapshot(root.path(), request());
    assert_eq!(
        total(&cold),
        40,
        "two distinct provider calls each used 20 tokens"
    );
    let warm = snapshot(root.path(), request());
    assert_eq!(
        total(&warm),
        40,
        "warm cache must preserve the corrected total"
    );
    assert!(warm.summary.sources[0].files_cached > 0);
}

#[test]
fn cold_snapshot_includes_archived_codex_sessions() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path(), "archived_sessions", &[10]);
    assert_eq!(total(&snapshot(root.path(), request())), 20);
}

#[test]
fn archiving_a_cached_rollout_never_counts_it_twice() {
    let root = tempfile::tempdir().unwrap();
    let file = fixture(root.path(), "sessions", &[10]);
    assert_eq!(total(&snapshot(root.path(), request())), 20);
    let archive = root.path().join("codex/archived_sessions");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::rename(file, archive.join("rollout.jsonl")).unwrap();
    let moved = snapshot(root.path(), request());
    assert_eq!(total(&moved), 20);
    assert!(
        !moved.summary.partial,
        "an ordinary archive move is not a failed scan"
    );
}

#[test]
fn extreme_period_boundaries_fall_back_without_panicking() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path(), "sessions", &[10]);
    let now = chrono::Utc::now().timestamp_millis();
    let mut invalid = request();
    invalid.period_starts_ms = vec![i64::MIN, now - 5, now - 4, now - 3, now - 2, now - 1, now];
    invalid.until_ms = Some(now + 1);
    let result = snapshot(root.path(), invalid);
    assert!(result.summary.period_starts_ms.is_empty());
    assert_eq!(total(&result), 20);
    assert_eq!(
        total(&snapshot(root.path(), request())),
        20,
        "invalid input must not poison subsequent requests"
    );
}
