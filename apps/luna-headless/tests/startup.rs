//! Actual headless binary with an explicitly isolated synthetic startup profile.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use luna_proto::EngineInfo;
use luna_rpc::remote_auth::{CreatePairing, IssuedPairing, RedeemPairing, RemoteScope};
use luna_rpc::{RpcClient, methods};
use tempfile::TempDir;
use tokio::process::{Child, Command};

async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

fn start(root: &Path, ipc: u16, remote: u16) -> Child {
    let binary = std::env::var_os("LUNA_HEADLESS_TEST_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_luna-headless").into());
    let mut command = Command::new(binary);
    command
        .args([
            "--synthetic-fixture-root",
            root.to_str().unwrap(),
            "--ipc-port",
            &ipc.to_string(),
            "--remote-port",
            &remote.to_string(),
        ])
        .env("LUNA_DATA_DIR", root.join("data"))
        .env("LUNA_WORKOS_CLIENT_ID", "")
        .env("LUNA_EDGE_URL", "http://127.0.0.1:1")
        .env("LUNA_HARNESS", "mock")
        .env("CODEX_HOME", root.join("synthetic-codex"))
        .env("CLAUDE_CONFIG_DIR", root.join("synthetic-claude"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.spawn().unwrap()
}

async fn wait_client(child: &mut Child, port: u16) -> RpcClient {
    let url = format!("ws://127.0.0.1:{port}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("synthetic headless exited before readiness: {status}");
        }
        if let Ok(client) = luna_rpc::connect_ws(&url).await {
            return client;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "headless did not open port {port}"
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
}

#[tokio::test]
async fn packaged_headless_startup_guards_remote_socket_and_shutdown() {
    let temp = TempDir::new().unwrap();
    let ipc = free_port().await;
    let remote = free_port().await;
    assert_ne!(ipc, remote);
    let mut child = start(temp.path(), ipc, remote);
    let local = wait_client(&mut child, ipc).await;
    let remote_client = wait_client(&mut child, remote).await;
    assert!(
        remote_client
            .call(methods::ENGINE_INFO, serde_json::json!({}))
            .await
            .is_err()
    );
    let info: EngineInfo = local
        .call_as(methods::ENGINE_INFO, serde_json::json!({}))
        .await
        .unwrap();
    let issued: IssuedPairing = local
        .call_as(
            methods::CREATE_REMOTE_PAIRING,
            serde_json::to_value(CreatePairing {
                label: "synthetic CLI".into(),
                scopes: vec![RemoteScope::EnvironmentRead, RemoteScope::TasksRead],
                pairing_ttl_secs: 300,
                session_ttl_secs: 300,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    let session = remote_client
        .redeem_remote_pairing(RedeemPairing {
            target: issued.target.clone(),
            credential: issued.credential,
            client_label: "synthetic client".into(),
        })
        .await
        .unwrap();
    remote_client
        .authenticate_remote(luna_rpc::remote_auth::AuthenticateRemote {
            target: issued.target,
            session_token: session.session_token,
        })
        .await
        .unwrap();
    let remote_info: EngineInfo = remote_client
        .call_as(methods::ENGINE_INFO, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(remote_info.device_id, info.device_id);
    let stranger = luna_rpc::connect_ws(&format!("ws://127.0.0.1:{remote}"))
        .await
        .unwrap();
    assert!(
        stranger
            .call(methods::ENGINE_INFO, serde_json::json!({}))
            .await
            .is_err()
    );
    let _: serde_json::Value = local
        .call_as(methods::STOP_ENGINE, serde_json::json!({}))
        .await
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(8), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn second_synthetic_headless_cannot_bind_same_remote_listener() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    let first_ipc = free_port().await;
    let second_ipc = free_port().await;
    let remote = free_port().await;
    let mut first_child = start(first.path(), first_ipc, remote);
    let local = wait_client(&mut first_child, first_ipc).await;
    let mut second_child = start(second.path(), second_ipc, remote);
    let status = tokio::time::timeout(Duration::from_secs(8), second_child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    let _: serde_json::Value = local
        .call_as(methods::STOP_ENGINE, serde_json::json!({}))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(8), first_child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
