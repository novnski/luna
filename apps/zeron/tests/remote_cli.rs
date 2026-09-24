//! Actual `luna remote` binary against a copy-isolated synthetic engine.

use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;
use tokio::net::TcpListener;
use zeron_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessId, HarnessRegistry,
};
use zeron_rpc::remote_auth::{RedeemPairing, RemoteTarget};

fn synthetic_accounts(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
        cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
    }
}

async fn cli(root: &Path, port: u16, arguments: &[&str]) -> std::process::Output {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_luna"))
        .args(["remote", "--port", &port.to_string()])
        .args(arguments)
        .env("LUNA_DATA_DIR", root.join("child-data"))
        .env("CODEX_HOME", root.join("synthetic-codex"))
        .env("CLAUDE_CONFIG_DIR", root.join("synthetic-claude"))
        .env("LUNA_WORKOS_CLIENT_ID", "")
        .env("LUNA_EDGE_URL", "http://127.0.0.1:1")
        .output()
        .await
        .unwrap()
}

#[tokio::test]
async fn remote_cli_issues_lists_and_revokes_one_real_engine_session() {
    let root = TempDir::new().unwrap();
    let accounts = synthetic_accounts(root.path());
    accounts.disable_claude_keychain_fallback().unwrap();
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.path().join("data"), "l29-org", "l29-user"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap();
    let mut auth = AuthConfig::new("http://127.0.0.1:1", root.path().join("data"));
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(zeron_rpc::serve_ws_listener(listener, core.rpc_service()));

    let issued = cli(
        root.path(),
        port,
        &[
            "pair",
            "--label",
            "synthetic phone",
            "--scope",
            "environment-read",
        ],
    )
    .await;
    assert!(
        issued.status.success(),
        "{}",
        String::from_utf8_lossy(&issued.stderr)
    );
    let printed = String::from_utf8(issued.stdout).unwrap();
    assert!(printed.contains(&core.device_id));
    let credential = printed
        .lines()
        .find_map(|line| line.strip_prefix("Credential (shown once): "))
        .unwrap();
    let environment_id = printed
        .lines()
        .find_map(|line| line.strip_prefix("Environment: "))
        .unwrap();
    let local = zeron_rpc::connect_ws(&format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let info: zeron_proto::EngineInfo = local
        .call_as(zeron_rpc::methods::ENGINE_INFO, serde_json::json!({}))
        .await
        .unwrap();
    let target = RemoteTarget {
        device_id: info.device_id,
        workspace_scope: info.workspace_scope,
        environment_id: environment_id.into(),
        protocol_version: zeron_rpc::remote_auth::PROTOCOL_VERSION,
        required_capabilities: vec![zeron_rpc::remote_auth::CAPABILITY.into()],
    };
    let remote = zeron_rpc::memory_client(core.remote_rpc_service().unwrap());
    let session = remote
        .redeem_remote_pairing(RedeemPairing {
            target: target.clone(),
            credential: credential.into(),
            client_label: "synthetic phone".into(),
        })
        .await
        .unwrap();
    remote
        .authenticate_remote(zeron_rpc::remote_auth::AuthenticateRemote {
            target,
            session_token: session.session_token,
        })
        .await
        .unwrap();
    let sessions = cli(root.path(), port, &["sessions"]).await;
    assert!(sessions.status.success());
    assert!(String::from_utf8_lossy(&sessions.stdout).contains(&session.session.id));
    let revoked = cli(root.path(), port, &["revoke-session", &session.session.id]).await;
    assert!(revoked.status.success());
    assert!(String::from_utf8_lossy(&revoked.stdout).contains("session revoked: true"));
    assert!(
        remote
            .call(zeron_rpc::methods::ENGINE_INFO, serde_json::json!({}))
            .await
            .is_err()
    );

    server.abort();
    core.shutdown().await;
}
