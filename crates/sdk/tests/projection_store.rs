//! RM-03 client-side refusals of `luna_sdk::ProjectionStore`, the shared
//! client runtime state every RM-03 client applies host projections to.
//!
//! The end-to-end owner (`luna-engine`'s `remote_access` test
//! `gateway::mobile_runtime_follows_mac_tasks_...`) drives a real host,
//! which never sends these sequences. This table owns what a client must do
//! when one arrives anyway (a late item from an old socket, a gap, another
//! host, an unbounded or malformed projection, a foreign or damaged saved
//! cache): refuse it and keep its state. Failure contract:
//! docs/pending-tasks/control-this-mac/rm-03/README.md (C1–C12).

use luna_proto::WorkspaceScope;
use luna_proto::remote_projection::{
    MAX_REGISTRY_TASKS, ProjectionCurrent, ProjectionCursor, ProjectionHost, RemoteHistoryEntry,
    RemoteTaskDelta, RemoteTaskDetail, RemoteTaskItem, RemoteTaskRegistry, RemoteTaskRow,
    RemoteTaskStatus, RemoteTasksDelta, RemoteTasksItem,
};
use luna_sdk::{
    CacheError, Freshness, HostKey, MAX_CACHE_BYTES, MAX_CACHED_TASKS, ProjectionError,
    ProjectionStore,
};

const GENERATION: &str = "generation-1";

fn key() -> HostKey {
    HostKey {
        device_id: "mac".into(),
        environment_id: "profile".into(),
        workspace_scope: WorkspaceScope::Local,
    }
}

fn host() -> ProjectionHost {
    ProjectionHost {
        device_id: "mac".into(),
        environment_id: Some("profile".into()),
        workspace_scope: WorkspaceScope::Local,
        profile_id: None,
        generation: GENERATION.into(),
        engine_version: "test".into(),
        capabilities: vec![],
    }
}

fn cursor(revision: u64) -> ProjectionCursor {
    ProjectionCursor {
        generation: GENERATION.into(),
        revision,
    }
}

fn row(id: &str) -> RemoteTaskRow {
    RemoteTaskRow {
        task_id: id.into(),
        project_id: "project".into(),
        title: Some(id.into()),
        status: RemoteTaskStatus::Idle,
        archived: false,
        pinned: false,
        unseen: false,
        provider: None,
        created_at_ms: 1,
        last_activity_ms: None,
        preview: None,
    }
}

fn snapshot(revision: u64, ids: &[&str]) -> RemoteTasksItem {
    RemoteTasksItem::Snapshot {
        registry: RemoteTaskRegistry {
            host: host(),
            cursor: cursor(revision),
            observed_at_ms: 1,
            projects: vec![],
            tasks: ids.iter().map(|id| row(id)).collect(),
            total_tasks: ids.len() as u32,
            providers: vec![],
        },
    }
}

fn delta(
    base: u64,
    next: u64,
    upsert: &[&str],
    remove: &[&str],
    order: Option<&[&str]>,
) -> RemoteTasksItem {
    RemoteTasksItem::Delta {
        delta: RemoteTasksDelta {
            base: cursor(base),
            cursor: cursor(next),
            observed_at_ms: 2,
            projects: None,
            upsert: upsert.iter().map(|id| row(id)).collect(),
            remove: remove.iter().map(|id| (*id).to_owned()).collect(),
            order: order.map(|order| order.iter().map(|id| (*id).to_owned()).collect()),
            total_tasks: 9,
        },
    }
}

fn current(revision: u64) -> RemoteTasksItem {
    RemoteTasksItem::Current {
        current: ProjectionCurrent {
            host: host(),
            cursor: cursor(revision),
            observed_at_ms: 3,
        },
    }
}

fn entry(id: &str) -> RemoteHistoryEntry {
    RemoteHistoryEntry {
        entry_id: id.into(),
        role: "user".into(),
        status: None,
        created_at_ms: 1,
        text: id.into(),
        text_truncated: false,
        tools: vec![],
        other_parts: 0,
        error: false,
    }
}

fn detail(task_id: &str, revision: u64) -> RemoteTaskItem {
    RemoteTaskItem::Snapshot {
        detail: RemoteTaskDetail {
            host: host(),
            cursor: cursor(revision),
            observed_at_ms: 1,
            task: row(task_id),
            task_revision: 1,
            task_sequence: 1,
            entries: vec![entry("e1")],
            total_entries: 1,
        },
    }
}

