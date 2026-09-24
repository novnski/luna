use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use luna_relay::protocol::{Control, VERSION, proof};
use luna_relay::{ClientKey, RelayConfig, RouteConfig, Target};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use zeron_proto::WorkspaceScope;
use zeron_rpc::remote_auth::{
    AuthenticateRemote, CreatePairing, RedeemPairing, RemoteAuthStore, RemoteEnvironment,
    RemoteRpcService, RemoteScope,
};
use zeron_rpc::{RpcClient, RpcError, RpcReply, RpcService, methods};

const HOST_KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CLIENT_KEY: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct FakeEngine;

#[async_trait::async_trait]
impl RpcService for FakeEngine {
    async fn handle(&self, method: &str, _params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            methods::ENGINE_INFO => RpcReply::value(&serde_json::json!({"deviceId":"machine-a"})),
            methods::GET_TASK_IDENTITY => RpcReply::value(&serde_json::json!({"taskId":"task-a"})),
            methods::EXECUTE_TASK_COMMAND => panic!("remote write reached inner engine"),
            _ => Err(RpcError::UnknownMethod(method.to_owned())),
        }
    }
}

fn target() -> Target {
    Target {
        device_id: "machine-a".into(),
        workspace_scope: WorkspaceScope::Development,
        environment_id: "env-a".into(),
        protocol_version: zeron_rpc::remote_auth::PROTOCOL_VERSION,
        required_capabilities: vec![zeron_rpc::remote_auth::CAPABILITY.into()],
    }
}

fn config() -> RelayConfig {
    RelayConfig {
        version: VERSION,
        routes: vec![RouteConfig {
            id: "env-a".into(),
            target: target(),
            host_secret: HOST_KEY.into(),
            clients: vec![ClientKey {
                id: "viewer".into(),
                secret: CLIENT_KEY.into(),
            }],
        }],
    }
}

fn private_json(path: &Path, value: &impl serde::Serialize) {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).unwrap();
    file.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
    file.sync_all().unwrap();
}

async fn port() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

async fn pair(
    auth: &Arc<RemoteAuthStore>,
    label: &str,
    scopes: Vec<RemoteScope>,
) -> zeron_rpc::remote_auth::IssuedPairing {
    auth.create_pairing(CreatePairing {
        label: label.into(),
        scopes,
        pairing_ttl_secs: 300,
        session_ttl_secs: 300,
    })
    .unwrap()
}

async fn connect_paired(
    port: u16,
    pair: &zeron_rpc::remote_auth::IssuedPairing,
) -> (RpcClient, zeron_rpc::remote_auth::IssuedSession) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(client) = zeron_rpc::connect_ws(&format!("ws://127.0.0.1:{port}")).await
            && let Ok(session) = client
                .redeem_remote_pairing(RedeemPairing {
                    target: pair.target.clone(),
                    credential: pair.credential.clone(),
                    client_label: pair.pairing.label.clone(),
                })
                .await
        {
            client
                .authenticate_remote(AuthenticateRemote {
                    target: pair.target.clone(),
                    session_token: session.session_token.clone(),
                })
                .await
                .unwrap();
            return (client, session);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "relay did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
}

