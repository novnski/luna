//! L17 tests use only fresh synthetic roots and an injected fake key.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use luna_doc::{
    MessagePart, MessageRole, MessageStatus, RegistryDoc, SessionDoc, SessionMessageEntry,
};
use luna_engine::backup::{BackupError, BackupService, IsolatedRoot, RecoveryKey, inspect_archive};
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry, InstanceLock};
use luna_proto::HarnessId;
use luna_proto::{AppshotActivityKind, AppshotActivityRecord, Chat, Space};
use luna_sync::DocsStore;
use rusqlite::{Connection, params};
use tempfile::TempDir;

fn fixture() -> (TempDir, PathBuf, PathBuf) {
    let parent = TempDir::new().unwrap();
    let root = parent.path().join("synthetic-luna");
    let backups = parent.path().join("backups");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&backups).unwrap();
    fs::write(
        backups.join(".luna-l17-isolated-backups"),
        "L17 synthetic or copy-isolated backup destination\n",
    )
    .unwrap();
    fs::write(
        root.join(".luna-l17-isolated-root"),
        "L17 synthetic or copy-isolated root\n",
    )
    .unwrap();
    fs::write(root.join("ui-settings.json"), r#"{"theme":"synthetic"}"#).unwrap();
    fs::write(root.join("device-id"), "synthetic-device").unwrap();
    let profile = root.join("profiles/local");
    let store = DocsStore::open(&profile).unwrap();
    store
        .save_snapshot("synthetic-doc", b"synthetic source")
        .unwrap();
    drop(store);
    fs::create_dir_all(profile.join("uploads")).unwrap();
    let attachment = profile.join("uploads/synthetic.txt");
    fs::write(&attachment, b"synthetic attachment").unwrap();
    let conn = Connection::open(profile.join("docs.sqlite3")).unwrap();
    conn.execute(
        "INSERT INTO attachment_owners(device_id,blob_path,owner_kind,owner_id,created_at)
         VALUES (?1,?2,'draft','synthetic-draft',1)",
        params!["synthetic-device", attachment.to_string_lossy().as_ref()],
    )
    .unwrap();
    drop(conn);
    (parent, root, backups)
}

fn service(root: &Path) -> BackupService {
    BackupService::new(
        IsolatedRoot::open(root).unwrap(),
        RecoveryKey::injected([17; 32]),
    )
}

/// Failure cases recorded in the 2026-10-08 history/platform audit before
/// implementation: current schema rejected; definition tombstones, mutation
/// receipts or active occurrence claims lost during archive/restore.
#[test]
fn automation_tables_survive_encrypted_archive_and_activation() {
    let (_parent, root, backups) = fixture();
    let database = root.join("profiles/local/docs.sqlite3");
    let definition = br#"{"id":"backup-auto","enabled":false,"deleted":true}"#;
    let receipt = br#"{"requestId":"backup-mutation","payloadHash":"frozen"}"#;
    let occurrence = br#"{"id":"backup-run","state":"unknown","bootId":"old-boot"}"#;
    let conn = Connection::open(&database).unwrap();
    conn.execute("INSERT INTO automations(id,owner_host_id,revision,enabled,deleted,next_due_ms,origin_task_id,record) VALUES('backup-auto','synthetic-device',2,0,1,NULL,'origin',?1)", [definition.as_slice()]).unwrap();
    conn.execute("INSERT INTO automation_mutations(request_id,automation_id,payload_hash,committed_at_ms,receipt) VALUES('backup-mutation','backup-auto','frozen',42,?1)", [receipt.as_slice()]).unwrap();
    conn.execute("INSERT INTO automation_runs(id,automation_id,owner_host_id,scheduled_at_ms,task_id,message_id,blocks_next,state,record) VALUES('backup-run','backup-auto','synthetic-device',42,'origin','occurrence-message',1,'\"unknown\"',?1)", [occurrence.as_slice()]).unwrap();
    drop(conn);
    let backup = service(&root)
        .export(&backups.join("automations.lunabk"))
        .unwrap();
    assert_eq!(backup.manifest.databases[0].schema_version, 19);
    let conn = Connection::open(&database).unwrap();
    conn.execute_batch(
        "DELETE FROM automations; DELETE FROM automation_mutations; DELETE FROM automation_runs;",
    )
    .unwrap();
    drop(conn);
    let staged = service(&root).stage_restore(&backup.archive).unwrap();
    service(&root).verify_staged_for_tests(&staged).unwrap();
    service(&root).activate(staged).unwrap();
    let restored = Connection::open(&database).unwrap();
    let (enabled, deleted, record): (bool, bool, Vec<u8>) = restored
        .query_row(
            "SELECT enabled,deleted,record FROM automations WHERE id='backup-auto'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(!enabled && deleted);
    assert_eq!(record, definition);
    let saved_receipt: Vec<u8> = restored
        .query_row(
            "SELECT receipt FROM automation_mutations WHERE request_id='backup-mutation'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved_receipt, receipt);
    let (blocks_next, state, saved_run): (bool, String, Vec<u8>) = restored
        .query_row(
            "SELECT blocks_next,state,record FROM automation_runs WHERE id='backup-run'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(blocks_next);
    assert_eq!(state, "\"unknown\"");
    assert_eq!(saved_run, occurrence);
    if let Ok(dir) = std::env::var("LUNA_HISTORY_PLATFORM_EVIDENCE_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            Path::new(&dir).join("automation-backup-roundtrip.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schemaVersion":18, "definitionBytesRestored":true, "deleted":deleted,
                "enabled":enabled, "mutationReceiptBytesRestored":true,
                "occurrenceBytesRestored":true, "occurrenceBlocksNext":blocks_next,
            }))
            .unwrap(),
        )
        .unwrap();
    }
}

