//! L29 through the shipped EngineRpc and the same guarded dispatch used by
//! device-room virtual connections. All roots and credentials are synthetic.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::json;
use tempfile::TempDir;
use zeron_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessId, HarnessRegistry,
};
use zeron_proto::{EngineInfo, WorkspaceScope};
use zeron_rpc::remote_auth::{
    AuthenticateRemote, CreatePairing, IssuedPairing, IssuedSession, RedeemPairing, RemoteScope,
    RemoteTarget, SessionView,
};
use zeron_rpc::{RpcClient, methods};

fn synthetic_accounts(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
        cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
    }
}

fn open_core(root: &Path) -> EngineCore {
    open_core_profile(root, "l29-org", "l29-user")
}

fn open_core_profile(root: &Path, org: &str, user: &str) -> EngineCore {
    let accounts = synthetic_accounts(root);
    accounts.disable_claude_keychain_fallback().unwrap();
    let data_root = root.join("data");
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&data_root, org, user),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap();
    let mut auth = AuthConfig::new("http://127.0.0.1:1", data_root);
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    core
}

async fn call<T: DeserializeOwned>(
    client: &RpcClient,
    method: &str,
    params: serde_json::Value,
) -> T {
    serde_json::from_value(client.call(method, params).await.unwrap()).unwrap()
}

async fn issue(
    local: &RpcClient,
    label: &str,
    scopes: Vec<RemoteScope>,
    session_ttl_secs: u64,
) -> IssuedPairing {
    call(
        local,
        methods::CREATE_REMOTE_PAIRING,
        serde_json::to_value(CreatePairing {
            label: label.into(),
            scopes,
            pairing_ttl_secs: 300,
            session_ttl_secs,
        })
        .unwrap(),
    )
    .await
}

async fn redeem(client: &RpcClient, pairing: &IssuedPairing, label: &str) -> IssuedSession {
    client
        .redeem_remote_pairing(RedeemPairing {
            target: pairing.target.clone(),
            credential: pairing.credential.clone(),
            client_label: label.into(),
        })
        .await
        .unwrap()
}