/// A store on connection 1 holding the registry `[a, b]` at revision 5.
fn held() -> ProjectionStore {
    let mut store = ProjectionStore::new(key());
    store.begin_epoch(1);
    store.apply_tasks(1, snapshot(5, &["a", "b"]), 10).unwrap();
    store
}

fn ids(store: &ProjectionStore) -> Vec<String> {
    store
        .registry()
        .map(|view| view.tasks.iter().map(|row| row.task_id.clone()).collect())
        .unwrap_or_default()
}

#[test]
fn out_of_place_registry_items_are_refused_and_change_nothing() {
    let mut other_device = host();
    other_device.device_id = "another-mac".into();
    let mut other_profile = host();
    other_profile.environment_id = Some("another-profile".into());
    let mut other_scope = host();
    other_scope.workspace_scope = WorkspaceScope::Synced;
    let foreign = |host: ProjectionHost| match snapshot(9, &["x"]) {
        RemoteTasksItem::Snapshot { mut registry } => {
            registry.host = host;
            RemoteTasksItem::Snapshot { registry }
        }
        item => item,
    };
    let too_many: Vec<String> = (0..=MAX_REGISTRY_TASKS).map(|n| format!("t{n}")).collect();
    let too_many: Vec<&str> = too_many.iter().map(String::as_str).collect();
    type Case = (
        &'static str,
        u64,
        RemoteTasksItem,
        fn(&ProjectionError) -> bool,
    );
    let stale: fn(&ProjectionError) -> bool = |e| matches!(e, ProjectionError::StaleGeneration);
    let wrong: fn(&ProjectionError) -> bool = |e| matches!(e, ProjectionError::WrongHost(_));
    let order: fn(&ProjectionError) -> bool = |e| matches!(e, ProjectionError::OutOfOrder(_));
    let bad: fn(&ProjectionError) -> bool = |e| matches!(e, ProjectionError::Invalid(_));
    let cases: Vec<Case> = vec![
        // C1: a late item from the previous socket.
        (
            "late item from an old socket",
            0,
            snapshot(9, &["x"]),
            stale,
        ),
        (
            "delta from an old socket",
            0,
            delta(5, 6, &["a"], &[], None),
            stale,
        ),
        // C2: another host, profile or scope behind a verified socket.
        ("another device", 1, foreign(other_device), wrong),
        ("another profile", 1, foreign(other_profile), wrong),
        ("another scope", 1, foreign(other_scope), wrong),
        // C3: a gap, a duplicate or a cursor the client never held.
        ("gap", 1, delta(6, 7, &["a"], &[], None), order),
        ("duplicate", 1, delta(4, 5, &["a"], &[], None), order),
        ("current for another cursor", 1, current(4), order),
        (
            "delta that does not move forward",
            1,
            delta(5, 5, &["a"], &[], None),
            order,
        ),
        // C4: rows added or removed without the order that places them.
        (
            "added without order",
            1,
            delta(5, 6, &["c"], &[], None),
            bad,
        ),
        (
            "removed without order",
            1,
            delta(5, 6, &[], &["a"], None),
            bad,
        ),
        (
            "order names an unknown row",
            1,
            delta(5, 6, &[], &[], Some(&["a", "z"])),
            bad,
        ),
        (
            "order misses a row",
            1,
            delta(5, 6, &[], &[], Some(&["a"])),
            bad,
        ),
        (
            "changed and removed",
            1,
            delta(5, 6, &["a"], &["a"], Some(&["b"])),
            bad,
        ),
        // C5: outside the projection bounds.
        ("more rows than the bound", 1, snapshot(9, &too_many), bad),
        ("repeated row", 1, snapshot(9, &["a", "a"]), bad),
    ];
    for (name, epoch, item, expected) in cases {
        let mut store = held();
        let error = store.apply_tasks(epoch, item, 20).expect_err(name);
        assert!(expected(&error), "{name}: {error:?}");
        assert_eq!(ids(&store), ["a", "b"], "{name} changed the held registry");
        assert_eq!(store.registry_cursor(), Some(cursor(5)), "{name}");
    }
}