/// Full encrypted archive -> staged verification -> activation with a saved
/// tab tree. Failure cases precede code in the chat-workbench design brief.
#[test]
fn workbench_layout_archive_roundtrip_preserves_ids_ratios_and_closed_history() {
    use luna_proto::{
        ChatWorkbenchCommand, ChatWorkbenchSnapshot, ChatWorkbenchTab, WorkbenchDestination,
        WorkbenchEdge,
    };
    use sha2::{Digest, Sha256};
    let (_parent, root, backups) = fixture();
    let main = "workbench-backup-main";
    let mut layout = ChatWorkbenchSnapshot::new(main);
    layout
        .apply(&ChatWorkbenchCommand::OpenTab {
            tab: ChatWorkbenchTab::chat("child-backup"),
            destination: Some(WorkbenchDestination::Split {
                node_id: "main-pane".into(),
                edge: WorkbenchEdge::Right,
                new_pane_id: "right".into(),
                split_id: "split".into(),
            }),
        })
        .unwrap();
    layout
        .apply(&ChatWorkbenchCommand::ResizeSplit {
            split_id: "split".into(),
            ratio: 0.63,
        })
        .unwrap();
    layout
        .apply(&ChatWorkbenchCommand::OpenTab {
            tab: ChatWorkbenchTab::chat("closed-backup"),
            destination: None,
        })
        .unwrap();
    layout
        .apply(&ChatWorkbenchCommand::CloseTab {
            chat_id: "closed-backup".into(),
        })
        .unwrap();
    layout.revision = 4;
    let folder = root.join("profiles/local/chat-workbenches-v1");
    fs::create_dir_all(&folder).unwrap();
    let file = folder.join(format!("{:x}.json", Sha256::digest(main.as_bytes())));
    let bytes = serde_json::to_vec(&serde_json::json!({"snapshot":layout,"receipts":[]})).unwrap();
    fs::write(&file, &bytes).unwrap();
    let temporary = folder.join(".pending-workbench-uncommitted");
    fs::write(&temporary, b"unfinished").unwrap();
    let receipt = service(&root)
        .export(&backups.join("workbench.lunabk"))
        .unwrap();
    assert_eq!(
        receipt
            .manifest
            .entries
            .iter()
            .filter(|entry| entry.category == "chat-workbench")
            .count(),
        1
    );
    assert!(
        !receipt
            .manifest
            .entries
            .iter()
            .any(|entry| entry.path.contains("pending-workbench"))
    );
    fs::remove_file(temporary).unwrap();
    fs::write(&file, b"changed after backup").unwrap();
    let staged = service(&root).stage_restore(&receipt.archive).unwrap();
    service(&root).verify_staged_for_tests(&staged).unwrap();
    service(&root).activate(staged).unwrap();
    assert_eq!(fs::read(&file).unwrap(), bytes);
    if let Ok(dir) = std::env::var("WORKBENCH_EVIDENCE_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(Path::new(&dir).join("layout-backup-roundtrip.json"),serde_json::to_vec_pretty(&serde_json::json!({"manifest":receipt.manifest,"restoredLayout":layout,"exactBytesRestored":true,"temporaryFileExcluded":true})).unwrap()).unwrap();
        fs::copy(
            receipt.archive,
            Path::new(&dir).join("synthetic-layout.lunabk"),
        )
        .unwrap();
    }
}

#[test]
fn sqlite_backup_restore_preserves_workspace_w_and_older_execution_lineage() {
    use luna_proto::{
        RecoveryAcceptance, RecoveryAction, RecoveryFailure, RecoveryStatus, TaskRecovery,
    };
    let (_parent, root, backups) = fixture();
    let task = "workspace-authority-backup";
    let profile = root.join("profiles/local");
    let view = TaskRecovery {
        chain_id: "old-chain".into(),
        task_id: task.into(),
        original_message_id: "old-message".into(),
        original_command_id: None,
        original_acceptance: RecoveryAcceptance::Unknown,
        acceptance: RecoveryAcceptance::Unknown,
        failure: RecoveryFailure::Transport,
        action: RecoveryAction::ObserveOnly,
        status: RecoveryStatus::Held,
        attempt_count: 0,
        max_attempts: 4,
        automatic: false,
        retry_at_ms: None,
        automatic_deadline_ms: i64::MAX,
        environment_id: "synthetic-device".into(),
        binding_id: None,
        binding_generation: None,
        detail: "Preserved older workspace lineage".into(),
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    let request = luna_proto::RunRequest {
        review: None,
        goal: None,
        input_items: Vec::new(),
        prompt: "old recovery".into(),
        harness: Some(HarnessId::Mock),
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: "/synthetic".into(),
        sandbox: luna_proto::SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: Default::default(),
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: vec![],
        worktree: None,
    };
    let recovery = serde_json::to_vec(&serde_json::json!({
        "workspaceRepairRevision":0, "view":view, "harness":"mock", "request":request,
        "originalAcceptance":"unknown", "attemptAutomatic":false,
    }))
    .unwrap();
    let sql = Connection::open(profile.join("docs.sqlite3")).unwrap();
    sql.execute("INSERT INTO task_command_state(task_id,revision,workspace_repair_revision) VALUES (?1,9,7)", [task]).unwrap();
    sql.execute("INSERT INTO local_executable_authority(doc_id,kind,row_id,payload_hash,workspace_repair_revision) VALUES (?1,'queue','old-row','old-hash',0)", [task]).unwrap();
    let configuration = br#"{"config":null,"cwd":"/tmp"}"#;
    sql.execute("INSERT INTO task_execution_lineage(task_id,message_id,workspace_repair_revision,configuration) VALUES (?1,'old-message',0,?2)", params![task, configuration.as_slice()]).unwrap();
    sql.execute("INSERT INTO task_recovery_state(task_id,chain_id,attempt_count,status,record_json,updated_at) VALUES (?1,'old-chain',0,'held',?2,1)", params![task,recovery]).unwrap();
    drop(sql);
    let backup = service(&root)
        .backup_now(&backups, 1_700_000_000_000)
        .unwrap();
    assert_eq!(backup.manifest.databases[0].schema_version, 19);
    let staged = service(&root).stage_restore(&backup.archive).unwrap();
    service(&root).verify_staged_for_tests(&staged).unwrap();
    service(&root).activate(staged).unwrap();
    let restored = DocsStore::open(&profile).unwrap();
    assert_eq!(restored.workspace_repair_revision(task).unwrap(), 7);
    assert_eq!(
        restored
            .execution_workspace_repair_revision(task, "old-message")
            .unwrap(),
        Some(0)
    );
    assert_eq!(
        restored
            .execution_configuration(task, "old-message")
            .unwrap(),
        Some(configuration.to_vec())
    );
    assert!(
        !restored
            .local_executable_matches(
                task,
                luna_sync::LocalExecutableKind::Queue,
                "old-row",
                "old-hash"
            )
            .unwrap()
    );
    assert_eq!(
        restored.task_recovery(task).unwrap().unwrap().record_json,
        recovery
    );
    if let Some(dir) = std::env::var_os("LUNA_WORKSPACE_AUTHORITY_REPORT_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            PathBuf::from(dir).join("backup-restore-workspace-authority.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"schema":17,"restoredW":7,
                "grantW":0,"recoveryW":0,"messageW":0,"oldGrantExecutable":false,
                "fullProfileRollbackProtectionClaimed":false}))
            .unwrap(),
        )
        .unwrap();
    }
    // This preserves the W captured IN the archive. It makes no claim about
    // detecting a restore that deliberately rolls back W and grants together.
}

