//! Historical usage through the public scanner, real JSONL files and its disk cache.
//! Failure cases: equal per-call counts with advancing totals, duplicate notifications,
//! cold archived history, a cached rollout moved to the archive, and overflowing
//! viewer-supplied period boundaries. Environment merges must preserve pricing
//! warnings and incomplete-summary evidence, without multiplying duplicate warnings.
//! Parser upgrades must retain unavailable history, reparse available sources,
//! and avoid duplicating a retained legacy record after a rollout is archived.
//! Retention also applies when the provider removes the transcript directory.
//! No provider credentials or network are used.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use luna_engine::usage::snapshot_live;
use luna_proto::{UsageProvider, UsageRequest, UsageSnapshot, merge_usage_snapshots};
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

#[test]
fn environment_merge_keeps_scanner_pricing_warnings() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path(), "sessions", &[10]);
    let scanned = snapshot(root.path(), request());
    assert_eq!(scanned.summary.warnings.len(), 1);
    assert!(scanned.summary.warnings[0].contains("model prices unavailable"));

    let merged = merge_usage_snapshots(vec![scanned.clone()]).unwrap();
    assert_eq!(merged.summary.warnings, scanned.summary.warnings);
    assert_eq!(total(&merged), 20);

    let duplicate = merge_usage_snapshots(vec![scanned.clone(), scanned.clone()]).unwrap();
    assert_eq!(
        duplicate
            .summary
            .warnings
            .iter()
            .filter(|warning| { *warning == &scanned.summary.warnings[0] })
            .count(),
        1,
        "one pricing warning suffices when multiple views share a physical source"
    );
    assert!(
        duplicate
            .summary
            .warnings
            .iter()
            .any(|warning| { warning.contains("duplicate physical source excluded") })
    );
    assert_eq!(total(&duplicate), 20);
}

#[test]
fn environment_merge_keeps_older_partial_summary_and_distinct_warnings() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path(), "sessions", &[10]);
    let mut newest = snapshot(root.path(), request());
    newest.summary.warnings.clear();
    let mut older = newest.clone();
    older.summary.read_at_ms -= 1;
    older.summary.partial = true;
    older.summary.warnings = vec!["A remote history page was unavailable".into()];
    older.summary.sources[0].fingerprint = "another-machine".into();
    assert!(!newest.summary.partial);

    let merged = merge_usage_snapshots(vec![older.clone(), newest]).unwrap();
    assert!(
        merged.summary.partial,
        "newer complete data cannot erase another environment's partial state"
    );
    assert_eq!(merged.summary.warnings, older.summary.warnings);
    assert_eq!(total(&merged), 40);
}

fn legacy_cache(root: &Path) -> PathBuf {
    let original = snapshot(root, request());
    assert_eq!(total(&original), 20);
    let mut cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("data/usage-scan-v2.json")).unwrap())
            .unwrap();
    cache["version"] = json!(2);
    for file in cache["files"].as_object_mut().unwrap().values_mut() {
        file.as_object_mut().unwrap().remove("legacy_parser");
        for record in file["records"].as_array_mut().unwrap() {
            // Version 2 did not record Codex deduplication identities.
            record["dedupe"] = serde_json::Value::Null;
            record.as_object_mut().unwrap().remove("legacy_parser");
        }
    }
    let restored = root.join("data-reopened");
    std::fs::create_dir(&restored).unwrap();
    std::fs::write(
        restored.join("usage-scan-v2.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
    restored
}

fn reopened_snapshot(root: &Path, data: &Path) -> UsageSnapshot {
    snapshot_live(
        request(),
        Some(root.join("codex")),
        root.join("unused-claude"),
        data.to_owned(),
        vec![UsageProvider::Codex],
    )
}

#[test]
fn parser_upgrade_retains_deleted_rollout_history_with_an_explicit_warning() {
    let root = tempfile::tempdir().unwrap();
    let file = fixture(root.path(), "sessions", &[10]);
    let restored = legacy_cache(root.path());
    std::fs::remove_file(file).unwrap();
    let upgraded = reopened_snapshot(root.path(), &restored);
    assert_eq!(
        total(&upgraded),
        20,
        "an upgrade cannot recreate deleted provider history"
    );
    assert!(upgraded.summary.partial);
    assert!(
        upgraded
            .summary
            .warnings
            .iter()
            .any(|warning| warning.contains("older cached totals"))
    );
    let warm = reopened_snapshot(root.path(), &restored);
    assert_eq!(total(&warm), 20);
    assert!(warm.summary.partial);

    // Read a second persisted copy to exercise another cold cache load.
    let second = root.path().join("data-reopened-again");
    std::fs::create_dir(&second).unwrap();
    std::fs::copy(
        restored.join("usage-scan-v2.json"),
        second.join("usage-scan-v2.json"),
    )
    .unwrap();
    let restarted = reopened_snapshot(root.path(), &second);
    assert_eq!(total(&restarted), 20);
    assert!(restarted.summary.partial);
}

#[test]
fn parser_upgrade_reparses_available_rollouts_and_supersedes_archived_legacy_rows() {
    for archived in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let file = fixture(root.path(), "sessions", &[10]);
        let restored = legacy_cache(root.path());
        if archived {
            let archive = root.path().join("codex/archived_sessions");
            std::fs::create_dir(&archive).unwrap();
            std::fs::rename(file, archive.join("rollout.jsonl")).unwrap();
        } else {
            // Available transcripts must be reparsed even when metadata and
            // guards still match a legacy cache's older interpretation.
            let path = restored.join("usage-scan-v2.json");
            let mut cache: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            for cached in cache["files"].as_object_mut().unwrap().values_mut() {
                cached["records"][0]["tokens"]["output"] = json!(999);
            }
            std::fs::write(path, serde_json::to_vec(&cache).unwrap()).unwrap();
        }
        let upgraded = reopened_snapshot(root.path(), &restored);
        assert_eq!(
            total(&upgraded),
            20,
            "available or archived source is authoritative"
        );
        assert!(!upgraded.summary.partial);
        assert!(upgraded.summary.sources[0].files_scanned > 0);
        assert_eq!(total(&reopened_snapshot(root.path(), &restored)), 20);
    }
}