async fn raw_host(
    base: &str,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    u64,
) {
    let path = "/v1/host/env-a";
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("{base}{path}"))
        .await
        .unwrap();
    let Some(Ok(Message::Text(challenge))) = ws.next().await else {
        panic!("missing host challenge")
    };
    let Control::Challenge { nonce } = serde_json::from_str(&challenge).unwrap() else {
        panic!("wrong host challenge")
    };
    ws.send(Message::Text(
        serde_json::to_string(&Control::Hello {
            version: VERSION,
            target: target(),
            client_id: None,
            generation: 0,
            proof: proof(HOST_KEY, &nonce, path, &target(), 0).unwrap(),
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    let Some(Ok(Message::Text(ready))) = ws.next().await else {
        panic!("missing host ready")
    };
    let Control::Ready { generation } = serde_json::from_str(&ready).unwrap() else {
        panic!("wrong host ready")
    };
    (ws, generation)
}

#[tokio::test]
async fn exact_route_keeps_l29_principals_separate_and_revocation_closes_active_routes() {
    let root = TempDir::new().unwrap();
    let authority_root = root.path().join("authority");
    std::fs::create_dir(&authority_root).unwrap();
    let auth = RemoteAuthStore::open(
        &authority_root,
        RemoteEnvironment {
            device_id: "machine-a".into(),
            workspace_scope: WorkspaceScope::Development,
            environment_id: "env-a".into(),
        },
    )
    .unwrap();
    let (guarded, guarded_port) = port().await;
    let auth_for_factory = auth.clone();
    let factory: Arc<dyn Fn() -> Arc<dyn RpcService> + Send + Sync> = Arc::new(move || {
        Arc::new(RemoteRpcService::new(
            Arc::new(FakeEngine),
            auth_for_factory.clone(),
        ))
    });
    let guarded_task = tokio::spawn(zeron_rpc::serve_ws_listener_with_factory(guarded, factory));
    let config_path = root.path().join("relay.json");
    private_json(&config_path, &config());
    let (relay_listener, relay_port) = port().await;
    let (relay_stop_tx, mut relay_stop) = watch::channel(false);
    let relay_task = tokio::spawn(luna_relay::serve(
        relay_listener,
        config_path.clone(),
        async move {
            let _ = relay_stop.changed().await;
        },
    ));
    let base = format!("ws://127.0.0.1:{relay_port}");
    let (host_stop_tx, host_stop) = watch::channel(false);
    let host_task = tokio::spawn({
        let base = base.clone();
        async move {
            luna_relay::run_host(&base, "env-a", &target(), HOST_KEY, guarded_port, host_stop).await
        }
    });
    let (forward_listener, forward_port) = port().await;
    let (forward_stop_tx, mut forward_stop) = watch::channel(false);
    let forward_task = tokio::spawn({
        let base = base.clone();
        async move {
            luna_relay::run_client_forward(
                forward_listener,
                base,
                "env-a".into(),
                target(),
                "viewer".into(),
                CLIENT_KEY.into(),
                async move {
                    let _ = forward_stop.changed().await;
                },
            )
            .await
        }
    });

    let pair_a = pair(&auth, "A", vec![RemoteScope::EnvironmentRead]).await;
    let (client_a, session_a) = connect_paired(forward_port, &pair_a).await;
    let client_b = zeron_rpc::connect_ws(&format!("ws://127.0.0.1:{forward_port}"))
        .await
        .unwrap();
    assert!(
        client_b
            .call(methods::ENGINE_INFO, serde_json::json!({}))
            .await
            .is_err()
    );
    let pair_b = pair(&auth, "B", vec![RemoteScope::TasksRead]).await;
    let session_b = client_b
        .redeem_remote_pairing(RedeemPairing {
            target: pair_b.target.clone(),
            credential: pair_b.credential,
            client_label: "B".into(),
        })
        .await
        .unwrap();
    client_b
        .authenticate_remote(AuthenticateRemote {
            target: pair_b.target,
            session_token: session_b.session_token,
        })
        .await
        .unwrap();
    let info: serde_json::Value = client_a
        .call_as(methods::ENGINE_INFO, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(info["deviceId"], "machine-a");
    assert!(
        client_b
            .call(methods::ENGINE_INFO, serde_json::json!({}))
            .await
            .is_err()
    );
    assert!(
        client_a
            .call(methods::GET_TASK_IDENTITY, serde_json::json!({}))
            .await
            .is_err()
    );
    assert!(
        client_b
            .call(methods::EXECUTE_TASK_COMMAND, serde_json::json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("local only")
    );
    let identity: serde_json::Value = client_b
        .call_as(methods::GET_TASK_IDENTITY, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(identity["taskId"], "task-a");
    assert!(auth.revoke_session(&session_a.session.id).unwrap());
    assert!(
        client_a
            .call(methods::ENGINE_INFO, serde_json::json!({}))
            .await
            .is_err()
    );
    let _: serde_json::Value = client_b
        .call_as(methods::GET_TASK_IDENTITY, serde_json::json!({}))
        .await
        .unwrap();

    // A replacement control tunnel closes old bridges; the environment
    // sidecar must reconnect and establish a fresh generation.
    let (replacement, _) = raw_host(&base).await;
    drop(replacement);
    let pair_c = pair(&auth, "C", vec![RemoteScope::TasksRead]).await;
    let (client_c, _) = connect_paired(forward_port, &pair_c).await;
    let _: serde_json::Value = client_c
        .call_as(methods::GET_TASK_IDENTITY, serde_json::json!({}))
        .await
        .unwrap();

    let mut revoked = config();
    revoked.routes[0].clients.clear();
    private_json(&config_path, &revoked);
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert!(
        client_c
            .call(methods::GET_TASK_IDENTITY, serde_json::json!({}))
            .await
            .is_err()
    );
    assert!(
        luna_relay::client_socket(&base, "env-a", &target(), "viewer", CLIENT_KEY)
            .await
            .is_err()
    );

    forward_stop_tx.send_replace(true);
    host_stop_tx.send_replace(true);
    relay_stop_tx.send_replace(true);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), forward_task)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(3), host_task)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(3), relay_task)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    guarded_task.abort();
}

#[tokio::test]
async fn host_rejects_local_owner_ipc_even_with_valid_relay_credentials() {
    let (owner_listener, owner_port) = port().await;
    let owner_task = tokio::spawn(zeron_rpc::serve_ws_listener(
        owner_listener,
        Arc::new(FakeEngine),
    ));
    let (_stop_tx, stop) = watch::channel(false);
    let error = luna_relay::run_host(
        "ws://127.0.0.1:1",
        "env-a",
        &target(),
        HOST_KEY,
        owner_port,
        stop,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("not a L29 guarded"));
    owner_task.abort();
}

#[tokio::test]
async fn route_authentication_rejects_wrong_role_target_and_replayed_challenge() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("relay.json");
    private_json(&path, &config());
    let (listener, port) = port().await;
    let (stop_tx, mut stop) = watch::channel(false);
    let task = tokio::spawn(luna_relay::serve(listener, path, async move {
        let _ = stop.changed().await;
    }));
    let base = format!("ws://127.0.0.1:{port}");
    let mut wrong = target();
    wrong.environment_id = "other".into();
    assert!(
        luna_relay::client_socket(&base, "env-a", &wrong, "viewer", CLIENT_KEY)
            .await
            .is_err()
    );
    assert!(
        luna_relay::client_socket(&base, "env-a", &target(), "viewer", HOST_KEY)
            .await
            .is_err()
    );
    assert!(
        luna_relay::client_socket(&base, "env-a", &target(), "unknown", CLIENT_KEY)
            .await
            .is_err()
    );
    assert!(
        luna_relay::client_socket("ws://example.com", "env-a", &target(), "viewer", CLIENT_KEY)
            .await
            .is_err()
    );
    let url = format!("{base}/v1/client/env-a/viewer");
    let (mut first, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let Some(Ok(Message::Text(challenge))) = first.next().await else {
        panic!("missing challenge")
    };
    let Control::Challenge { nonce } = serde_json::from_str(&challenge).unwrap() else {
        panic!("wrong challenge")
    };
    let old_proof = proof(CLIENT_KEY, &nonce, "/v1/client/env-a/viewer", &target(), 0).unwrap();
    drop(first);
    let (mut second, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let Some(Ok(Message::Text(challenge))) = second.next().await else {
        panic!("missing second challenge")
    };
    let Control::Challenge { nonce: newer } = serde_json::from_str(&challenge).unwrap() else {
        panic!("wrong second challenge")
    };
    assert_ne!(nonce, newer);
    second
        .send(Message::Text(
            serde_json::to_string(&Control::Hello {
                version: VERSION,
                target: target(),
                client_id: Some("viewer".into()),
                generation: 0,
                proof: old_proof,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    let Some(Ok(Message::Text(response))) = second.next().await else {
        panic!("missing rejection")
    };
    assert!(matches!(
        serde_json::from_str::<Control>(&response).unwrap(),
        Control::Rejected { .. }
    ));
    let mut browser = url.as_str().into_client_request().unwrap();
    browser
        .headers_mut()
        .insert("Origin", "https://evil.example".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(browser).await.is_err());
    let (mut incompatible, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let Some(Ok(Message::Text(challenge))) = incompatible.next().await else {
        panic!("missing version challenge")
    };
    let Control::Challenge { nonce } = serde_json::from_str(&challenge).unwrap() else {
        panic!("wrong version challenge")
    };
    incompatible
        .send(Message::Text(
            serde_json::to_string(&Control::Hello {
                version: VERSION + 1,
                target: target(),
                client_id: Some("viewer".into()),
                generation: 0,
                proof: proof(CLIENT_KEY, &nonce, "/v1/client/env-a/viewer", &target(), 0).unwrap(),
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    let Some(Ok(Message::Text(response))) = incompatible.next().await else {
        panic!("missing version rejection")
    };
    assert!(matches!(
        serde_json::from_str::<Control>(&response).unwrap(),
        Control::Rejected { .. }
    ));
    stop_tx.send_replace(true);
    assert!(task.await.unwrap().is_ok());
}

#[tokio::test]
async fn replacement_host_fences_old_pending_bridge_and_client() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("relay.json");
    private_json(&path, &config());
    let (listener, port) = port().await;
    let (stop_tx, mut stop) = watch::channel(false);
    let task = tokio::spawn(luna_relay::serve(listener, path, async move {
        let _ = stop.changed().await;
    }));
    let base = format!("ws://127.0.0.1:{port}");
    let (mut old_host, old_generation) = raw_host(&base).await;
    let client = tokio::spawn({
        let base = base.clone();
        async move { luna_relay::client_socket(&base, "env-a", &target(), "viewer", CLIENT_KEY).await }
    });
    let open = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(Ok(Message::Text(text))) = old_host.next().await
                && let Ok(Control::Open {
                    stream_id,
                    generation,
                }) = serde_json::from_str(&text)
            {
                return (stream_id, generation);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(open.1, old_generation);
    let (_new_host, new_generation) = raw_host(&base).await;
    assert_ne!(old_generation, new_generation);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), client)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let path = format!("/v1/bridge/env-a/{}", open.0);
    let (mut old_bridge, _) = tokio_tungstenite::connect_async(format!("{base}{path}"))
        .await
        .unwrap();
    let Some(Ok(Message::Text(challenge))) = old_bridge.next().await else {
        panic!("missing bridge challenge")
    };
    let Control::Challenge { nonce } = serde_json::from_str(&challenge).unwrap() else {
        panic!("wrong bridge challenge")
    };
    old_bridge
        .send(Message::Text(
            serde_json::to_string(&Control::Hello {
                version: VERSION,
                target: target(),
                client_id: None,
                generation: old_generation,
                proof: proof(HOST_KEY, &nonce, &path, &target(), old_generation).unwrap(),
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(3), old_bridge.next())
        .await
        .unwrap();
    assert!(
        !matches!(reply, Some(Ok(Message::Text(text))) if matches!(serde_json::from_str::<Control>(&text), Ok(Control::Ready { .. })))
    );
    stop_tx.send_replace(true);
    assert!(task.await.unwrap().is_ok());
}

#[tokio::test]
async fn actual_relay_host_client_binaries_start_route_and_stop() {
    let root = TempDir::new().unwrap();
    let authority_root = root.path().join("authority");
    std::fs::create_dir(&authority_root).unwrap();
    let auth = RemoteAuthStore::open(
        &authority_root,
        RemoteEnvironment {
            device_id: "machine-a".into(),
            workspace_scope: WorkspaceScope::Development,
            environment_id: "env-a".into(),
        },
    )
    .unwrap();
    let (guarded, guarded_port) = port().await;
    let auth_for_factory = auth.clone();
    let factory: Arc<dyn Fn() -> Arc<dyn RpcService> + Send + Sync> = Arc::new(move || {
        Arc::new(RemoteRpcService::new(
            Arc::new(FakeEngine),
            auth_for_factory.clone(),
        ))
    });
    let guarded_task = tokio::spawn(zeron_rpc::serve_ws_listener_with_factory(guarded, factory));
    let config_path = root.path().join("relay.json");
    let target_path = root.path().join("target.json");
    let host_key_path = root.path().join("host.key");
    let client_key_path = root.path().join("client.key");
    private_json(&config_path, &config());
    private_json(&target_path, &target());
    private_text(&host_key_path, HOST_KEY);
    private_text(&client_key_path, CLIENT_KEY);
    let (_, relay_port) = port().await;
    let (_, forward_port) = port().await;
    let binary = env!("CARGO_BIN_EXE_luna-relay");
    let mut server = tokio::process::Command::new(binary)
        .args([
            "serve",
            "--config",
            config_path.to_str().unwrap(),
            "--bind",
            &format!("127.0.0.1:{relay_port}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let base = format!("ws://127.0.0.1:{relay_port}");
    let mut host = tokio::process::Command::new(binary)
        .args([
            "host",
            "--server",
            &base,
            "--route",
            "env-a",
            "--target-file",
            target_path.to_str().unwrap(),
            "--host-secret-file",
            host_key_path.to_str().unwrap(),
            "--guarded-rpc-port",
            &guarded_port.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut viewer = tokio::process::Command::new(binary)
        .args([
            "client",
            "--server",
            &base,
            "--route",
            "env-a",
            "--target-file",
            target_path.to_str().unwrap(),
            "--client-id",
            "viewer",
            "--client-secret-file",
            client_key_path.to_str().unwrap(),
            "--bind",
            &format!("127.0.0.1:{forward_port}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pairing = pair(&auth, "binary", vec![RemoteScope::EnvironmentRead]).await;
    let (client, _) = connect_paired(forward_port, &pairing).await;
    let info: serde_json::Value = client
        .call_as(methods::ENGINE_INFO, serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(info["deviceId"], "machine-a");
    for child in [&mut viewer, &mut host, &mut server] {
        child.start_kill().unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .unwrap()
                .is_ok()
        );
    }
    assert!(
        client
            .call(methods::ENGINE_INFO, serde_json::json!({}))
            .await
            .is_err()
    );
    guarded_task.abort();
}

fn private_text(path: &Path, value: &str) {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .unwrap()
        .write_all(value.as_bytes())
        .unwrap();
}

#[test]
fn config_rejects_scope_sprawl_and_reused_roles() {
    let mut bad = config();
    bad.routes[0].clients[0].secret = HOST_KEY.into();
    assert!(bad.validate().is_err());
    let mut bad = config();
    bad.routes[0].target.protocol_version += 1;
    assert!(bad.validate().is_err());
    let mut bad = config();
    bad.routes[0].id = "../../ipc".into();
    assert!(bad.validate().is_err());
    assert!(
        proof(
            "too-long".repeat(20).as_str(),
            &"a".repeat(64),
            "/v1/host/env-a",
            &target(),
            0
        )
        .is_err()
    );
}