#[test]
fn bb_style_archive_is_gzip_manifest_verified_and_staged_without_a_key() {
    let (_parent, root, backups) = fixture();
    let backup = BackupService::new_bb_style(IsolatedRoot::open(&root).unwrap());
    let receipt = backup.backup_now(&backups, 1_700_000_000_000).unwrap();
    assert_eq!(receipt.manifest.format_version, 3);
    assert_eq!(receipt.archive.extension().unwrap(), "gz");
    let mut prefix = [0u8; 2];
    fs::File::open(&receipt.archive)
        .unwrap()
        .read_exact(&mut prefix)
        .unwrap();
    assert_eq!(prefix, [0x1f, 0x8b]);
    let staged = backup.stage_restore(&receipt.archive).unwrap();
    backup.verify_staged_for_tests(&staged).unwrap();
    assert_eq!(staged.preview.manifest, receipt.manifest);
}

#[test]
fn managed_offline_daily_backup_requires_stopped_engine_and_private_sibling() {
    let (parent, root, _backups) = fixture();
    fs::remove_file(root.join(".luna-l17-isolated-root")).unwrap();
    let lock = InstanceLock::acquire(&root).unwrap();
    assert!(matches!(
        BackupService::run_managed_offline_if_due(&root, 1_700_000_000_000),
        Err(BackupError::EngineRunning)
    ));
    drop(lock);
    let first = BackupService::run_managed_offline_if_due(&root, 1_700_000_000_000)
        .unwrap()
        .unwrap();
    let destination = parent
        .path()
        .canonicalize()
        .unwrap()
        .join("synthetic-luna.backups");
    assert!(first.archive.starts_with(&destination));
    assert!(first.archive.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&first.archive).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(
        BackupService::run_managed_offline_if_due(&root, 1_700_000_000_001)
            .unwrap()
            .is_none()
    );
    fs::write(root.join("ui-settings.json"), b"synthetic changed").unwrap();
    let second = BackupService::run_managed_offline_if_due(&root, 1_700_086_400_001)
        .unwrap()
        .unwrap();
    assert_ne!(first.archive_sha256, second.archive_sha256);
    assert_eq!(
        second.manifest.previous_backup_sha256,
        Some(first.archive_sha256)
    );
}

#[test]
fn managed_offline_restore_preserves_excluded_files_and_keeps_rollback() {
    let (_parent, root, _backups) = fixture();
    fs::remove_file(root.join(".luna-l17-isolated-root")).unwrap();
    fs::create_dir(root.join("credentials")).unwrap();
    fs::write(root.join("credentials/synthetic-only.txt"), b"do not touch").unwrap();
    let remote_grants = root.join("profiles/local/remote-auth.json");
    fs::write(&remote_grants, b"synthetic remote grant excluded").unwrap();
    let db = root.join("profiles/local/docs.sqlite3");
    let conn = Connection::open(&db).unwrap();
    conn.execute(
        "INSERT INTO task_cleanup(task_id,state,detail,updated_at,owner_boot_id)
         VALUES ('synthetic-task','failed','synthetic checkpoint',1,'synthetic-boot')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO local_executable_authority(doc_id,kind,row_id,payload_hash)
         VALUES ('synthetic-doc','command','synthetic-row','synthetic-hash')",
        [],
    )
    .unwrap();
    drop(conn);
    let receipt = BackupService::run_managed_offline_now(&root, 1_700_000_000_000).unwrap();
    let claims = &receipt.manifest.databases[0];
    assert_eq!(claims.cleanup_tasks, Some(1));
    assert_eq!(claims.cleanup_projects, Some(0));
    assert_eq!(claims.cleanup_paths, Some(0));
    assert_eq!(claims.local_executable_authority, Some(1));
    assert!(
        !receipt
            .manifest
            .entries
            .iter()
            .any(|entry| entry.path.ends_with("remote-auth.json"))
    );
    let preview = BackupService::preview_managed_offline(&root, &receipt.archive).unwrap();
    assert_eq!(preview.manifest, receipt.manifest);
    assert!(preview.required_bytes > 0);
    assert!(preview.unsupported.is_empty(), "{:?}", preview.unsupported);
    assert!(preview.manifest.databases.len() > 0);
    assert!(
        preview
            .manifest
            .entries
            .iter()
            .any(|entry| entry.category == "attachment")
    );
    let copied_root = root.with_file_name("copied-luna");
    fs::create_dir(&copied_root).unwrap();
    fs::write(copied_root.join("device-id"), "synthetic-device").unwrap();
    let copied_preview =
        BackupService::preview_managed_offline(&copied_root, &receipt.archive).unwrap();
    assert!(
        copied_preview
            .unsupported
            .iter()
            .any(|reason| reason.contains("original data-root path"))
    );
    fs::write(root.join("ui-settings.json"), b"synthetic newer settings").unwrap();
    let journals = root.join("profiles/local/journals");
    fs::create_dir_all(&journals).unwrap();
    fs::write(journals.join("new.jsonl"), b"synthetic newer journal\n").unwrap();
    let lock = InstanceLock::acquire(&root).unwrap();
    assert!(matches!(
        BackupService::restore_managed_offline(&root, &receipt.archive),
        Err(BackupError::EngineRunning)
    ));
    assert!(matches!(
        BackupService::preview_managed_offline(&root, &receipt.archive),
        Err(BackupError::EngineRunning)
    ));
    drop(lock);
    let restored = BackupService::restore_managed_offline(&root, &receipt.archive).unwrap();
    assert!(restored.restored_entries > 0);
    let conn = Connection::open(&db).unwrap();
    for table in ["task_cleanup", "local_executable_authority"] {
        let count: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "{table} survived restore");
    }
    drop(conn);
    assert_eq!(
        fs::read(root.join("ui-settings.json")).unwrap(),
        br#"{"theme":"synthetic"}"#
    );
    assert!(!journals.join("new.jsonl").exists());
    assert_eq!(
        fs::read(root.join("credentials/synthetic-only.txt")).unwrap(),
        b"do not touch"
    );
    assert_eq!(
        fs::read(&remote_grants).unwrap(),
        b"synthetic remote grant excluded"
    );
    assert_eq!(
        fs::read(restored.rollback_dir.join("ui-settings.json")).unwrap(),
        b"synthetic newer settings"
    );
    assert_eq!(
        fs::read(
            restored
                .rollback_dir
                .join("profiles/local/journals/new.jsonl")
        )
        .unwrap(),
        b"synthetic newer journal\n"
    );
    assert!(!BackupService::recover_managed_restore(&root).unwrap());
}

