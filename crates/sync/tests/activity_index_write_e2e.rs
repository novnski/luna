//! File-backed Activity replacement acceptance, written before the lock change.
//! Failures: changed payload/filter columns; partial visibility; lost unrelated
//! rows; failed replacement not rolling back; Appshot replacement deleting
//! transcript rows; oversized/count-limited preparation dropping its remainder;
//! restart losing the committed replacement. See slice-6.md for the full matrix.

use luna_proto::{
    ACTIVITY_ENVELOPE_VERSION, ActivityEnvelope, ActivityKind, ActivityOutcome, ActivityRedaction,
    ActivitySourceRef, HarnessId, HistorySearchFilters, HistorySearchRequest, HistorySearchScope,
    ReasoningLevel,
};
use luna_sync::DocsStore;
use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    path::Path,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn event(task: &str, id: &str, sequence: u64) -> ActivityEnvelope {
    ActivityEnvelope {
        schema_version: ACTIVITY_ENVELOPE_VERSION,
        event_id: id.into(),
        environment_id: "fixture-environment".into(),
        device_id: "fixture-device".into(),
        task_id: task.into(),
        turn_id: "fixture-turn".into(),
        sequence,
        timestamp_ms: 1_700_000_000_000 + sequence as i64,
        kind: ActivityKind::Tool,
        outcome: ActivityOutcome::Succeeded,
        harness: Some(HarnessId::Codex),
        provider: Some("fixture-provider".into()),
        model: Some("fixture-model".into()),
        reasoning: Some(ReasoningLevel::High),
        project_id: Some("fixture-project".into()),
        workspace_id: Some("fixture-workspace".into()),
        tool: Some("Fixture Tool".into()),
        app: Some("Fixture App".into()),
        file: Some("fixture.txt".into()),
        command: Some("printf NEEDLE".into()),
        summary: format!("Mélange Σ {id}"),
        redaction: ActivityRedaction::None,
        source: ActivitySourceRef {
            task_id: task.into(),
            message_id: format!("message-{id}"),
            part_id: Some("part".into()),
            available_here: true,
            unavailable_reason: None,
        },
    }
}

fn snapshot(db: &Path, task: &str) -> Vec<ActivityEnvelope> {
    let conn = Connection::open(db).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    let mut statement = conn
        .prepare("SELECT envelope_json FROM activity_events WHERE task_id=?1 ORDER BY event_id")
        .unwrap();
    statement
        .query_map([task], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|row| serde_json::from_slice(&row.unwrap()).unwrap())
        .collect()
}

fn sorted(mut events: Vec<ActivityEnvelope>) -> Vec<ActivityEnvelope> {
    events.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    events
}

