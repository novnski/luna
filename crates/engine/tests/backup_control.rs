//! No native login, Keychain, provider store or normal LUNA profile is used.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use luna_engine::backup::{BackupService, IsolatedRoot};
use luna_engine::backup_control::BackupControl;
use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use luna_proto::HarnessId;
use luna_proto::{BackupArchiveRequest, BackupAvailability, BackupConfigureRequest};
use luna_rpc::{memory_client, methods};
use luna_sync::DocsStore;
use std::sync::Arc;
use tempfile::TempDir;

fn marked_root(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::write(
        path.join(".luna-l17-isolated-root"),
        "L17 synthetic or copy-isolated root\n",
    )
    .unwrap();
}

#[test]
fn bb_style_control_drives_persisted_manual_and_daily_copy_backup() {
    let temp = TempDir::new().unwrap();
    let owner = temp.path().join("synthetic-engine");
    let source = temp.path().join("copy-isolated-source");
    let destination = temp.path().join("marked-backups");
    marked_root(&owner);
    marked_root(&source);
    fs::create_dir(&destination).unwrap();
    fs::write(
        destination.join(".luna-l17-isolated-backups"),
        "L17 synthetic or copy-isolated backup destination\n",
    )
    .unwrap();
    let store = DocsStore::open(source.join("profiles/local")).unwrap();
    store
        .save_snapshot("synthetic", b"synthetic transcript")
        .unwrap();
    drop(store);

    let control = BackupControl::new(IsolatedRoot::open(&owner).unwrap());
    assert_eq!(
        control.status().unwrap().availability,
        BackupAvailability::Unconfigured
    );
    assert!(!control.status().unwrap().key_configured);

    let status = control
        .configure(BackupConfigureRequest {
            source_root: source.display().to_string(),
            destination_dir: destination.display().to_string(),
            automatic_enabled: true,
        })
        .unwrap();
    assert_eq!(status.availability, BackupAvailability::Ready);
    assert!(!status.restore_activation_available);
    let manual = control.manual_backup().unwrap();
    assert!(Path::new(&manual.archive_path).is_file());
    assert!(manual.archive_path.ends_with(".tar.gz"));
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&manual.archive_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(control.status().unwrap().last_success.unwrap(), manual);
    let preview = control
        .inspect(BackupArchiveRequest {
            archive_path: manual.archive_path.clone(),
        })
        .unwrap();
    assert_eq!(preview.document_count, 1);
    assert!(!preview.restore_activation_available);
    let staged = control
        .stage_restore(BackupArchiveRequest {
            archive_path: manual.archive_path.clone(),
        })
        .unwrap();
    assert!(Path::new(&staged.staging_path).is_dir());
    assert!(!staged.restore_activation_available);
    let fresh = BackupControl::new(IsolatedRoot::open(&owner).unwrap());
    assert_eq!(fresh.status().unwrap().last_success.unwrap(), manual);
    let reverified = fresh
        .stage_restore(BackupArchiveRequest {
            archive_path: manual.archive_path.clone(),
        })
        .unwrap();
    assert_eq!(reverified.staging_path, staged.staging_path);
    assert!(
        reverified
            .reason
            .contains("fresh archive and index readback")
    );
    assert!(
        fresh
            .stage_restore(BackupArchiveRequest {
                archive_path: destination.join("different.tar.gz").display().to_string(),
            })
            .is_err()
    );
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!(fresh.tick_automatic(now).unwrap().is_none());
    std::thread::sleep(std::time::Duration::from_millis(10));
    fs::write(
        source.join("ui-settings.json"),
        b"synthetic setting changed",
    )
    .unwrap();
    let automatic = fresh.tick_automatic(now + 86_400_000 + 1).unwrap().unwrap();
    assert!(Path::new(&automatic.archive_path).is_file());
    assert_ne!(automatic.archive_sha256, manual.archive_sha256);
    assert_eq!(fresh.status().unwrap().last_success.unwrap(), automatic);

    assert!(!owner.join("credentials").exists());
}