#[test]
fn managed_offline_backup_refuses_unreviewed_future_schema() {
    let (_parent, root, _backups) = fixture();
    fs::remove_file(root.join(".luna-l17-isolated-root")).unwrap();
    let conn = Connection::open(root.join("profiles/local/docs.sqlite3")).unwrap();
    conn.execute(
        "INSERT INTO schema_migrations(version,applied_at)
         SELECT COALESCE(MAX(version),0)+1,1 FROM schema_migrations",
        [],
    )
    .unwrap();
    drop(conn);
    let result = BackupService::run_managed_offline_now(&root, 1_700_000_000_000);
    assert!(
        matches!(result, Err(BackupError::Unsupported(message)) if message.contains("newer than supported"))
    );
}

#[test]
fn managed_offline_backup_refuses_missing_required_tables() {
    for table in [
        "local_executable_authority",
        "project_command_configs",
        "project_command_runs",
        "side_chats",
        "side_chat_entries",
        "side_chat_sends",
        "task_execution_lineage",
        "automations",
        "automation_mutations",
        "automation_runs",
        "board_projects",
        "board_items",
        "board_mutations",
        "board_events",
        "board_task_links",
        "board_issue_refs",
        "board_launches",
        "board_outcomes",
        "board_intents",
        "board_review_runs",
        "board_authoring_sessions",
        "board_authoring_entries",
        "board_dependencies",
    ] {
        let (_parent, root, _backups) = fixture();
        fs::remove_file(root.join(".luna-l17-isolated-root")).unwrap();
        let conn = Connection::open(root.join("profiles/local/docs.sqlite3")).unwrap();
        conn.execute(&format!("DROP TABLE {table}"), []).unwrap();
        drop(conn);
        let result = BackupService::run_managed_offline_now(&root, 1_700_000_000_000);
        assert!(
            matches!(result, Err(BackupError::Invalid(message)) if message.contains(&format!("missing table {table}"))),
            "missing {table} was not refused"
        );
    }
}

#[test]
fn backup_refuses_missing_workspace_authority_and_recovery_columns() {
    for (table, column) in [
        ("task_command_state", "workspace_repair_revision"),
        ("task_command_receipts", "workspace_repair_revision"),
        ("local_executable_authority", "workspace_repair_revision"),
        ("task_execution_lineage", "workspace_repair_revision"),
        ("task_execution_lineage", "configuration"),
    ] {
        let (_parent, root, backups) = fixture();
        let sql = Connection::open(root.join("profiles/local/docs.sqlite3")).unwrap();
        sql.execute(&format!("ALTER TABLE {table} DROP COLUMN {column}"), [])
            .unwrap();
        drop(sql);
        assert!(
            matches!(service(&root).backup_now(&backups, 1_700_000_000_000),
            Err(BackupError::Invalid(message)) if message.contains(column)),
            "{table}.{column}"
        );
    }
}

#[test]
fn managed_offline_v9_archive_remains_restorable_after_current_migrations() {
    let (_parent, root, _backups) = fixture();
    fs::remove_file(root.join(".luna-l17-isolated-root")).unwrap();
    let db = root.join("profiles/local/docs.sqlite3");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "DROP TABLE prompt_stash;
         DROP TABLE local_executable_authority;
         DROP TABLE task_cleanup_paths;
         DROP TABLE task_cleanup;
         DROP TABLE project_cleanup;
         DROP TABLE project_command_configs;
         DROP TABLE project_command_runs;
         DROP TABLE side_chat_entries;
         DROP TABLE side_chat_sends;
         DROP TABLE side_chats;
         DROP TABLE task_execution_lineage;
         DROP TABLE automation_runs;
         DROP TABLE automation_mutations;
         DROP TABLE automations;
         DROP TABLE board_projects;
         DROP TABLE board_items;
         DROP TABLE board_mutations;
         DROP TABLE board_events;
         DROP TABLE board_task_links;
         DROP TABLE board_issue_refs;
         DROP TABLE board_launches;
         DROP TABLE board_outcomes;
         DROP TABLE board_intents;
         DROP TABLE board_review_runs;
         DROP TABLE board_authoring_sessions;
         DROP TABLE board_authoring_entries;
         DROP TABLE board_dependencies;
         ALTER TABLE task_command_state DROP COLUMN workspace_repair_revision;
         ALTER TABLE task_command_receipts DROP COLUMN workspace_repair_revision;
         DELETE FROM schema_migrations WHERE version > 9;",
    )
    .unwrap();
    drop(conn);
    let archive = BackupService::run_managed_offline_now(&root, 1_700_000_000_000).unwrap();
    let claims = &archive.manifest.databases[0];
    assert_eq!(claims.schema_version, 9);
    assert_eq!(claims.cleanup_tasks, None);
    assert_eq!(claims.local_executable_authority, None);
    let preview = BackupService::preview_managed_offline(&root, &archive.archive).unwrap();
    assert!(preview.unsupported.is_empty(), "{:?}", preview.unsupported);
    fs::write(root.join("ui-settings.json"), b"synthetic changed").unwrap();
    BackupService::restore_managed_offline(&root, &archive.archive).unwrap();
    assert_eq!(
        fs::read(root.join("ui-settings.json")).unwrap(),
        br#"{"theme":"synthetic"}"#
    );
    let conn = Connection::open(&db).unwrap();
    let schema: i64 = conn
        .query_row("SELECT max(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(schema, 19);
}

