//! Production account/vault lifecycle over disposable device roots, without OS credentials.
use luna_engine::{
    AgentAccounts, AgentAccountsConfig, CredentialBroker, CredentialScope, SecretIngress,
};
use luna_proto::{AgentAuthenticationState, HarnessId};
use std::fs;

#[tokio::test]
async fn claude_identity_without_secret_is_available_without_credential_verification() {
    let root = tempfile::tempdir().unwrap();
    let config = AgentAccountsConfig {
        data_dir: root.path().join("data"),
        claude_config_dir: root.path().join("claude"),
        claude_config_file: root.path().join("claude.json"),
        codex_home: root.path().join("codex"),
    };
    let identity =
        br#"{"oauthAccount":{"emailAddress":"fixture@example.test","accountUuid":"fixture"}}"#;
    fs::write(&config.claude_config_file, identity).unwrap();
    // Deliberately no opt-out marker and no credentials file.
    let accounts = AgentAccounts::new(config.clone());
    let snapshot = accounts
        .list_harness(HarnessId::ClaudeCode, false)
        .await
        .unwrap();
    assert_eq!(snapshot.accounts.len(), 1);
    assert!(snapshot.accounts[0].active);
    assert_eq!(
        snapshot.accounts[0].authentication,
        AgentAuthenticationState::Unknown
    );
    assert!(snapshot.warnings.is_empty());
    accounts
        .activate(HarnessId::ClaudeCode, &snapshot.accounts[0].id)
        .await
        .unwrap();
    assert_eq!(fs::read(&config.claude_config_file).unwrap(), identity);
    assert!(!config.claude_config_dir.join(".credentials.json").exists());
    fs::create_dir_all(&config.claude_config_dir).unwrap();
    for payload in ["{}", "null", r#"{"mcpOAuth":{"fixture":true}}"#] {
        fs::write(config.claude_config_dir.join(".credentials.json"), payload).unwrap();
        let snapshot = accounts
            .list_harness(HarnessId::ClaudeCode, false)
            .await
            .unwrap();
        assert_eq!(
            snapshot.accounts[0].authentication,
            AgentAuthenticationState::Unknown,
            "{payload}"
        );
    }
}