#[test]
fn a_new_connection_confirms_before_it_changes_and_ended_ones_are_stale() {
    let mut store = held();
    store.end_epoch();
    // C6: with no connection nothing applies, and live turns stale.
    assert_eq!(
        store.apply_tasks(1, current(5), 20),
        Err(ProjectionError::StaleGeneration)
    );
    assert_eq!(store.registry().unwrap().freshness, Freshness::Stale);
    // C7: a new connection's first delta before its snapshot or `current`.
    store.begin_epoch(2);
    assert!(matches!(
        store.apply_tasks(2, delta(5, 6, &["a"], &[], None), 20),
        Err(ProjectionError::OutOfOrder(_))
    ));
    assert_eq!(store.registry().unwrap().freshness, Freshness::Stale);
    let confirmed = store.apply_tasks(2, current(5), 30).unwrap();
    assert_eq!(confirmed.freshness, Freshness::Live);
    assert_eq!(confirmed.confirmed_at_ms, 30);
    let moved = store
        .apply_tasks(2, delta(5, 6, &["c"], &["a"], Some(&["c", "b"])), 40)
        .unwrap();
    assert_eq!(ids(&store), ["c", "b"]);
    assert_eq!(moved.cursor, cursor(6));
}

#[test]
fn task_details_refuse_another_task_and_stay_bounded() {
    let mut store = ProjectionStore::new(key());
    store.begin_epoch(1);
    // C8: a detail delivered on another task's watch.
    assert!(matches!(
        store.apply_task(1, "a", detail("b", 1), 1),
        Err(ProjectionError::Invalid(_))
    ));
    // C9: a delta for a task never confirmed, then for a removed one.
    let step = RemoteTaskItem::Delta {
        delta: RemoteTaskDelta {
            base: cursor(1),
            cursor: cursor(2),
            observed_at_ms: 2,
            task: None,
            task_revision: 1,
            task_sequence: 1,
            upsert: vec![],
            remove: vec![],
            order: None,
            total_entries: 1,
        },
    };
    assert!(matches!(
        store.apply_task(1, "a", step.clone(), 1),
        Err(ProjectionError::OutOfOrder(_))
    ));
    store.apply_task(1, "a", detail("a", 1), 1).unwrap();
    store.release_task("a");
    assert_eq!(store.task("a").unwrap().freshness, Freshness::Stale);
    assert!(matches!(
        store.apply_task(1, "a", step, 1),
        Err(ProjectionError::OutOfOrder(_))
    ));
    // C10: at most MAX_CACHED_TASKS details; the least recent are dropped.
    for n in 0..=MAX_CACHED_TASKS {
        let id = format!("task-{n}");
        store.apply_task(1, &id, detail(&id, 1), 1).unwrap();
    }
    assert!(store.task("a").is_none() && store.task("task-0").is_none());
    assert!(store.task("task-1").is_some());
    assert!(store.task(&format!("task-{MAX_CACHED_TASKS}")).is_some());
}

#[test]
fn a_saved_cache_restores_only_for_its_host_and_never_as_live() {
    let store = held();
    let bytes = store.encode(50);
    // C11: restored views are cached, with the cursor to resume from.
    let restored = ProjectionStore::decode(&bytes, &key()).unwrap();
    let view = restored.registry().unwrap();
    assert_eq!(view.freshness, Freshness::Cached);
    assert_eq!(restored.registry_cursor(), Some(cursor(5)));
    // C12: another host's, a damaged or an oversized cache is refused.
    let mut other = key();
    other.environment_id = "another-profile".into();
    assert_eq!(
        ProjectionStore::decode(&bytes, &other).err(),
        Some(CacheError::OtherHost)
    );
    assert!(matches!(
        ProjectionStore::decode(&bytes[..bytes.len() / 2], &key()),
        Err(CacheError::Unreadable(_))
    ));
    assert_eq!(
        ProjectionStore::decode(&vec![b' '; MAX_CACHE_BYTES + 1], &key()).err(),
        Some(CacheError::TooLarge)
    );
    let mut tampered: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    tampered["registry"]["host"]["deviceId"] = "another-mac".into();
    assert_eq!(
        ProjectionStore::decode(&serde_json::to_vec(&tampered).unwrap(), &key()).err(),
        Some(CacheError::OtherHost)
    );
}