#[test]
fn staged_checkpoint_fails_closed_across_restart_and_retries_after_repair() {
    let temp = TempDir::new().unwrap();
    let owner = temp.path().join("synthetic-engine");
    let source = temp.path().join("copy-isolated-source");
    let destination = temp.path().join("marked-backups");
    marked_root(&owner);
    marked_root(&source);
    fs::create_dir(&destination).unwrap();
    fs::write(
        destination.join(".luna-l17-isolated-backups"),
        "L17 synthetic or copy-isolated backup destination\n",
    )
    .unwrap();
    fs::write(source.join("ui-settings.json"), b"synthetic original").unwrap();
    let store = DocsStore::open(source.join("profiles/local")).unwrap();
    store
        .save_snapshot("synthetic", b"synthetic transcript")
        .unwrap();
    drop(store);

    let control = BackupControl::new(IsolatedRoot::open(&owner).unwrap());
    control
        .configure(BackupConfigureRequest {
            source_root: source.display().to_string(),
            destination_dir: destination.display().to_string(),
            automatic_enabled: false,
        })
        .unwrap();
    let archive = control.manual_backup().unwrap().archive_path;
    assert!(archive.ends_with(".tar.gz"));
    let request = BackupArchiveRequest {
        archive_path: archive,
    };
    let staged = control.stage_restore(request.clone()).unwrap();
    drop(control);

    let fresh = BackupControl::new(IsolatedRoot::open(&owner).unwrap());
    let stage_path = Path::new(&staged.staging_path);
    fs::write(stage_path.join("ui-settings.json"), b"changed stage").unwrap();
    assert!(fresh.stage_restore(request.clone()).is_err());
    let failed = fresh.status().unwrap().staged_restore.unwrap();
    assert!(failed.reason.contains("Staged verification failed"));
    assert_eq!(failed.staging_path, staged.staging_path);

    fs::write(stage_path.join("ui-settings.json"), b"synthetic original").unwrap();
    fresh.stage_restore(request.clone()).unwrap();
    let moved = temp.path().join("interrupted-stage-held");
    fs::rename(stage_path, &moved).unwrap();
    assert!(
        fresh
            .status()
            .unwrap()
            .staged_restore
            .unwrap()
            .reason
            .contains("missing")
    );
    assert!(fresh.stage_restore(request.clone()).is_err());
    fs::rename(&moved, stage_path).unwrap();
    let recovered = fresh.stage_restore(request.clone()).unwrap();
    assert_eq!(recovered.staging_path, staged.staging_path);
    assert!(!recovered.restore_activation_available);

    let state_path = owner.join("backup-control-v1.json");
    let mut old_state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    old_state.as_object_mut().unwrap().remove("stagedEvidence");
    fs::write(&state_path, serde_json::to_vec(&old_state).unwrap()).unwrap();
    let legacy = BackupControl::new(IsolatedRoot::open(&owner).unwrap());
    assert!(
        legacy
            .status()
            .unwrap()
            .staged_restore
            .unwrap()
            .reason
            .contains("predates restart-safe readback")
    );
    assert!(legacy.stage_restore(request).is_err());
    assert!(stage_path.is_dir());
}