#[test]
fn production_vault_survives_restart_and_two_handles_without_os_store() {
    let root = tempfile::tempdir().unwrap();
    let scope = CredentialScope::luna_sync();
    let broker = CredentialBroker::for_data_dir(root.path());
    let (_, first) = broker
        .store_local(
            SecretIngress::LocalEngine,
            scope.clone(),
            "first",
            "fixture",
            "local",
            b"first-fixture-value",
        )
        .unwrap();
    let key = root.path().join("credentials/environment-v1.key");
    assert_eq!(fs::read(&key).unwrap().len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(key.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    let restart = CredentialBroker::for_data_dir(root.path());
    let (_, second) = restart
        .store_local(
            SecretIngress::LocalEngine,
            scope.clone(),
            "second",
            "fixture",
            "local",
            b"second-fixture-value",
        )
        .unwrap();
    assert_eq!(restart.lease_by_id(&first.id, &scope).unwrap(), first);
    assert_eq!(broker.lease_by_id(&second.id, &scope).unwrap(), second);
    assert_eq!(restart.list_metadata().unwrap().len(), 2);
    let ciphertext = fs::read(root.path().join("credentials/environment-v1.vault")).unwrap();
    assert!(!String::from_utf8_lossy(&ciphertext).contains("fixture-value"));
}

#[test]
fn missing_and_unsafe_vault_keys_preserve_existing_ciphertext() {
    for case in ["missing", "short", "insecure", "symlink", "hardlink"] {
        let root = tempfile::tempdir().unwrap();
        let scope = CredentialScope::luna_sync();
        let broker = CredentialBroker::for_data_dir(root.path());
        broker
            .store_local(
                SecretIngress::LocalEngine,
                scope.clone(),
                "fixture",
                "fixture",
                "local",
                b"secret-fixture",
            )
            .unwrap();
        let key = root.path().join("credentials/environment-v1.key");
        let vault = root.path().join("credentials/environment-v1.vault");
        let original = fs::read(&vault).unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, [7u8; 32]).unwrap();
        match case {
            "missing" => fs::remove_file(&key).unwrap(),
            "short" => fs::write(&key, b"short").unwrap(),
            #[cfg(unix)]
            "insecure" => {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
            }
            #[cfg(unix)]
            "symlink" => {
                fs::remove_file(&key).unwrap();
                std::os::unix::fs::symlink(&outside, &key).unwrap();
            }
            "hardlink" => {
                fs::remove_file(&key).unwrap();
                fs::hard_link(&outside, &key).unwrap();
            }
            _ => continue,
        }
        let restart = CredentialBroker::for_data_dir(root.path());
        assert!(restart.list_metadata().is_err(), "{case}");
        assert!(
            restart
                .store_local(
                    SecretIngress::LocalEngine,
                    scope,
                    "new",
                    "fixture",
                    "local",
                    b"other-fixture"
                )
                .is_err(),
            "{case}"
        );
        assert_eq!(fs::read(&vault).unwrap(), original, "{case}");
        assert_eq!(fs::read(&outside).unwrap(), [7u8; 32], "{case}");
        if case == "missing" {
            assert!(!key.exists());
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn interrupted_first_key_write_is_recoverable_without_replacing_keys() {
    if let Some(root) = std::env::var_os("LUNA_KEY_WRITE_FAILURE_FIXTURE") {
        // Only this disposable child has a file-size limit and ignored signal.
        unsafe {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let limit = libc::rlimit {
                rlim_cur: 16,
                rlim_max: 16,
            };
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
        }
        let broker = CredentialBroker::for_data_dir(std::path::Path::new(&root));
        assert!(
            broker
                .store_local(
                    SecretIngress::LocalEngine,
                    CredentialScope::luna_sync(),
                    "fixture",
                    "fixture",
                    "local",
                    b"fixture"
                )
                .is_err()
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "interrupted_first_key_write_is_recoverable_without_replacing_keys",
            "--nocapture",
        ])
        .env("LUNA_KEY_WRITE_FAILURE_FIXTURE", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.path().join("credentials/environment-v1.key").exists());
    assert!(
        !root
            .path()
            .join("credentials/environment-v1.vault")
            .exists()
    );
    let broker = CredentialBroker::for_data_dir(root.path());
    assert!(broker.list_metadata().unwrap().is_empty());
    broker
        .store_local(
            SecretIngress::LocalEngine,
            CredentialScope::luna_sync(),
            "fixture",
            "fixture",
            "local",
            b"fixture",
        )
        .unwrap();
    assert_eq!(broker.list_metadata().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn redirected_credential_directory_and_lock_do_not_touch_foreign_targets() {
    use std::os::unix::fs::PermissionsExt;
    for case in ["directory", "lock-symlink", "lock-hardlink"] {
        let root = tempfile::tempdir().unwrap();
        let credentials = root.path().join("credentials");
        let outside = root.path().join("outside");
        if case == "directory" {
            fs::create_dir(&outside).unwrap();
            fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
            std::os::unix::fs::symlink(&outside, &credentials).unwrap();
        } else {
            fs::create_dir(&credentials).unwrap();
            fs::write(&outside, b"foreign fixture").unwrap();
            fs::set_permissions(&outside, fs::Permissions::from_mode(0o644)).unwrap();
            let lock = credentials.join("environment-v1.lock");
            if case == "lock-symlink" {
                std::os::unix::fs::symlink(&outside, &lock).unwrap();
            } else {
                fs::hard_link(&outside, &lock).unwrap();
            }
        }
        let permissions = fs::metadata(&outside).unwrap().permissions().mode();
        let broker = CredentialBroker::for_data_dir(root.path());
        assert!(broker.list_metadata().is_err(), "{case}");
        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().mode(),
            permissions,
            "{case}"
        );
        if case == "directory" {
            assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        } else {
            assert_eq!(fs::read(&outside).unwrap(), b"foreign fixture");
        }
    }
}
