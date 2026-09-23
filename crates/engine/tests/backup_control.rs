//! No native login, Keychain, provider store or normal LUNA profile is used.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use std::sync::Arc;
use tempfile::TempDir;
use zeron_engine::backup::{IsolatedRoot, RecoveryKey, inspect_archive};
use zeron_engine::backup_control::{BackupControl, BackupControlError};
use zeron_engine::credentials::{CredentialBroker, CredentialScope};
use zeron_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessRegistry};
use zeron_proto::HarnessId;
use zeron_proto::{BackupArchiveRequest, BackupAvailability, BackupConfigureRequest};
use zeron_rpc::{memory_client, methods};
use zeron_sync::DocsStore;

fn marked_root(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::write(
        path.join(".luna-l17-isolated-root"),
        "L17 synthetic or copy-isolated root\n",
    )
    .unwrap();
}

#[test]
fn synthetic_key_generation_never_falls_back_to_platform_broker() {
    let temp = TempDir::new().unwrap();
    let owner = temp.path().join("marked-synthetic-owner");
    marked_root(&owner);
    let control = BackupControl::new(
        IsolatedRoot::open(&owner).unwrap(),
        CredentialBroker::for_data_dir(&owner),
    );
    assert!(matches!(
        control.generate_recovery_key_for_synthetic_tests(),
        Err(BackupControlError::SyntheticKeyOnly)
    ));
    assert!(!owner.join("credentials").exists());
}

#[test]
fn broker_scoped_key_drives_persisted_manual_and_daily_copy_backup() {
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

    let broker = CredentialBroker::in_memory_for_tests(&owner);
    let control = BackupControl::new(IsolatedRoot::open(&owner).unwrap(), broker.clone());
    assert_eq!(
        control.status().unwrap().availability,
        BackupAvailability::Unconfigured
    );
    assert!(control.configure_recovery_key_hex("bad".into()).is_err());
    control.configure_recovery_key_hex("11".repeat(32)).unwrap();
    assert!(control.status().unwrap().key_configured);
    let key = broker
        .list_metadata()
        .unwrap()
        .into_iter()
        .find(|m| m.name == "LUNA backup recovery key")
        .unwrap();
    assert!(
        broker
            .lease_by_id(&key.id, &CredentialScope::luna_sync())
            .is_err()
    );
    assert!(
        broker
            .lease_by_id(&key.id, &CredentialScope::luna_backup())
            .is_ok()
    );

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
    assert!(control.configure_recovery_key_hex("33".repeat(32)).is_err());
    assert!(Path::new(&manual.archive_path).is_file());
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
    let fresh = BackupControl::new(IsolatedRoot::open(&owner).unwrap(), broker.clone());
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
                archive_path: destination.join("different.lunabk").display().to_string(),
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

    // A different in-memory store cannot recover the scoped archive key. It
    // must report unconfigured instead of reading a native credential store.
    let unrelated = BackupControl::new(
        IsolatedRoot::open(&owner).unwrap(),
        CredentialBroker::in_memory_for_tests(&owner),
    );
    let status = unrelated.status().unwrap();
    assert_eq!(status.availability, BackupAvailability::Unconfigured);
    assert!(!status.key_configured);
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

    let broker = CredentialBroker::in_memory_for_tests(&owner);
    let control = BackupControl::new(IsolatedRoot::open(&owner).unwrap(), broker.clone());
    let owner_code = control.generate_recovery_key_for_synthetic_tests().unwrap();
    assert_eq!(owner_code.len(), 64);
    assert!(owner_code.bytes().all(|byte| byte.is_ascii_hexdigit()));
    control
        .configure(BackupConfigureRequest {
            source_root: source.display().to_string(),
            destination_dir: destination.display().to_string(),
            automatic_enabled: false,
        })
        .unwrap();
    let archive = control.manual_backup().unwrap().archive_path;
    let recovered_bytes: [u8; 32] = std::array::from_fn(|index| {
        u8::from_str_radix(&owner_code[index * 2..index * 2 + 2], 16).unwrap()
    });
    assert!(inspect_archive(Path::new(&archive), &RecoveryKey::injected(recovered_bytes)).is_ok());
    assert!(control.generate_recovery_key_for_synthetic_tests().is_err());
    let request = BackupArchiveRequest {
        archive_path: archive,
    };
    let staged = control.stage_restore(request.clone()).unwrap();
    drop(control);

    let fresh = BackupControl::new(IsolatedRoot::open(&owner).unwrap(), broker.clone());
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
    let legacy = BackupControl::new(IsolatedRoot::open(&owner).unwrap(), broker);
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
    let broker = CredentialBroker::in_memory_for_tests(&owner);
    let control = BackupControl::new(IsolatedRoot::open(&owner).unwrap(), broker);
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
        cursor_sdk_auth_file: temp.path().join("synthetic-cursor-auth.json"),
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
    let status: zeron_proto::BackupStatus = serde_json::from_value(
        client
            .call(methods::GET_BACKUP_STATUS, serde_json::json!({}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(status.availability, BackupAvailability::Unconfigured);
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
    client
        .call(
            methods::CONFIGURE_BACKUP_KEY,
            serde_json::json!({
                "recoveryKeyHex":"22".repeat(32)
            }),
        )
        .await
        .unwrap();
    let status: zeron_proto::BackupStatus = serde_json::from_value(
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
    let summary: zeron_proto::BackupSummary = serde_json::from_value(
        client
            .call(methods::BACKUP_NOW, serde_json::json!({}))
            .await
            .unwrap(),
    )
    .unwrap();
    let preview: zeron_proto::BackupPreview = serde_json::from_value(
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
    let stage: zeron_proto::BackupStageStatus = serde_json::from_value(
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
    let rechecked: zeron_proto::BackupStageStatus = serde_json::from_value(
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
        cursor_sdk_auth_file: temp.path().join("synthetic-cursor-auth.json"),
    };
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&owner, "fixture-org", "fixture-user"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap();
    let client = memory_client(core.rpc_service());
    let status: zeron_proto::BackupStatus = serde_json::from_value(
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
    core.shutdown().await;
}
