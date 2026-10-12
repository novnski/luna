//! RM-01 remote access at the engine boundary: the remote authority store's
//! on-disk lifecycle (migration, corrupt-store recovery). Failure cases:
//! `docs/pending-tasks/control-this-mac/rm-01/failures.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use luna_engine::{AgentAccountsConfig, EngineCore, EngineProfile, HarnessId, HarnessRegistry};
use luna_rpc::methods;
use luna_rpc::remote_auth::{IssuedPairing, IssuedSession, SessionView};
use serde_json::{Value, json};

fn profile(root: &Path) -> EngineProfile {
    EngineProfile::development(&root.join("profile"), "rm01-org", "rm01-user")
}

fn assemble(root: &Path) -> EngineCore {
    let accounts = AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    };
    accounts
        .disable_claude_keychain_fallback()
        .expect("seal synthetic Claude home");
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        profile(root),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts,
    )
    .expect("assemble profile")
}

fn authority_path(root: &Path) -> PathBuf {
    profile(root).store_root().join("remote-auth.json")
}

async fn shutdown(core: EngineCore) {
    core.shutdown().await;
    drop(core);
}

/// A1: a store written by v1 (no per-session last-use time) migrates to v2
/// in one private, atomic publish, and its paired session keeps working.
#[tokio::test]
async fn a_v1_authority_store_migrates_once_and_keeps_its_sessions() {
    let root = tempfile::tempdir().unwrap();
    let core = assemble(root.path());
    let local = luna_rpc::memory_client(core.rpc_service());
    let issued: IssuedPairing = local
        .call_as(
            methods::CREATE_REMOTE_PAIRING,
            json!({"label": "phone", "scopes": ["environment-read"],
                "pairingTtlSecs": 300, "sessionTtlSecs": 3600}),
        )
        .await
        .unwrap();
    let remote = luna_rpc::memory_client(core.remote_rpc_service().unwrap());
    let session: IssuedSession = remote
        .redeem_remote_pairing(luna_rpc::remote_auth::RedeemPairing {
            target: issued.target.clone(),
            credential: issued.credential.clone(),
            client_label: "phone".into(),
        })
        .await
        .unwrap();
    shutdown(core).await;

    // Rewrite the store exactly as v1 wrote it.
    let path = authority_path(root.path());
    let mut state: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    state["version"] = json!(1);
    for stored in state["sessions"].as_array_mut().unwrap() {
        stored["view"]
            .as_object_mut()
            .unwrap()
            .remove("lastUsedAtMs");
    }
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

    let reopened = assemble(root.path());
    let migrated: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(migrated["version"], 2, "the store migrates on open");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the migrated store stays private");
    }
    let remote = luna_rpc::memory_client(reopened.remote_rpc_service().unwrap());
    let view: SessionView = remote
        .authenticate_remote(luna_rpc::remote_auth::AuthenticateRemote {
            target: session.target.clone(),
            session_token: session.session_token.clone(),
        })
        .await
        .expect("the v1 session survives the migration");
    assert_eq!(view.id, session.session.id);
    remote
        .call(methods::ENGINE_INFO, json!({}))
        .await
        .expect("its scope still admits calls");
    shutdown(reopened).await;
}

/// A2: a corrupt store disables remote access with its reason, keeps its
/// bytes, and only an explicit local reset quarantines it and starts over.
#[tokio::test]
async fn a_corrupt_authority_store_stays_refused_until_an_explicit_reset() {
    let root = tempfile::tempdir().unwrap();
    let path = authority_path(root.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let garbage = b"{\"version\": 1, not json";
    std::fs::write(&path, garbage).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let core = assemble(root.path());
    let local = luna_rpc::memory_client(core.rpc_service());
    local
        .call(methods::ENGINE_INFO, json!({}))
        .await
        .expect("local work is unaffected");
    assert!(core.remote_rpc_service().is_none(), "no remote transport");
    let refused = local
        .call(methods::LIST_REMOTE_SESSIONS, json!({}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("invalid remote authority state"),
        "the reason is reported: {refused}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), garbage, "nothing rewrote it");

    let reset = local
        .call("ResetRemoteAuthority", json!({}))
        .await
        .expect("an explicit reset quarantines the store");
    let quarantined = PathBuf::from(reset["quarantined"].as_str().unwrap());
    assert_eq!(std::fs::read(&quarantined).unwrap(), garbage, "bytes kept");
    assert!(!path.exists());
    assert_eq!(reset["restartRequired"], true);
    shutdown(core).await;

    let reopened = assemble(root.path());
    let local = luna_rpc::memory_client(reopened.rpc_service());
    local
        .call(
            methods::CREATE_REMOTE_PAIRING,
            json!({"label": "phone", "scopes": ["environment-read"],
                "pairingTtlSecs": 300, "sessionTtlSecs": 3600}),
        )
        .await
        .expect("a fresh store pairs again");
    let healthy = local
        .call("ResetRemoteAuthority", json!({}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        healthy.contains("healthy"),
        "a healthy store is never reset: {healthy}"
    );
    shutdown(reopened).await;
}
