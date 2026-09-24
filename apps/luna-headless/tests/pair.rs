//! Minimal binary pairing uses only the local-owner RPC of an isolated engine.

use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;
use tokio::net::TcpListener;
use zeron_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessId, HarnessRegistry,
};
use zeron_rpc::remote_auth::{IssuedPairing, RedeemPairing, RemoteScope};

fn accounts(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
        cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
    }
}

#[tokio::test]
async fn minimal_binary_issues_one_time_pairing_for_exact_engine() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let accounts = accounts(root);
    accounts.disable_claude_keychain_fallback().unwrap();
    let core = EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(&root.join("host-profile"), "l30-org", "l30-user"),
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
        accounts,
    )
    .unwrap();
    let mut config = AuthConfig::new("http://127.0.0.1:1", root.join("host-profile"));
    config.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(zeron_rpc::serve_ws_listener(listener, core.rpc_service()));

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna-headless"))
        .args([
            "pair",
            "--ipc-port",
            &port.to_string(),
            "--label",
            "synthetic client",
            "--scope",
            "environment-read",
            "--scope",
            "tasks-read",
        ])
        .env("LUNA_DATA_DIR", root.join("child-profile"))
        .env("CODEX_HOME", root.join("synthetic-codex"))
        .env("CLAUDE_CONFIG_DIR", root.join("synthetic-claude"))
        .env("LUNA_WORKOS_CLIENT_ID", "")
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let issued: IssuedPairing = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(issued.target.device_id, core.device_id);
    assert_eq!(
        issued.pairing.scopes,
        vec![RemoteScope::EnvironmentRead, RemoteScope::TasksRead]
    );
    let client = zeron_rpc::memory_client(core.remote_rpc_service().unwrap());
    let session = client
        .redeem_remote_pairing(RedeemPairing {
            target: issued.target.clone(),
            credential: issued.credential,
            client_label: "synthetic client".into(),
        })
        .await
        .unwrap();
    assert!(!session.session_token.is_empty());

    let wrong = tokio::process::Command::new(env!("CARGO_BIN_EXE_luna-headless"))
        .args([
            "pair",
            "--ipc-port",
            &port.to_string(),
            "--label",
            "wrong target",
            "--scope",
            "environment-read",
            "--expect-machine",
            "11111111-1111-4111-8111-111111111111",
            "--expect-environment",
            &issued.target.environment_id,
            "--expect-workspace-scope",
            "development",
        ])
        .output()
        .await
        .unwrap();
    assert!(!wrong.status.success());
    assert!(
        wrong.stdout.is_empty(),
        "mismatched pairing credential must not be printed"
    );
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("issued grant revoked"));
    let local = zeron_rpc::memory_client(core.rpc_service());
    let pairings: Vec<zeron_rpc::remote_auth::PairingView> = local
        .call_as(
            zeron_rpc::methods::LIST_REMOTE_PAIRINGS,
            serde_json::json!({}),
        )
        .await
        .unwrap();
    assert!(pairings.is_empty(), "mismatched grant must be revoked");
    server.abort();
}
