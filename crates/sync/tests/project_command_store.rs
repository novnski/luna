//! Failure-first file-backed migration, backup-snapshot and attempt-fence acceptance.
#[test]
fn newer_database_schema_is_rejected_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("docs.sqlite3");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL) STRICT; INSERT INTO schema_migrations VALUES(999,1); CREATE TABLE future_data(value TEXT); INSERT INTO future_data VALUES('preserve-me');").unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    let failure = match luna_sync::DocsStore::open(dir.path()) {
        Ok(_) => panic!("newer schema must fail closed"),
        Err(e) => e.to_string(),
    };
    assert!(
        failure.contains("999") && failure.contains("newer"),
        "{failure}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn migration_snapshot_ownership_and_attempt_fence() {
    use luna_proto::{ProjectAction, ProjectCommandRun, ProjectCommandStatus};
    let dir = tempfile::tempdir().unwrap();
    let store = luna_sync::DocsStore::open(dir.path()).unwrap();
    let action = ProjectAction {
        id: "build".into(),
        name: "Build".into(),
        command: "printf harmless".into(),
        setup_on_worktree_create: false,
        wait_for_setup: true,
        preview_url: None,
    };
    store
        .save_project_command_config("project", "device", &[action.clone()], None)
        .unwrap();
    assert!(
        store
            .project_command_config("project", "foreign-device")
            .unwrap()
            .is_empty()
    );
    let mut run = ProjectCommandRun {
        id: "run".into(),
        project_id: "project".into(),
        device_id: "device".into(),
        chat_id: "task".into(),
        cwd: "/fixture".into(),
        checkout_generation: "generation1".into(),
        action,
        action_digest: "digest".into(),
        setup: false,
        attempt: 1,
        status: ProjectCommandStatus::Pending,
        terminal_id: None,
        previous_terminal_ids: vec![],
        process_group_id: None,
        exit_code: None,
        detail: None,
        updated_at_ms: 1,
    };
    assert!(store.save_project_command_run(&run, None, None).unwrap());
    assert!(!store.save_project_command_run(&run, None, None).unwrap());
    run.attempt = 2;
    run.status = ProjectCommandStatus::Running;
    assert!(store.save_project_command_run(&run, Some(1), None).unwrap());
    let mut stale = run.clone();
    stale.attempt = 1;
    stale.status = ProjectCommandStatus::Succeeded;
    assert!(
        !store
            .save_project_command_run(&stale, Some(1), None)
            .unwrap()
    );
    assert_eq!(store.project_command_runs("device").unwrap()[0].attempt, 2);
    assert!(
        store
            .project_command_runs("foreign-device")
            .unwrap()
            .is_empty()
    );
    let live = rusqlite::Connection::open(dir.path().join("docs.sqlite3")).unwrap();
    assert_eq!(
        live.query_row("SELECT max(version) FROM schema_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        14
    );
    let copy = dir.path().join("snapshot.sqlite3");
    live.execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
        .unwrap();
    let snapshot = rusqlite::Connection::open(copy).unwrap();
    assert_eq!(
        snapshot
            .query_row("SELECT count(*) FROM project_command_configs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        snapshot
            .query_row("SELECT count(*) FROM project_command_runs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    store.retire_project_local_records("project").unwrap();
    assert!(
        store
            .project_command_config("project", "device")
            .unwrap()
            .is_empty()
    );
    assert!(store.project_command_runs("device").unwrap().is_empty());
}