#[test]
fn managed_offline_recovery_replays_only_journaled_owned_paths() {
    let (parent, root, _backups) = fixture();
    fs::remove_file(root.join(".luna-l17-isolated-root")).unwrap();
    let root = root.canonicalize().unwrap();
    let rollback = parent
        .path()
        .canonicalize()
        .unwrap()
        .join(".luna-l17-managed-rollback-synthetic");
    fs::create_dir(&rollback).unwrap();
    fs::rename(
        root.join("ui-settings.json"),
        rollback.join("ui-settings.json"),
    )
    .unwrap();
    fs::write(root.join("ui-settings.json"), b"partial import").unwrap();
    let journal = serde_json::json!({
        "version": 1,
        "root": root,
        "rollback_dir": rollback,
        "entries": [{"path":"ui-settings.json","existed":true}],
    });
    fs::write(
        root.join(".luna-l17-managed-restore.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    assert!(BackupService::run_managed_offline_now(&root, 1_700_000_000_001).is_err());
    assert!(BackupService::recover_managed_restore(&root).unwrap());
    assert_eq!(
        fs::read(root.join("ui-settings.json")).unwrap(),
        br#"{"theme":"synthetic"}"#
    );
    assert!(!root.join(".luna-l17-managed-restore.json").exists());
}

#[test]
fn managed_offline_failed_readback_rolls_back_without_touching_exclusions() {
    let (_parent, root, _backups) = fixture();
    fs::remove_file(root.join(".luna-l17-isolated-root")).unwrap();
    let receipt = BackupService::run_managed_offline_now(&root, 1_700_000_000_000).unwrap();
    fs::write(root.join("ui-settings.json"), b"newer synthetic settings").unwrap();
    fs::write(root.join("session.json"), b"synthetic excluded auth").unwrap();
    let result = BackupService::restore_managed_offline_with_readback_for_tests(
        &root,
        &receipt.archive,
        |_| Err(BackupError::Invalid("synthetic readback failure".into())),
    );
    assert!(result.is_err());
    assert_eq!(
        fs::read(root.join("ui-settings.json")).unwrap(),
        b"newer synthetic settings"
    );
    assert_eq!(
        fs::read(root.join("session.json")).unwrap(),
        b"synthetic excluded auth"
    );
    assert!(!root.join(".luna-l17-managed-restore.json").exists());
}

fn copy_synthetic_tree(source: &Path, target: &Path) {
    fs::create_dir(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let kind = fs::symlink_metadata(&source_path).unwrap().file_type();
        assert!(!kind.is_symlink() && (kind.is_dir() || kind.is_file()));
        if kind.is_dir() {
            copy_synthetic_tree(&source_path, &target_path);
        } else {
            fs::copy(&source_path, &target_path).unwrap();
        }
    }
}

fn fake_accounts(parent: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: parent.join("synthetic-provider-state"),
        claude_config_dir: parent.join("synthetic-claude"),
        claude_config_file: parent.join("synthetic-claude.json"),
        codex_home: parent.join("synthetic-codex"),
    }
}

#[test]
fn active_engine_lock_refuses_copy_backup_until_writer_stops() {
    let (_parent, root, backups) = fixture();
    let lock = InstanceLock::acquire(&root).unwrap();
    assert!(matches!(
        service(&root).export(&backups.join("blocked.lunabk")),
        Err(BackupError::EngineRunning)
    ));
    assert!(!backups.join("blocked.lunabk").exists());
    drop(lock);
    assert!(
        service(&root)
            .export(&backups.join("after-stop.lunabk"))
            .is_ok()
    );
}

fn add_synthetic_task(root: &Path, room_gen: Option<u32>) {
    let profile = root.join("profiles/local");
    let store = DocsStore::open(&profile).unwrap();
    let now = chrono::Utc::now();
    let mut registry = RegistryDoc::new("synthetic-device");
    registry
        .upsert_space(&Space {
            execution_defaults: Default::default(),
            id: "synthetic-project".into(),
            device_id: "synthetic-device".into(),
            path: root.display().to_string(),
            name: Some("Synthetic project".into()),
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: now,
            removed_at: None,
        })
        .unwrap();
    registry
        .upsert_chat(&Chat {
            child_thread: None,
            id: "synthetic-task".into(),
            device_id: "synthetic-device".into(),
            title: Some("Synthetic task".into()),
            archived: false,
            settlement_override: None,
            pinned_order: None,
            cwd: None,
            branch: None,
            checkout_id: None,
            source_context: None,
            config: None,
            last_message_preview: None,
            last_message_at: Some(now),
            created_at: now,
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: Some("synthetic-project".into()),
            last_seen_at: None,
            manually_unread: false,
            room_gen,
        })
        .unwrap();
    store
        .save_snapshot("registry1", &registry.to_bytes().unwrap())
        .unwrap();
    let transcript = SessionDoc::init("synthetic-task").unwrap();
    transcript
        .push_message(&SessionMessageEntry {
            id: "synthetic-message".into(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text {
                id: "synthetic-part".into(),
                text: "synthetic prompt".into(),
            }],
            created_at: now.timestamp_millis(),
            device_id: "synthetic-device".into(),
            status: Some(MessageStatus::Complete),
            continuation_of: None,
        })
        .unwrap();
    store
        .save_snapshot("synthetic-task", &transcript.export_snapshot().unwrap())
        .unwrap();
}

#[test]
fn staged_activity_rebuild_survives_restart_and_detects_interrupted_readback() {
    let (_parent, root, backups) = fixture();
    add_synthetic_task(&root, None);
    let backup = service(&root)
        .export(&backups.join("activity.lunabk"))
        .unwrap();
    let counts = &backup.manifest.databases[0];
    assert_eq!(
        (counts.projects, counts.tasks, counts.attachment_blobs),
        (Some(1), Some(1), Some(1))
    );
    assert_eq!(counts.activity_events_expected, Some(1));
    let staged = service(&root).stage_restore(&backup.archive).unwrap();
    assert!(
        !staged
            .preview
            .unsupported
            .iter()
            .any(|reason| reason.contains("usage cache"))
    );
    let db = staged.path.join("profiles/local/docs.sqlite3");
    let conn = Connection::open(&db).unwrap();
    let indexed: i64 = conn
        .query_row("SELECT count(*) FROM activity_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(indexed, 1);
    conn.execute("UPDATE activity_events SET envelope_json = X'7b7d'", [])
        .unwrap();
    drop(conn);

    // A fresh service stands in for a restarted owner. It rejects the
    // interrupted/changed stage, then a clean retry rebuilds and reads back.
    assert!(service(&root).verify_staged_for_tests(&staged).is_err());
    fs::remove_dir_all(&staged.path).unwrap();
    let retry = service(&root).stage_restore(&backup.archive).unwrap();
    service(&root).verify_staged_for_tests(&retry).unwrap();
    let staged_blob = retry.path.join("profiles/local/uploads/synthetic.txt");
    fs::write(&staged_blob, b"damaged synthetic attachment").unwrap();
    assert!(service(&root).verify_staged_for_tests(&retry).is_err());
    fs::write(&staged_blob, b"synthetic attachment").unwrap();
    service(&root).verify_staged_for_tests(&retry).unwrap();
    assert!(service(&root).activate(retry).is_ok());
}

#[test]
fn staged_restore_rebuilds_appshot_history_from_v9_source_facts_on_current_schema() {
    let (_parent, root, backups) = fixture();
    add_synthetic_task(&root, None);
    let record = AppshotActivityRecord {
        event_id: "appshot:synthetic-draft:synthetic-capture:staged".into(),
        draft_id: "synthetic-draft".into(),
        task_id: Some("synthetic-task".into()),
        capture_id: "synthetic-capture".into(),
        screenshot_attachment_id: "synthetic-image".into(),
        capture_device_id: "synthetic-viewer".into(),
        target_environment_id: Some("synthetic-device".into()),
        kind: AppshotActivityKind::Staged,
        timestamp_ms: 42,
        app_name: "Synthetic App".into(),
        message_id: None,
    };
    let conn = Connection::open(root.join("profiles/local/docs.sqlite3")).unwrap();
    conn.execute(
        "INSERT INTO appshot_activity(event_id,draft_id,task_id,capture_id,target_environment_id,record_json)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            record.event_id,
            record.draft_id,
            record.task_id,
            record.capture_id,
            record.target_environment_id,
            serde_json::to_vec(&record).unwrap(),
        ],
    )
    .unwrap();
    drop(conn);

    let backup = service(&root)
        .export(&backups.join("appshot-v9.lunabk"))
        .unwrap();
    assert_eq!(backup.manifest.databases[0].schema_version, 19);
    assert_eq!(
        backup.manifest.databases[0].activity_events_expected,
        Some(2)
    );
    let staged = service(&root).stage_restore(&backup.archive).unwrap();
    service(&root).verify_staged_for_tests(&staged).unwrap();
    let conn = Connection::open(staged.path.join("profiles/local/docs.sqlite3")).unwrap();
    let (environment, capture_device, summary): (String, String, String) = conn
        .query_row(
            "SELECT json_extract(envelope_json,'$.environmentId'),
                    json_extract(envelope_json,'$.deviceId'),
                    json_extract(envelope_json,'$.summary')
             FROM activity_events WHERE event_id=?1",
            [record.event_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(environment, "synthetic-device");
    assert_eq!(capture_device, "synthetic-viewer");
    assert_eq!(summary, "Staged Appshot from Synthetic App");
}

#[test]
fn chat2_snapshot_requires_full_engine_replay_before_activation() {
    let (_parent, root, backups) = fixture();
    add_synthetic_task(&root, Some(2));
    let profile = root.join("profiles/local");
    let store = DocsStore::open(&profile).unwrap();
    let bytes = store.load_snapshot("synthetic-task").unwrap().unwrap();
    let doc = SessionDoc::from_snapshot(&bytes).unwrap();
    let frontier = doc.doc().oplog_vv();
    doc.push_message(&SessionMessageEntry {
        id: "pending-message".into(),
        role: MessageRole::User,
        parts: vec![MessagePart::Text {
            id: "pending-part".into(),
            text: "pending synthetic prompt".into(),
        }],
        created_at: chrono::Utc::now().timestamp_millis(),
        device_id: "synthetic-device".into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    })
    .unwrap();
    let update = doc
        .doc()
        .export(loro::ExportMode::updates(&frontier))
        .unwrap();
    store
        .save_snapshot_with_cursor("synthetic-task", &bytes, 4, 2)
        .unwrap();
    store
        .enqueue_chat_update("synthetic-task", "pending-batch", &update)
        .unwrap();
    drop(store);
    let backup = service(&root)
        .export(&backups.join("chat2.lunabk"))
        .unwrap();
    assert_eq!(backup.manifest.databases[0].tasks, Some(1));
    assert_eq!(
        backup.manifest.databases[0].activity_events_expected,
        Some(2)
    );
    let staged = service(&root).stage_restore(&backup.archive).unwrap();
    let conn = Connection::open(staged.path.join("profiles/local/docs.sqlite3")).unwrap();
    let indexed: i64 = conn
        .query_row("SELECT count(*) FROM activity_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(indexed, 2);
    assert!(
        staged
            .preview
            .unsupported
            .iter()
            .any(|reason| reason.contains("chat2 remote frontier and pending updates"))
    );
    assert!(service(&root).activate(staged).is_err());

    let store = DocsStore::open(&profile).unwrap();
    store
        .reject_chat_update("synthetic-task", "pending-batch")
        .unwrap();
    drop(store);
    let rejected = service(&root)
        .export(&backups.join("chat2-rejected.lunabk"))
        .unwrap();
    assert!(
        rejected.manifest.databases[0]
            .derived_rebuild_blocker
            .as_deref()
            .is_some_and(|reason| reason.contains("rejected chat2 updates"))
    );
}

#[test]
fn staged_activity_replays_luna_task_identity_without_provider_access() {
    let (_parent, root, backups) = fixture();
    add_synthetic_task(&root, None);
    let store = DocsStore::open(root.join("profiles/local")).unwrap();
    let log = serde_json::json!({
        "schemaVersion": 1,
        "events": [
            {"type":"workspaceObserved", "workspace": {
                "schemaVersion":1, "workspaceId":"identity-workspace",
                "environmentId":"synthetic-device", "currentPath":root.display().to_string(),
                "observedAtMs":1
            }},
            {"type":"taskRegistered", "task": {
                "schemaVersion":1, "taskId":"synthetic-task", "origin":"lunaCreated",
                "lineage":{}, "workspaceId":"identity-workspace", "createdAtMs":1
            }}
        ]
    });
    store
        .save_snapshot(
            "luna-task-identity-events-v1",
            &serde_json::to_vec(&log).unwrap(),
        )
        .unwrap();
    drop(store);
    let backup = service(&root)
        .export(&backups.join("identity.lunabk"))
        .unwrap();
    assert_eq!(
        backup.manifest.databases[0].activity_events_expected,
        Some(1)
    );
    let staged = service(&root).stage_restore(&backup.archive).unwrap();
    let conn = Connection::open(staged.path.join("profiles/local/docs.sqlite3")).unwrap();
    let json: Vec<u8> = conn
        .query_row("SELECT envelope_json FROM activity_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    let event: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(event["workspaceId"], "identity-workspace");
    assert!(
        !staged
            .preview
            .unsupported
            .iter()
            .any(|reason| reason.contains("usage cache"))
    );
}

#[test]
fn invalid_task_identity_log_blocks_staged_rebuild() {
    let (_parent, root, backups) = fixture();
    add_synthetic_task(&root, None);
    let store = DocsStore::open(root.join("profiles/local")).unwrap();
    store
        .save_snapshot(
            "luna-task-identity-events-v1",
            b"invalid synthetic identity log",
        )
        .unwrap();
    drop(store);
    let backup = service(&root)
        .export(&backups.join("invalid-identity.lunabk"))
        .unwrap();
    assert!(
        backup.manifest.databases[0]
            .derived_rebuild_blocker
            .as_deref()
            .is_some_and(|reason| reason.contains("task identity replay failed"))
    );
    assert!(service(&root).stage_restore(&backup.archive).is_err());

    let orphan_log = serde_json::json!({
        "schemaVersion": 1,
        "events": [
            {"type":"workspaceObserved", "workspace": {
                "schemaVersion":1, "workspaceId":"identity-workspace",
                "environmentId":"synthetic-device", "currentPath":root.display().to_string(),
                "observedAtMs":1
            }},
            {"type":"taskRegistered", "task": {
                "schemaVersion":1, "taskId":"missing-task", "origin":"lunaCreated",
                "lineage":{}, "workspaceId":"identity-workspace", "createdAtMs":1
            }}
        ]
    });
    let store = DocsStore::open(root.join("profiles/local")).unwrap();
    store
        .save_snapshot(
            "luna-task-identity-events-v1",
            &serde_json::to_vec(&orphan_log).unwrap(),
        )
        .unwrap();
    drop(store);
    let orphan = service(&root)
        .export(&backups.join("orphan-identity.lunabk"))
        .unwrap();
    assert!(
        orphan.manifest.databases[0]
            .derived_rebuild_blocker
            .as_deref()
            .is_some_and(|reason| reason.contains("no registry task"))
    );
}

#[test]
fn malformed_registry_row_cannot_be_silently_counted_as_absent() {
    let (_parent, root, backups) = fixture();
    add_synthetic_task(&root, None);
    let profile = root.join("profiles/local");
    let store = DocsStore::open(&profile).unwrap();
    let mut raw: serde_json::Value =
        serde_json::from_slice(&store.load_snapshot("registry1").unwrap().unwrap()).unwrap();
    for batch in raw["pending"].as_array_mut().unwrap() {
        for op in batch["ops"].as_array_mut().unwrap() {
            if op["kind"] == "chats" {
                op["set"].as_object_mut().unwrap().remove("id");
            }
        }
    }
    store
        .save_snapshot("registry1", &serde_json::to_vec(&raw).unwrap())
        .unwrap();
    drop(store);
    let backup = service(&root)
        .export(&backups.join("malformed-registry.lunabk"))
        .unwrap();
    assert!(
        backup.manifest.databases[0]
            .derived_rebuild_blocker
            .as_deref()
            .unwrap()
            .contains("malformed project or task rows")
    );
    assert!(service(&root).stage_restore(&backup.archive).is_err());
}

#[test]
fn export_is_encrypted_verified_and_excludes_secrets() {
    let (_parent, root, backups) = fixture();
    fs::create_dir(root.join("credentials")).unwrap();
    fs::write(
        root.join("credentials/environment-v1.vault"),
        b"fake forbidden secret",
    )
    .unwrap();
    fs::create_dir(root.join("provider-auth")).unwrap();
    fs::write(
        root.join("provider-auth/auth.json"),
        b"fake provider secret",
    )
    .unwrap();
    let receipt = service(&root)
        .export(&backups.join("manual.lunabk"))
        .unwrap();
    let bytes = fs::read(&receipt.archive).unwrap();
    assert!(bytes.starts_with(b"LUNABK01"));
    assert!(
        !bytes
            .windows(b"synthetic source".len())
            .any(|part| part == b"synthetic source")
    );
    assert!(
        !bytes
            .windows(b"fake forbidden secret".len())
            .any(|part| part == b"fake forbidden secret")
    );
    assert_eq!(receipt.manifest.databases[0].documents, 1);
    assert!(
        receipt
            .manifest
            .entries
            .iter()
            .any(|e| e.category == "attachment")
    );
    assert!(
        !receipt
            .manifest
            .entries
            .iter()
            .any(|e| e.path.contains("credential")
                || e.path.contains("provider-auth")
                || e.path == "device-id")
    );
    assert_eq!(
        inspect_archive(&receipt.archive, &RecoveryKey::injected([17; 32])).unwrap(),
        receipt.manifest
    );
    assert!(inspect_archive(&receipt.archive, &RecoveryKey::injected([18; 32])).is_err());

    let truncated = backups.join("truncated.lunabk");
    fs::write(&truncated, &bytes[..bytes.len() - 10]).unwrap();
    assert!(inspect_archive(&truncated, &RecoveryKey::injected([17; 32])).is_err());
    let tampered = backups.join("tampered.lunabk");
    let mut tampered_bytes = bytes;
    let midpoint = tampered_bytes.len() / 2;
    tampered_bytes[midpoint] ^= 0x20;
    fs::write(&tampered, tampered_bytes).unwrap();
    assert!(inspect_archive(&tampered, &RecoveryKey::injected([17; 32])).is_err());
}

#[test]
fn staged_restore_detects_change_then_activates_and_rolls_back() {
    let (_parent, root, backups) = fixture();
    let backup = service(&root)
        .export(&backups.join("original.lunabk"))
        .unwrap();
    assert!(
        service(&root)
            .stage_restore_with_available_bytes_for_tests(&backup.archive, 0)
            .is_err()
    );
    assert!(
        !fs::read_dir(root.parent().unwrap())
            .unwrap()
            .flatten()
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".luna-l17-stage-")
            })
    );
    let attachment = root.join("profiles/local/uploads/synthetic.txt");
    fs::remove_file(&attachment).unwrap();
    fs::write(root.join("ui-settings.json"), r#"{"theme":"changed"}"#).unwrap();
    let stage = service(&root).stage_restore(&backup.archive).unwrap();
    assert_eq!(stage.preview.manifest.databases[0].documents, 1);
    assert!(stage.preview.unsupported.is_empty());
    assert!(
        stage
            .preview
            .conflicts
            .contains(&"ui-settings.json".to_owned())
    );
    fs::write(
        root.join("ui-settings.json"),
        r#"{"theme":"changed-again"}"#,
    )
    .unwrap();
    assert!(service(&root).activate(stage).is_err());
    let stage = service(&root).stage_restore(&backup.archive).unwrap();
    let receipt = service(&root).activate(stage).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("ui-settings.json")).unwrap(),
        r#"{"theme":"synthetic"}"#
    );
    assert_eq!(
        fs::read(root.join("profiles/local/uploads/synthetic.txt")).unwrap(),
        b"synthetic attachment"
    );
    assert!(receipt.rollback_root.is_dir());
    let failed_root = receipt.rollback().unwrap();
    assert_eq!(
        fs::read_to_string(root.join("ui-settings.json")).unwrap(),
        r#"{"theme":"changed-again"}"#
    );
    assert!(failed_root.is_dir());
    assert!(!attachment.exists());
}

#[test]
fn restore_refuses_excluded_target_material_without_mutation() {
    let (_parent, root, backups) = fixture();
    let backup = service(&root)
        .export(&backups.join("original.lunabk"))
        .unwrap();
    fs::create_dir(root.join("credentials")).unwrap();
    let forbidden = root.join("credentials/environment-v1.vault");
    fs::write(&forbidden, b"fake-key-remains").unwrap();
    let preview = service(&root).preview_restore(&backup.archive).unwrap();
    assert!(!preview.unsupported.is_empty());
    assert!(service(&root).stage_restore(&backup.archive).is_err());
    assert_eq!(fs::read(&forbidden).unwrap(), b"fake-key-remains");
}

#[test]
fn failed_post_activation_readback_rolls_back_synthetic_root() {
    let (parent, root, backups) = fixture();
    let backup = service(&root)
        .export(&backups.join("original.lunabk"))
        .unwrap();
    fs::write(root.join("ui-settings.json"), b"changed after backup").unwrap();
    let staged = service(&root).stage_restore(&backup.archive).unwrap();
    let result = service(&root).activate_with_readback_for_tests(staged, |restored| {
        let store = DocsStore::open(restored.join("profiles/local"))
            .map_err(|error| BackupError::Invalid(error.to_string()))?;
        assert!(store.load_snapshot("synthetic-doc").unwrap().is_some());
        Err(BackupError::Invalid(
            "synthetic post-start readback failed".into(),
        ))
    });
    assert!(result.is_err());
    assert_eq!(
        fs::read(root.join("ui-settings.json")).unwrap(),
        b"changed after backup"
    );
    let failed: Vec<_> = fs::read_dir(parent.path())
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".luna-l17-failed-")
        })
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(
        fs::read(failed[0].path().join("ui-settings.json")).unwrap(),
        br#"{"theme":"synthetic"}"#
    );
}