#[test]
fn unknown_cache_versions_do_not_publish_unverifiable_history() {
    let root = tempfile::tempdir().unwrap();
    let file = fixture(root.path(), "sessions", &[10]);
    let restored = legacy_cache(root.path());
    std::fs::remove_file(file).unwrap();
    let path = restored.join("usage-scan-v2.json");
    let mut cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cache["version"] = json!(u32::MAX);
    std::fs::write(path, serde_json::to_vec(&cache).unwrap()).unwrap();
    assert_eq!(total(&reopened_snapshot(root.path(), &restored)), 0);
}

#[test]
fn cached_history_survives_a_removed_transcript_directory() {
    for (legacy, directory) in [
        (false, "sessions"),
        (true, "sessions"),
        (false, "archived_sessions"),
        (true, "archived_sessions"),
    ] {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), directory, &[10]);
        let restored = legacy_cache(root.path());
        let data = if legacy {
            restored
        } else {
            root.path().join("data")
        };
        std::fs::remove_dir_all(root.path().join("codex").join(directory)).unwrap();
        let retained = reopened_snapshot(root.path(), &data);
        assert_eq!(
            total(&retained),
            20,
            "removing {directory} does not invalidate retained cached usage (legacy={legacy})"
        );
        assert!(retained.summary.partial);
        assert_eq!(total(&reopened_snapshot(root.path(), &data)), 20);
    }
}

// This process boundary isolates HOME without changing other concurrent tests.
// Failure modes: approved QA data is mistaken for the provider home, arbitrary
// provider children are allowed, or a nested/symlink path escapes the exception.
#[test]
fn isolated_qa_cache_paths_preserve_provider_home_boundaries() {
    if let Some(home) = std::env::var_os("LUNA_HISTORY_CACHE_PATH_FIXTURE") {
        let home = PathBuf::from(home);
        let fixture_root = home.join("fixtures");
        fixture(&fixture_root, "sessions", &[10]);
        let checkout = home.join(".codex/worktrees/qa/luna");
        let accepted = checkout.join("target/luna-qa-data/history");
        std::fs::create_dir_all(&accepted).unwrap();
        // A Git worktree registration points in both directions. Keep this
        // metadata inside the disposable HOME; never edit the real registry.
        let registration = home.join("repository/.git/worktrees/history");
        std::fs::create_dir_all(&registration).unwrap();
        let git_file = checkout.join(".git");
        std::fs::write(&git_file, format!("gitdir: {}\n", registration.display())).unwrap();
        std::fs::write(
            registration.join("gitdir"),
            format!("{}\n", git_file.display()),
        )
        .unwrap();
        let scanned = reopened_snapshot(&fixture_root, &accepted);
        assert_eq!(total(&scanned), 20);
        assert!(
            accepted.join("usage-scan-v2.json").is_file(),
            "isolated QA data must retain the scan cache"
        );
        assert!(reopened_snapshot(&fixture_root, &accepted).summary.sources[0].files_cached > 0);

        let mut rejected = vec![
            home.join(".codex/arbitrary/data"),
            checkout.join("arbitrary-data"),
            accepted.join("nested"),
            home.join(".codex/worktrees/unregistered/luna/target/luna-qa-data/history"),
        ];
        let copied_checkout = home.join(".codex/worktrees/copied/luna");
        std::fs::create_dir_all(&copied_checkout).unwrap();
        std::fs::copy(&git_file, copied_checkout.join(".git")).unwrap();
        rejected.push(copied_checkout.join("target/luna-qa-data/history"));
        #[cfg(unix)]
        {
            let outside = home.join(".codex/private-cache-target");
            std::fs::create_dir_all(&outside).unwrap();
            let link = checkout.join("target/luna-qa-data/linked");
            std::os::unix::fs::symlink(outside, &link).unwrap();
            rejected.push(link);
        }
        for data in rejected {
            std::fs::create_dir_all(&data).unwrap();
            assert_eq!(total(&reopened_snapshot(&fixture_root, &data)), 20);
            assert!(
                !data.join("usage-scan-v2.json").exists(),
                "provider-home descendants must remain read-only"
            );
        }
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "isolated_qa_cache_paths_preserve_provider_home_boundaries",
            "--nocapture",
        ])
        .env("HOME", home.path())
        .env("LUNA_HISTORY_CACHE_PATH_FIXTURE", home.path())
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated cache path regression failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