#[tokio::test]
async fn local_rpc_reports_copy_only_status_and_rejects_foreign_target() {
    let temp = TempDir::new().unwrap();
    let owner = temp.path().join("synthetic-engine");
    let source = temp.path().join("copy-isolated-source");
    let destination = temp.path().join("marked-backups");
    marked_root(&owner);
    marked_root(&source);
    fs::create_dir(&destination).unwrap();
    fs::write(
        destination.join(".luna-l17-isolated-backups"),
        "L17 synthetic or copy-isolated backup destination\n",
    )
    .unwrap();
    let store = DocsStore::open(source.join("profiles/local")).unwrap();
    store
        .save_snapshot("synthetic", b"synthetic transcript")
        .unwrap();
    drop(store);
    let control = BackupControl::new(IsolatedRoot::open(&owner).unwrap());
    control
        .configure(BackupConfigureRequest {
            source_root: source.display().to_string(),
            destination_dir: destination.display().to_string(),
            automatic_enabled: false,
        })
        .unwrap();
    let accounts = AgentAccountsConfig {
        data_dir: temp.path().join("provider-state"),
        claude_config_dir: temp.path().join("synthetic-claude"),
        claude_config_file: temp.path().join("synthetic-claude.json"),
        codex_home: temp.path().join("synthetic-codex"),
    };
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&owner, "fixture-org", "fixture-user"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap()
    .with_backup_control_for_tests(control);
    let client = memory_client(core.rpc_service());
    let status: luna_proto::BackupStatus = serde_json::from_value(
        client
            .call(methods::GET_BACKUP_STATUS, serde_json::json!({}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(status.availability, BackupAvailability::Ready);
    assert!(!status.key_configured);
    assert!(
        client
            .call(
                methods::CONFIGURE_BACKUP_KEY,
                serde_json::json!({
                    "targetDeviceId":"foreign", "recoveryKeyHex":"22".repeat(32)
                })
            )
            .await
            .is_err()
    );
    assert!(
        client
            .call(
                methods::CONFIGURE_BACKUP_KEY,
                serde_json::json!({ "recoveryKeyHex":"22".repeat(32) }),
            )
            .await
            .is_err()
    );
    let status: luna_proto::BackupStatus = serde_json::from_value(
        client
            .call(methods::GET_BACKUP_STATUS, serde_json::json!({}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(status.availability, BackupAvailability::Ready);
    assert!(!status.restore_activation_available);
    assert!(
        client
            .call(
                methods::BACKUP_NOW,
                serde_json::json!({"targetDeviceId":"foreign"})
            )
            .await
            .is_err()
    );
    let summary: luna_proto::BackupSummary = serde_json::from_value(
        client
            .call(methods::BACKUP_NOW, serde_json::json!({}))
            .await
            .unwrap(),
    )
    .unwrap();
    let preview: luna_proto::BackupPreview = serde_json::from_value(
        client
            .call(
                methods::INSPECT_BACKUP,
                serde_json::json!({"archivePath": summary.archive_path}),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview.document_count, 1);
    assert!(!preview.restore_activation_available);
    let stage: luna_proto::BackupStageStatus = serde_json::from_value(
        client
            .call(
                methods::STAGE_BACKUP_RESTORE,
                serde_json::json!({
                    "archivePath": preview.archive_path
                }),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(Path::new(&stage.staging_path).is_dir());
    assert!(!stage.restore_activation_available);
    let rechecked: luna_proto::BackupStageStatus = serde_json::from_value(
        client
            .call(
                methods::STAGE_BACKUP_RESTORE,
                serde_json::json!({"archivePath": stage.archive_path}),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(rechecked.staging_path, stage.staging_path);
    core.shutdown().await;
}

#[tokio::test]
async fn ordinary_unmarked_fixture_root_has_no_backup_control_or_vault_probe() {
    let temp = TempDir::new().unwrap();
    let owner = temp.path().join("unmarked-engine");
    let accounts = AgentAccountsConfig {
        data_dir: temp.path().join("provider-state"),
        claude_config_dir: temp.path().join("synthetic-claude"),
        claude_config_file: temp.path().join("synthetic-claude.json"),
        codex_home: temp.path().join("synthetic-codex"),
    };
    accounts.disable_claude_keychain_fallback().unwrap();
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&owner, "fixture-org", "fixture-user"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts.clone(),
    )
    .unwrap();
    let client = memory_client(core.rpc_service());
    let status: luna_proto::BackupStatus = serde_json::from_value(
        client
            .call(methods::GET_BACKUP_STATUS, serde_json::json!({}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(status.availability, BackupAvailability::Unavailable);
    assert!(!status.key_configured);
    assert!(
        client
            .call(methods::BACKUP_NOW, serde_json::json!({}))
            .await
            .is_err()
    );
    assert!(!owner.join("credentials").exists());
    assert!(core.shutdown().await);
    drop(core);
    let receipt = BackupService::run_managed_offline_if_due(&owner, 1_700_000_000_000)
        .unwrap()
        .unwrap();
    let after: luna_proto::BackupStatus = serde_json::from_value(
        client
            .call(methods::GET_BACKUP_STATUS, serde_json::json!({}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        after.last_success.unwrap().archive_path,
        receipt.archive.display().to_string()
    );
    assert!(after.automatic_enabled);
    drop(client);
    let restored = BackupService::restore_managed_offline(&owner, &receipt.archive).unwrap();
    assert!(restored.restored_entries > 0);
    let reopened =
        EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            EngineProfile::development(&owner, "fixture-org", "fixture-user"),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            accounts,
        )
        .unwrap();
    assert!(reopened.shutdown().await);
}