async fn authenticate(client: &RpcClient, target: &RemoteTarget, token: &str) -> SessionView {
    client
        .authenticate_remote(AuthenticateRemote {
            target: target.clone(),
            session_token: token.into(),
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn two_clients_pair_scope_revoke_stream_and_restart() {
    let root = TempDir::new().unwrap();
    let core = open_core(root.path());
    let local = zeron_rpc::memory_client(core.rpc_service());
    let remote_a = zeron_rpc::memory_client(core.remote_rpc_service().unwrap());
    let remote_b = zeron_rpc::memory_client(core.remote_rpc_service().unwrap());

    let info: EngineInfo = call(&local, methods::ENGINE_INFO, json!({})).await;
    assert_eq!(info.workspace_scope, WorkspaceScope::Development);
    assert!(info.supports(zeron_proto::capabilities::REMOTE_AUTH_V1));
    assert!(
        remote_a
            .call(methods::ENGINE_INFO, json!({}))
            .await
            .is_err()
    );
    assert!(
        remote_a
            .call(methods::CREATE_REMOTE_PAIRING, json!({}))
            .await
            .is_err()
    );

    let pair_a = issue(&local, "viewer A", vec![RemoteScope::EnvironmentRead], 300).await;
    let pair_b = issue(&local, "viewer B", vec![RemoteScope::TasksRead], 300).await;
    assert_eq!(pair_a.target.device_id, info.device_id);
    let mut wrong = pair_a.target.clone();
    wrong.device_id = "wrong-device".into();
    assert!(
        remote_a
            .call(
                methods::REDEEM_REMOTE_PAIRING,
                serde_json::to_value(RedeemPairing {
                    target: wrong,
                    credential: pair_a.credential.clone(),
                    client_label: "A".into(),
                })
                .unwrap()
            )
            .await
            .is_err()
    );
    let mut wrong_protocol = pair_a.target.clone();
    wrong_protocol.protocol_version += 1;
    assert!(
        remote_a
            .call(
                methods::REDEEM_REMOTE_PAIRING,
                serde_json::to_value(RedeemPairing {
                    target: wrong_protocol,
                    credential: pair_a.credential.clone(),
                    client_label: "A".into(),
                })
                .unwrap()
            )
            .await
            .is_err()
    );

    let session_a = redeem(&remote_a, &pair_a, "A").await;
    let session_b = redeem(&remote_b, &pair_b, "B").await;
    assert!(
        remote_b
            .call(
                methods::REDEEM_REMOTE_PAIRING,
                serde_json::to_value(RedeemPairing {
                    target: pair_a.target.clone(),
                    credential: pair_a.credential.clone(),
                    client_label: "replay".into(),
                })
                .unwrap()
            )
            .await
            .is_err()
    );
    authenticate(&remote_a, &pair_a.target, &session_a.session_token).await;
    authenticate(&remote_b, &pair_b.target, &session_b.session_token).await;
    let mut compatible = pair_a.target.clone();
    compatible
        .required_capabilities
        .push(zeron_proto::capabilities::TASK_IDENTITY_V1.into());
    authenticate(&remote_a, &compatible, &session_a.session_token).await;
    let mut unsupported = pair_a.target.clone();
    unsupported
        .required_capabilities
        .push("future-unavailable-capability".into());
    assert!(
        remote_a
            .authenticate_remote(AuthenticateRemote {
                target: unsupported,
                session_token: session_a.session_token.clone(),
            })
            .await
            .is_err()
    );
    assert!(
        remote_a
            .call(methods::ENGINE_INFO, json!({}))
            .await
            .is_err()
    );
    authenticate(&remote_a, &pair_a.target, &session_a.session_token).await;
    let _: EngineInfo = call(&remote_a, methods::ENGINE_INFO, json!({})).await;
    assert!(
        remote_b
            .call(methods::ENGINE_INFO, json!({}))
            .await
            .is_err()
    );
    for method in [
        methods::EXECUTE_TASK_COMMAND,
        methods::QUEUE_COMMAND,
        methods::RELAY_COMMAND,
        methods::WRITE_WORKSPACE_FILE,
    ] {
        let error = remote_b
            .call(method, json!({}))
            .await
            .expect_err("remote write denied");
        assert!(
            error.to_string().contains("local only"),
            "{method}: {error}"
        );
    }
    assert!(
        remote_a
            .call(methods::GET_TASK_IDENTITY, json!({ "taskId": "unknown" }))
            .await
            .is_err()
    );
    assert!(
        remote_a
            .call(methods::EXECUTE_TASK_COMMAND, json!({}))
            .await
            .is_err()
    );
    assert!(
        remote_a
            .call("FutureDangerousMethod", json!({}))
            .await
            .is_err()
    );
    assert!(
        remote_a
            .call(
                methods::ENGINE_INFO,
                json!({ "targetDeviceId": "other-device" })
            )
            .await
            .is_err()
    );

    let project = "29292929-2929-4292-8292-292929292929";
    let task = "30303030-3030-4030-8030-303030303030";
    let path = root.path().join("project");
    std::fs::create_dir_all(&path).unwrap();
    let _: serde_json::Value = call(
        &local,
        methods::MUTATE,
        json!({
            "op": "createSpace", "spaceId": project, "deviceId": info.device_id, "path": path,
        }),
    )
    .await;
    let _: serde_json::Value = call(&local, methods::MUTATE, json!({
        "op": "createChat", "chatId": task, "spaceId": project,
        "config": { "harness": "mock", "model": null, "reasoning": null, "sandbox": "workspace-write" },
    })).await;
    let mut stream = remote_b
        .subscribe(methods::WATCH_TASK_COMMANDS, json!({ "taskId": task }))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), stream.recv())
            .await
            .unwrap()
            .is_some()
    );

    let sessions: Vec<SessionView> = call(&local, methods::LIST_REMOTE_SESSIONS, json!({})).await;
    assert_eq!(sessions.len(), 2);
    let revoked: serde_json::Value = call(
        &local,
        methods::REVOKE_REMOTE_SESSION,
        json!({ "id": session_b.session.id }),
    )
    .await;
    assert_eq!(revoked["revoked"], true);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), stream.recv())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        remote_b
            .call(
                methods::GET_TASK_COMMAND_SNAPSHOT,
                json!({ "taskId": task })
            )
            .await
            .is_err()
    );
    let _: EngineInfo = call(&remote_a, methods::ENGINE_INFO, json!({})).await;

    let authority_path =
        EngineProfile::development(&root.path().join("data"), "l29-org", "l29-user")
            .store_root()
            .join("remote-auth.json");
    let store_text = std::fs::read_to_string(&authority_path).unwrap();
    assert!(!store_text.contains(&pair_a.credential));
    assert!(!store_text.contains(&session_a.session_token));
    core.shutdown().await;
    drop(stream);
    drop(remote_a);
    drop(remote_b);
    drop(local);
    drop(core);

    let reopened = open_core(root.path());
    assert_eq!(reopened.device_id, info.device_id);
    let reconnect_a = zeron_rpc::memory_client(reopened.remote_rpc_service().unwrap());
    let reconnect_b = zeron_rpc::memory_client(reopened.remote_rpc_service().unwrap());
    authenticate(&reconnect_a, &pair_a.target, &session_a.session_token).await;
    let _: EngineInfo = call(&reconnect_a, methods::ENGINE_INFO, json!({})).await;
    assert!(
        reconnect_b
            .call(
                methods::AUTHENTICATE_REMOTE,
                serde_json::to_value(AuthenticateRemote {
                    target: pair_b.target.clone(),
                    session_token: session_b.session_token.clone(),
                })
                .unwrap()
            )
            .await
            .is_err()
    );
    assert!(
        reconnect_b
            .call(
                methods::REDEEM_REMOTE_PAIRING,
                serde_json::to_value(RedeemPairing {
                    target: pair_a.target.clone(),
                    credential: pair_a.credential,
                    client_label: "replay".into(),
                })
                .unwrap()
            )
            .await
            .is_err()
    );
    reopened.shutdown().await;
}

