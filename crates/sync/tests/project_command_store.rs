//! Failure-first file-backed migration, backup-snapshot and attempt-fence acceptance.
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
        live.query_row(
            "SELECT count(*) FROM schema_migrations WHERE version=15",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1,
        "the project-command migration remains applied after later migrations"
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
    let mut setup = run.clone();
    setup.id = "setup-generation".into();
    setup.setup = true;
    setup.status = ProjectCommandStatus::Succeeded;
    assert!(store.save_project_command_run(&setup, None, None).unwrap());
    store
        .prepare_task_cleanup("device", "task", "fixture-boot")
        .unwrap();
    store.retire_task_local_records("device", "task").unwrap();
    let retained = store.project_command_runs("device").unwrap();
    assert_eq!(retained.len(), 1);
    assert!(
        retained[0].setup,
        "checkout setup replay guard survives task retirement"
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
