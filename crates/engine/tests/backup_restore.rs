//! L17 tests use only fresh synthetic roots and an injected fake key.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use tempfile::TempDir;
use zeron_engine::backup::{BackupService, IsolatedRoot, RecoveryKey, inspect_archive};
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