#[tokio::test]
async fn expired_session_and_invalid_reauthentication_cannot_reuse_connection() {
    let root = TempDir::new().unwrap();
    let core = open_core(root.path());
    let local = zeron_rpc::memory_client(core.rpc_service());
    let remote = zeron_rpc::memory_client(core.remote_rpc_service().unwrap());
    let pair = issue(&local, "short", vec![RemoteScope::EnvironmentRead], 1).await;
    let session = redeem(&remote, &pair, "short client").await;
    authenticate(&remote, &pair.target, &session.session_token).await;
    let _: EngineInfo = call(&remote, methods::ENGINE_INFO, json!({})).await;
    let mut wrong = pair.target.clone();
    wrong.workspace_scope = WorkspaceScope::Local;
    assert!(
        remote
            .call(
                methods::AUTHENTICATE_REMOTE,
                serde_json::to_value(AuthenticateRemote {
                    target: wrong,
                    session_token: session.session_token.clone(),
                })
                .unwrap()
            )
            .await
            .is_err()
    );
    assert!(remote.call(methods::ENGINE_INFO, json!({})).await.is_err());
    authenticate(&remote, &pair.target, &session.session_token).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(remote.call(methods::ENGINE_INFO, json!({})).await.is_err());
    core.shutdown().await;
}

#[tokio::test]
async fn corrupt_remote_authority_disables_remote_without_blocking_local_engine() {
    let root = TempDir::new().unwrap();
    let profile = EngineProfile::development(&root.path().join("data"), "l29-org", "l29-user");
    std::fs::create_dir_all(profile.store_root()).unwrap();
    let authority_path = profile.store_root().join("remote-auth.json");
    std::fs::write(&authority_path, "invalid synthetic authority state").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&authority_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let core = open_core(root.path());
    assert!(core.remote_rpc_service().is_none());
    let local = zeron_rpc::memory_client(core.rpc_service());
    let info: EngineInfo = call(&local, methods::ENGINE_INFO, json!({})).await;
    assert_eq!(info.device_id, core.device_id);
    assert!(!info.supports(zeron_proto::capabilities::REMOTE_AUTH_V1));
    assert!(
        local
            .call(methods::CREATE_REMOTE_PAIRING, json!({}))
            .await
            .is_err()
    );
    core.shutdown().await;
}

#[tokio::test]
async fn same_machine_different_profile_has_distinct_remote_authority() {
    let root = TempDir::new().unwrap();
    let first = open_core(root.path());
    let local = zeron_rpc::memory_client(first.rpc_service());
    let pairing = issue(
        &local,
        "first profile",
        vec![RemoteScope::EnvironmentRead],
        300,
    )
    .await;
    let client = zeron_rpc::memory_client(first.remote_rpc_service().unwrap());
    let session = redeem(&client, &pairing, "first client").await;
    let device_id = first.device_id.clone();
    first.shutdown().await;
    drop(client);
    drop(local);
    drop(first);

    let second = open_core_profile(root.path(), "other-org", "other-user");
    assert_eq!(second.device_id, device_id);
    let second_local = zeron_rpc::memory_client(second.rpc_service());
    let second_pairing = issue(
        &second_local,
        "second profile",
        vec![RemoteScope::EnvironmentRead],
        300,
    )
    .await;
    assert_ne!(
        pairing.target.environment_id,
        second_pairing.target.environment_id
    );
    let second_client = zeron_rpc::memory_client(second.remote_rpc_service().unwrap());
    assert!(
        second_client
            .authenticate_remote(AuthenticateRemote {
                target: pairing.target.clone(),
                session_token: session.session_token.clone(),
            })
            .await
            .is_err()
    );
    assert!(
        second_client
            .authenticate_remote(AuthenticateRemote {
                target: second_pairing.target,
                session_token: session.session_token,
            })
            .await
            .is_err()
    );
    second.shutdown().await;
}