#[tokio::test]
async fn staged_copy_engine_boot_probe_keeps_original_stage_untouched() {
    let (parent, root, backups) = fixture();
    let backup = service(&root)
        .export(&backups.join("boot-probe.lunabk"))
        .unwrap();
    let staged = service(&root).stage_restore(&backup.archive).unwrap();
    let probe = parent.path().join("marked-boot-probe");
    copy_synthetic_tree(&staged.path, &probe);
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::local(&probe).unwrap(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        fake_accounts(&parent.path().join("probe-provider-fixtures")),
    )
    .unwrap();
    let blocked_archive = backups.join("while-engine-running.lunabk");
    assert!(matches!(
        service(&probe).export(&blocked_archive),
        Err(BackupError::EngineRunning)
    ));
    assert!(!blocked_archive.exists());
    let store = DocsStore::open(probe.join("profiles/local")).unwrap();
    assert_eq!(
        store.load_snapshot("synthetic-doc").unwrap().unwrap(),
        b"synthetic source"
    );
    drop(store);
    core.shutdown().await;
    drop(core);
    assert!(!probe.join("credentials").exists());
    service(&root).verify_staged_for_tests(&staged).unwrap();

    let corrupt_probe = parent.path().join("marked-corrupt-probe");
    copy_synthetic_tree(&staged.path, &corrupt_probe);
    fs::write(
        corrupt_probe.join("profiles/local/docs.sqlite3"),
        b"invalid synthetic SQLite",
    )
    .unwrap();
    let boot = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::local(&corrupt_probe).unwrap(),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        fake_accounts(&parent.path().join("corrupt-provider-fixtures")),
    );
    assert!(boot.is_err());
    assert!(!corrupt_probe.join("credentials").exists());
    service(&root).verify_staged_for_tests(&staged).unwrap();
}

