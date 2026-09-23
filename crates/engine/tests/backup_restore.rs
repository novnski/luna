//! L17 tests use only fresh synthetic roots and an injected fake key.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use tempfile::TempDir;
use zeron_doc::{
    MessagePart, MessageRole, MessageStatus, RegistryDoc, SessionDoc, SessionMessageEntry,
};
use zeron_engine::InstanceLock;
use zeron_engine::backup::{
    BackupError, BackupService, IsolatedRoot, RecoveryKey, inspect_archive,
};
use zeron_proto::{Chat, Space};
use zeron_sync::DocsStore;

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
            id: "synthetic-project".into(),
            device_id: "synthetic-device".into(),
            path: root.display().to_string(),
            name: Some("Synthetic project".into()),
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: now,
        })
        .unwrap();
    registry
        .upsert_chat(&Chat {
            id: "synthetic-task".into(),
            device_id: "synthetic-device".into(),
            title: Some("Synthetic task".into()),
            archived: false,
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
        staged
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
    assert!(service(&root).activate(retry).is_err());
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
        staged
            .preview
            .unsupported
            .iter()
            .any(|s| s.contains("usage cache"))
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