#[test]
fn activity_replacement_preserves_atomicity_search_and_restart() {
    let evidence = std::env::var_os("LUNA_PERF_EVIDENCE").map(std::path::PathBuf::from);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("docs.sqlite3");
    let store = Arc::new(DocsStore::open(dir.path()).unwrap());
    let unrelated = event("other", "untouched", 1);
    store
        .replace_activity_events("other", std::slice::from_ref(&unrelated))
        .unwrap();
    let mut workloads = Vec::new();
    for count in [1200_usize, 4200] {
        let events: Vec<_> = (0..count)
            .map(|i| event("task", &format!("event-{i:05}"), i as u64))
            .collect();
        store.replace_activity_events("task", &events).unwrap();
        assert_eq!(snapshot(&db, "task"), events);
        let barrier = Arc::new(Barrier::new(2));
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let store = store.clone();
            let events = events.clone();
            let barrier = barrier.clone();
            let done = done.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let started = Instant::now();
                for _ in 0..4 {
                    store.replace_activity_events("task", &events).unwrap();
                }
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                done.store(true, Ordering::Release);
                elapsed
            })
        };
        let observer = Connection::open(&db).unwrap();
        observer.busy_timeout(Duration::from_secs(5)).unwrap();
        let mut latencies = Vec::new();
        barrier.wait();
        loop {
            let started = Instant::now();
            let ids = store.activity_task_ids().unwrap();
            latencies.push(started.elapsed().as_secs_f64() * 1000.0);
            assert!(ids.iter().any(|id| id == "task") && ids.iter().any(|id| id == "other"));
            let rows: i64 = observer
                .query_row(
                    "SELECT COUNT(*) FROM activity_events WHERE task_id=?1",
                    params!["task"],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                rows, count as i64,
                "independent reader sees no partial transaction"
            );
            if done.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let write_ms = writer.join().unwrap();
        latencies.sort_by(f64::total_cmp);
        workloads.push(
            json!({"events":count,"replacements":4,"writeWallMs":write_ms,
            "readSamples":latencies.len(),"readP95Ms":latencies[((latencies.len()-1) as f64*0.95).round() as usize],
            "readMaxMs":latencies.last(),"atomicRows":true}),
        );
        assert_eq!(snapshot(&db, "task"), events);
        assert_eq!(snapshot(&db, "other"), vec![unrelated.clone()]);
    }

    let request = HistorySearchRequest {
        query: "needle".into(),
        scope: HistorySearchScope::SelectedProject,
        project_id: Some("fixture-project".into()),
        filters: HistorySearchFilters {
            device_ids: vec!["fixture-device".into()],
            harnesses: vec![HarnessId::Codex],
            models: vec!["fixture-model".into()],
            outcomes: vec![ActivityOutcome::Succeeded],
            tools: vec!["Fixture Tool".into()],
            apps: vec!["Fixture App".into()],
            after_ms: Some(1_700_000_000_100),
            before_ms: Some(1_700_000_004_000),
        },
        cursor: None,
        limit: 100,
        target_device_id: None,
    };
    let page = store.search_activity_events(&request).unwrap();
    assert_eq!(page.events.len(), 100);
    assert!(
        page.events
            .iter()
            .all(|e| e.harness == Some(HarnessId::Codex)
                && e.model.as_deref() == Some("fixture-model")
                && e.outcome == ActivityOutcome::Succeeded)
    );
    let before = snapshot(&db, "task");
    let duplicate = event("task", "collision", 9);
    assert!(
        store
            .replace_activity_events("task", &[duplicate.clone(), duplicate])
            .is_err()
    );
    assert_eq!(
        snapshot(&db, "task"),
        before,
        "failed replacement rolls back"
    );

    let transcript = event("task", "transcript", 1);
    let shot = event("task", "appshot:old", 2);
    store
        .replace_activity_events("task", &[transcript.clone(), shot])
        .unwrap();
    let new_shot = event("task", "appshot:new", 3);
    store
        .replace_appshot_activity_events("task", std::slice::from_ref(&new_shot))
        .unwrap();
    assert_eq!(
        snapshot(&db, "task"),
        sorted(vec![transcript.clone(), new_shot.clone()])
    );
    assert!(
        store
            .replace_appshot_activity_events("task", &[new_shot.clone(), new_shot])
            .is_err()
    );
    assert_eq!(snapshot(&db, "task").len(), 2);
    store.replace_appshot_activity_events("task", &[]).unwrap();
    assert_eq!(snapshot(&db, "task"), vec![transcript]);

    // Beyond both preparation ceilings. The large summaries are injected
    // at the persistence boundary to exercise fallback without a provider.
    let large: Vec<_> = (0..40)
        .map(|i| {
            let mut row = event("task", &format!("large-{i:03}"), i);
            row.summary = "x".repeat(1024 * 1024);
            row
        })
        .collect();
    store.replace_activity_events("task", &large).unwrap();
    assert_eq!(snapshot(&db, "task"), large);
    let mut invalid_large = large.clone();
    invalid_large.last_mut().unwrap().event_id = large[0].event_id.clone();
    assert!(
        store
            .replace_activity_events("task", &invalid_large)
            .is_err()
    );
    assert_eq!(snapshot(&db, "task"), large);
    drop(invalid_large);
    let many: Vec<_> = (0..8200)
        .map(|i| event("task", &format!("many-{i:05}"), i))
        .collect();
    store.replace_activity_events("task", &many).unwrap();
    assert_eq!(snapshot(&db, "task"), many);
    let mut invalid_many = many.clone();
    invalid_many.last_mut().unwrap().event_id = many[0].event_id.clone();
    assert!(
        store
            .replace_activity_events("task", &invalid_many)
            .is_err()
    );
    assert_eq!(snapshot(&db, "task"), many);
    store.replace_activity_events("task", &[]).unwrap();
    assert!(snapshot(&db, "task").is_empty());
    assert_eq!(snapshot(&db, "other"), vec![unrelated.clone()]);
    drop(store);
    let reopened = DocsStore::open(dir.path()).unwrap();
    assert_eq!(reopened.activity_task_ids().unwrap(), vec!["other"]);
    assert_eq!(snapshot(&db, "other"), vec![unrelated]);
    let result = json!({"passed":true,"workloads":workloads,"duplicateRollback":true,
        "appshotSubset":true,"largeBatchEvents":40,"largeSummaryBytes":40*1024*1024,
        "countFallbackEvents":8200,"exactPayloads":true,"unrelatedPreserved":true,"restart":true});
    if let Some(out) = evidence {
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            out.join("activity-index.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
    }
    println!("{result}");
}