// Authored before the draft-PR backup enrollment: uncertain writes must survive
// an archive/restore unchanged and never become permission to replay a POST.
#[test]
fn github_pr_attempt_journal_survives_backup_and_staging() {
    let (_parent, root, backups) = fixture();
    let relative = format!("profiles/local/github-pr-attempts/{}.json", "a".repeat(64));
    let path = root.join(&relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let contents = br#"{"version":1,"operation_id":"38cc3a90-0eb9-454b-ab67-dd08bda1617a","target":{"repository":"github.com/acme/luna","base":"main","head":"feature/pr"},"head_sha":"1111111111111111111111111111111111111111"}"#;
    fs::write(&path, contents).unwrap();
    fs::write(path.with_extension("lock"), b"").unwrap();
    let backup = BackupService::new_bb_style(IsolatedRoot::open(&root).unwrap());
    let receipt = backup.backup_now(&backups, 1_700_000_000_000).unwrap();
    assert!(receipt.manifest.entries.iter().any(|e| e.path == relative));
    let staged = backup.stage_restore(&receipt.archive).unwrap();
    backup.verify_staged_for_tests(&staged).unwrap();
    assert_eq!(fs::read(staged.path.join(&relative)).unwrap(), contents);
    assert!(!staged.path.join(&relative).with_extension("lock").exists());
}

#[test]
fn side_chat_metadata_and_lazy_transcript_survive_backup_restore() {
    let (_parent, root, backups) = fixture();
    let profile = root.join("profiles/local/docs.sqlite3");
    let conn = Connection::open(&profile).unwrap();
    conn.execute("INSERT INTO side_chats (id,parent_id,title,created_at_ms,expires_at_ms,state,config,cwd,context,context_mode,origin_message_id,anchor_text,creation_hash) VALUES ('synthetic-side','synthetic-parent','Saved side chat',1,7200001,'unloaded','{}','/tmp','','nativeFork','origin-message','selected quote','fixture-hash')", []).unwrap();
    conn.execute("INSERT INTO side_chat_entries VALUES ('synthetic-side',0,?1)", [r#"{"id":"saved-message","role":"assistant","parts":[{"kind":"text","id":"text","text":"SIDE_BACKUP_SENTINEL"}],"createdAt":2,"deviceId":"synthetic-device","status":"complete"}"#]).unwrap();
    drop(conn);
    let backup = service(&root)
        .export(&backups.join("side-chat.lunabk"))
        .unwrap();
    assert_eq!(backup.manifest.databases[0].schema_version, 19);
    let staged = service(&root).stage_restore(&backup.archive).unwrap();
    service(&root).verify_staged_for_tests(&staged).unwrap();
    let restored = Connection::open(staged.path.join("profiles/local/docs.sqlite3")).unwrap();
    let row: (String,String,String) = restored.query_row("SELECT parent_id,origin_message_id,anchor_text FROM side_chats WHERE id='synthetic-side'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(
        row,
        (
            "synthetic-parent".into(),
            "origin-message".into(),
            "selected quote".into()
        )
    );
    let transcript: String = restored
        .query_row(
            "SELECT entry FROM side_chat_entries WHERE chat_id='synthetic-side'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(transcript.contains("SIDE_BACKUP_SENTINEL"));
}
