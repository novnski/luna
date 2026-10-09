//! M4b integration: `targetDeviceId` routing — engine A forwards device-addressed RPCs
//! to engine B through B's device-room relay (host relay on B, link cache on A), with a
//! minimal in-memory device-room standing in for the edge DO (route client→host with
//! `from` stamped, host→client by `to`).

// tungstenite's `accept_hdr_async` callback signature fixes the Err type as a full
// `Response` — its size is not ours to shrink.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::server::{
    Request as WsRequest, Response as WsResponse,
};

use luna_engine::{
    AgentAccountsConfig, BranchHeadContext, ChangeRequestError, CheckoutChangeRequestLookup,
    CheckoutChangeRequests, CheckoutSourceContext, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::{Harness, HarnessError, RunControls};
use luna_proto::WorkspaceScope;
use luna_proto::{
    AgentEvent, ChangeRequestState, ChangeRequestSummary, DoneStatus, HarnessId, Model,
    ReasoningLevel, RunRequest, SteeringMode,
};
use luna_rpc::remote_auth::{
    AuthenticateRemote, CreatePairing, IssuedPairing, RedeemPairing, RemoteAuthStore,
    RemoteEnvironment, RemoteScope,
};
use luna_rpc::{
    DeviceFrameHeader, HostRelay, HostRelayConfig, LinkCache, LinkCacheConfig, RpcError, RpcReply,
    RpcService, StaticToken, decode_device_frame, encode_device_frame, methods,
};

// ---------------------------------------------------------------------------
// Minimal in-memory device room (route-only subset of the DO semantics)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RelayState {
    host: Option<mpsc::UnboundedSender<Vec<u8>>>,
    clients: HashMap<String, mpsc::UnboundedSender<Vec<u8>>>,
}

async fn fake_device_room() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind relay");
    let url = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().expect("addr").port()
    );
    let state = Arc::new(Mutex::new(RelayState::default()));
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let mut uri = String::new();
                let Ok(ws) = tokio_tungstenite::accept_hdr_async(
                    stream,
                    |req: &WsRequest, res: WsResponse| {
                        uri = req.uri().to_string();
                        Ok(res)
                    },
                )
                .await
                else {
                    return;
                };
                let query = uri.split_once('?').map(|(_, q)| q).unwrap_or("");
                let is_host = query.contains("role=host");
                let conn_id = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("connId="))
                    .unwrap_or("anon")
                    .to_string();
                let (mut sink, mut ws_stream) = ws.split();
                let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
                {
                    let mut st = state.lock().expect("lock");
                    if is_host {
                        st.host = Some(tx);
                    } else {
                        st.clients.insert(conn_id.clone(), tx);
                    }
                }
                let writer = tokio::spawn(async move {
                    while let Some(bytes) = rx.recv().await {
                        if sink.send(WsMessage::Binary(bytes)).await.is_err() {
                            break;
                        }
                    }
                });
                while let Some(Ok(message)) = ws_stream.next().await {
                    let WsMessage::Binary(bytes) = message else {
                        continue;
                    };
                    let Ok((header, payload)) = decode_device_frame(&bytes) else {
                        break;
                    };
                    let st = state.lock().expect("lock");
                    if is_host {
                        let Some(to) = header.to else { continue };
                        if let Some(client) = st.clients.get(&to) {
                            let stripped = DeviceFrameHeader::new(header.s, header.k);
                            let _ = client
                                .send(encode_device_frame(&stripped, &payload).expect("encode"));
                        }
                    } else if let Some(host) = &st.host {
                        let mut routed = DeviceFrameHeader::new(header.s, header.k);
                        routed.from = Some(conn_id.clone());
                        let _ = host.send(encode_device_frame(&routed, &payload).expect("encode"));
                    }
                }
                writer.abort();
            });
        }
    });
    (url, task)
}

// ---------------------------------------------------------------------------
// Engine fixtures
// ---------------------------------------------------------------------------

/// Instant mock harness: the engines under test never wait on a provider.
struct InstantHarness;

#[async_trait]
impl Harness for InstantHarness {
    fn id(&self) -> HarnessId {
        HarnessId::Mock
    }
    fn display_name(&self) -> &str {
        "Instant"
    }
    fn supports_steering(&self) -> bool {
        false
    }
    fn steering_mode(&self) -> SteeringMode {
        SteeringMode::TurnBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        &[]
    }
    async fn models(&self) -> Result<Vec<Model>, HarnessError> {
        Ok(vec![])
    }
    async fn run(
        &self,
        _request: RunRequest,
        _controls: RunControls,
    ) -> Result<BoxStream<'static, Result<AgentEvent, HarnessError>>, HarnessError> {
        Ok(futures::stream::iter([
            Ok(AgentEvent::SessionStarted {
                harness: HarnessId::Mock,
                model: "instant-1".into(),
                tools: vec![],
                cwd: "/tmp".into(),
                session_id: "hs-1".into(),
                assistant_message_id: "a-1".into(),
            }),
            Ok(AgentEvent::TextDelta {
                text: "remote reply".into(),
            }),
            Ok(AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: Some("hs-1".into()),
            }),
        ])
        .boxed())
    }
}

fn registry() -> Arc<HarnessRegistry> {
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(InstantHarness));
    Arc::new(registry)
}

fn assemble(dir: &std::path::Path, device_id: &str) -> EngineCore {
    std::fs::create_dir_all(dir).expect("create data dir");
    std::fs::write(dir.join("device-id"), device_id).expect("write device id");
    let accounts = AgentAccountsConfig {
        data_dir: dir.join("provider-state"),
        claude_config_dir: dir.join("synthetic-claude"),
        claude_config_file: dir.join("synthetic-claude.json"),
        codex_home: dir.join("synthetic-codex"),
    };
    accounts
        .disable_claude_keychain_fallback()
        .expect("synthetic accounts");
    EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
        EngineProfile::development(dir, "dev-org", "dev-user"),
        registry(),
        HarnessId::Mock,
        None,
        accounts,
    )
    .expect("engine assembles")
}

async fn paired_remote_session(core: &EngineCore) -> AuthenticateRemote {
    let local = luna_rpc::memory_client(core.rpc_service());
    let pairing: IssuedPairing = local.call_as(methods::CREATE_REMOTE_PAIRING, serde_json::json!({
        "label": "synthetic routed client", "scopes": ["environment-read", "tasks-read", "files-read", "terminal-operate"],
        "pairingTtlSecs": 300, "sessionTtlSecs": 3600,
    })).await.unwrap();
    let remote = luna_rpc::memory_client(core.remote_rpc_service().unwrap());
    let session = remote
        .redeem_remote_pairing(RedeemPairing {
            target: pairing.target,
            credential: pairing.credential,
            client_label: "synthetic routed client".into(),
        })
        .await
        .unwrap();
    AuthenticateRemote {
        target: session.target,
        session_token: session.session_token,
    }
}

fn session_from_store(store: &Arc<RemoteAuthStore>) -> AuthenticateRemote {
    let pairing = store
        .create_pairing(CreatePairing {
            label: "synthetic legacy reader".into(),
            scopes: vec![RemoteScope::EnvironmentRead, RemoteScope::FilesRead],
            pairing_ttl_secs: 300,
            session_ttl_secs: 3600,
        })
        .unwrap();
    let session = store
        .redeem(RedeemPairing {
            target: pairing.target,
            credential: pairing.credential,
            client_label: "synthetic legacy reader".into(),
        })
        .unwrap();
    AuthenticateRemote {
        target: session.target,
        session_token: session.session_token,
    }
}

fn attach_remote_session(config: &mut LinkCacheConfig, session: AuthenticateRemote) {
    config.remote_session = Some(Arc::new(move |device_id| {
        (device_id == session.target.device_id).then(|| session.clone())
    }));
}

async fn init_workspace_repo(path: &std::path::Path) {
    std::fs::create_dir_all(path.join("src")).expect("repo tree");
    let run = |args: &'static [&'static str]| async move {
        let output = tokio::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .output()
            .await
            .expect("git spawns");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["init", "-b", "main"]).await;
    std::fs::write(
        path.join("src/remote.rs"),
        "pub const REMOTE: bool = true;\n",
    )
    .expect("remote source");
    run(&["add", "."]).await;
    run(&["commit", "-m", "initial"]).await;
}

struct StaticChangeRequestLookup {
    source: CheckoutSourceContext,
    summary: ChangeRequestSummary,
    resolves: AtomicUsize,
}

/// Models a host from before the checkout change-request watch existed.
struct LegacyChangeRequestService {
    list_harnesses_calls: AtomicUsize,
}

#[async_trait]
impl RpcService for LegacyChangeRequestService {
    async fn handle(&self, method: &str, _params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            methods::LIST_HARNESSES => {
                self.list_harnesses_calls.fetch_add(1, Ordering::AcqRel);
                RpcReply::value(&serde_json::json!([]))
            }
            other => Err(RpcError::UnknownMethod(other.into())),
        }
    }
}

#[async_trait]
impl CheckoutChangeRequestLookup for StaticChangeRequestLookup {
    async fn inspect_checkout(
        &self,
        _cwd: &std::path::Path,
    ) -> Result<CheckoutSourceContext, ChangeRequestError> {
        Ok(self.source.clone())
    }

    async fn resolve_github_source(
        &self,
        _source: &CheckoutSourceContext,
    ) -> Result<Option<ChangeRequestSummary>, ChangeRequestError> {
        self.resolves.fetch_add(1, Ordering::AcqRel);
        Ok(Some(self.summary.clone()))
    }
}

fn init_git_repo(path: &std::path::Path) {
    std::fs::create_dir_all(path).expect("create git fixture");
    let status = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(path)
        .status()
        .expect("spawn git init");
    assert!(status.success(), "git init failed");
}

fn change_request_lookup(root: &std::path::Path) -> Arc<StaticChangeRequestLookup> {
    Arc::new(StaticChangeRequestLookup {
        source: CheckoutSourceContext {
            checkout_root: root.to_owned(),
            branch: BranchHeadContext::resolve(
                "feature/status",
                Some("origin/feature/status"),
                Some("origin"),
                Some("https://github.com/acme/luna.git"),
            ),
            default_branch: Some("main".into()),
        },
        summary: ChangeRequestSummary {
            provider: "github".into(),
            number: 90,
            title: "Stream checkout pull request".into(),
            url: "https://github.com/acme/luna/pull/90".into(),
            state: ChangeRequestState::Open,
            base_ref: "main".into(),
            head_ref: "feature/status".into(),
        },
        resolves: AtomicUsize::new(0),
    })
}

/// Exercise the device-room wire format used by the native iOS relay client,
/// without routing through another desktop engine first.
async fn simulated_ios_change_request(
    relay_url: &str,
    conn_id: &str,
    cwd: &std::path::Path,
    session: &AuthenticateRemote,
) -> serde_json::Value {
    let ws_url = relay_url.replacen("http://", "ws://", 1);
    let url = format!("{ws_url}/device/device-b/ws?role=client&connId={conn_id}&token=ios-token");
    let (mut socket, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("iOS relay client connects");
    let auth = serde_json::json!({
        "id": 6,
        "method": methods::AUTHENTICATE_REMOTE,
        "params": session,
    });
    let header = DeviceFrameHeader::new("rpc", "rpc");
    socket
        .send(WsMessage::Binary(
            encode_device_frame(&header, auth.to_string().as_bytes()).unwrap(),
        ))
        .await
        .expect("send iOS authentication");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("iOS auth before timeout")
            .expect("iOS relay remains connected")
            .expect("valid iOS auth frame");
        let WsMessage::Binary(bytes) = message else {
            continue;
        };
        let (header, payload) = decode_device_frame(&bytes).expect("decode iOS auth frame");
        if header.k != "rpc" {
            continue;
        }
        let response: serde_json::Value =
            serde_json::from_slice(&payload).expect("decode iOS auth response");
        if response["id"] != 6 {
            continue;
        }
        assert!(
            response.get("ok").is_some(),
            "iOS authentication failed: {response}"
        );
        break;
    }
    let request = serde_json::json!({
        "id": 7,
        "method": methods::WATCH_CHECKOUT_CHANGE_REQUEST,
        "params": { "cwd": cwd },
    });
    let frame = encode_device_frame(&header, request.to_string().as_bytes()).expect("encode RPC");
    socket
        .send(WsMessage::Binary(frame))
        .await
        .expect("send iOS subscription");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("iOS item before timeout")
            .expect("iOS relay remains connected")
            .expect("valid iOS relay frame");
        let WsMessage::Binary(bytes) = message else {
            continue;
        };
        let (header, payload) = decode_device_frame(&bytes).expect("decode iOS relay frame");
        assert_eq!(header.k, "rpc");
        let response: serde_json::Value =
            serde_json::from_slice(&payload).expect("decode RPC response");
        assert_eq!(response["id"], 7);
        if let Some(error) = response["err"].as_str() {
            panic!("iOS subscription failed: {error}");
        }
        if let Some(item) = response.get("item") {
            return item.clone();
        }
    }
}

fn assert_public_change_request_payload(item: &serde_json::Value) {
    let mut keys: Vec<_> = item
        .as_object()
        .expect("status object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "branch",
            "changeRequest",
            "checkoutId",
            "cwd",
            "deviceId",
            "updatedAt",
        ]
    );
    let mut summary_keys: Vec<_> = item["changeRequest"]
        .as_object()
        .expect("change request object")
        .keys()
        .map(String::as_str)
        .collect();
    summary_keys.sort_unstable();
    assert_eq!(
        summary_keys,
        [
            "baseRef", "headRef", "number", "provider", "state", "title", "url"
        ]
    );
    let payload = item.to_string();
    assert!(!payload.contains("ios-token"));
    assert!(!payload.to_ascii_lowercase().contains("stderr"));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_change_request_stream_matches_locally_and_through_device_routing() {
    let (relay_url, _relay) = fake_device_room().await;
    let dirs = tempfile::tempdir().expect("tempdir");
    let checkout = dirs.path().join("checkout");
    init_git_repo(&checkout);

    let mut core_b = assemble(&dirs.path().join("b"), "device-b");
    core_b
        .workspace
        .create_space(
            "space-pr",
            "device-b",
            &checkout.to_string_lossy(),
            None,
            true,
        )
        .expect("checkout space");
    let lookup = change_request_lookup(&checkout);
    core_b.change_requests =
        CheckoutChangeRequests::new(core_b.repos.clone(), "device-b", lookup.clone());
    let local_client = luna_rpc::memory_client(core_b.rpc_service());
    let rejected = match local_client
        .subscribe_checked(
            methods::WATCH_CHECKOUT_CHANGE_REQUEST,
            serde_json::json!({ "cwd": dirs.path().join("not-registered") }),
        )
        .await
    {
        Ok(_) => panic!("unregistered cwd must be rejected"),
        Err(error) => error,
    };
    assert!(rejected.to_string().contains("not a known checkout"));
    let mut local = local_client
        .subscribe_checked(
            methods::WATCH_CHECKOUT_CHANGE_REQUEST,
            serde_json::json!({ "cwd": checkout }),
        )
        .await
        .expect("local change request subscribe");
    let local_frame = local.recv().await.expect("local initial frame");

    let _host = core_b.start_host_relay(&relay_url);
    let remote_session = paired_remote_session(&core_b).await;
    let core_a = assemble(&dirs.path().join("a"), "device-a");
    let mut link_config =
        LinkCacheConfig::new(relay_url.clone(), Arc::new(StaticToken("test-user".into())));
    link_config.probe_timeout = Duration::from_secs(5);
    attach_remote_session(&mut link_config, remote_session.clone());
    core_a.set_links(LinkCache::new(link_config));
    let client = luna_rpc::memory_client(core_a.rpc_service());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut remote = loop {
        match client
            .subscribe_checked(
                methods::WATCH_CHECKOUT_CHANGE_REQUEST,
                serde_json::json!({
                    "cwd": checkout,
                    "targetDeviceId": "device-b",
                }),
            )
            .await
        {
            Ok(stream) => break stream,
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "relay never came up: {error}"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    let remote_frame = tokio::time::timeout(Duration::from_secs(5), remote.recv())
        .await
        .expect("remote frame before timeout")
        .expect("remote initial frame");
    assert_eq!(remote_frame, local_frame);

    // A second desktop subscription and native mobile clients all consume B's
    // host cache. Neither A nor iOS performs its own GitHub lookup.
    let mut second_desktop = client
        .subscribe_checked(
            methods::WATCH_CHECKOUT_CHANGE_REQUEST,
            serde_json::json!({
                "cwd": checkout,
                "targetDeviceId": "device-b",
            }),
        )
        .await
        .expect("second desktop subscription");
    let second_desktop_frame = second_desktop
        .recv()
        .await
        .expect("second desktop initial frame");
    assert_eq!(second_desktop_frame, local_frame);

    let ios_frame =
        simulated_ios_change_request(&relay_url, "ios-1", &checkout, &remote_session).await;
    assert_eq!(ios_frame, local_frame);
    assert_public_change_request_payload(&ios_frame);

    // Reconnecting creates a new relay peer and receives a fresh initial
    // snapshot from the still-warm host cache.
    let reconnected_ios_frame =
        simulated_ios_change_request(&relay_url, "ios-2", &checkout, &remote_session).await;
    assert_eq!(reconnected_ios_frame, local_frame);
    assert_eq!(
        lookup.resolves.load(Ordering::Acquire),
        1,
        "all device-boundary subscribers must share one host resolution"
    );

    core_a.disconnect_edge();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), remote.recv())
            .await
            .expect("remote stream closes after link")
            .is_none()
    );

    core_a.shutdown().await;
    core_b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_remote_change_request_watch_keeps_the_shared_device_link() {
    let (relay_url, _relay) = fake_device_room().await;
    let dirs = tempfile::tempdir().expect("tempdir");
    let legacy = Arc::new(LegacyChangeRequestService {
        list_harnesses_calls: AtomicUsize::new(0),
    });
    let legacy_auth = RemoteAuthStore::open(
        &dirs.path().join("legacy-auth"),
        RemoteEnvironment {
            device_id: "legacy-device".into(),
            workspace_scope: WorkspaceScope::Development,
            environment_id: "synthetic-legacy-environment".into(),
        },
    )
    .unwrap();
    let legacy_session = session_from_store(&legacy_auth);
    let _legacy_host = HostRelay::spawn(
        HostRelayConfig::new(
            &relay_url,
            "legacy-device",
            Arc::new(StaticToken("test-user".into())),
        ),
        legacy.clone(),
        legacy_auth,
    );

    let core = assemble(&dirs.path().join("new"), "new-device");
    let mut link_config =
        LinkCacheConfig::new(relay_url, Arc::new(StaticToken("test-user".into())));
    link_config.probe_timeout = Duration::from_secs(5);
    attach_remote_session(&mut link_config, legacy_session.clone());
    core.set_links(LinkCache::new(link_config));
    let client = luna_rpc::memory_client(core.rpc_service());

    // The host can take a moment to attach to the room. Once attached, an old
    // host rejects only the capability added by this version.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        match client
            .subscribe_checked(
                methods::WATCH_CHECKOUT_CHANGE_REQUEST,
                serde_json::json!({
                    "cwd": "/legacy-checkout",
                    "targetDeviceId": "legacy-device",
                }),
            )
            .await
        {
            Err(RpcError::UnknownMethod(method)) => {
                assert_eq!(method, methods::WATCH_CHECKOUT_CHANGE_REQUEST);
                break;
            }
            Ok(_) => panic!("legacy host must reject the change-request watch"),
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "legacy host never came up: {error}"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }

    let harnesses = client
        .call(
            methods::LIST_HARNESSES,
            serde_json::json!({ "targetDeviceId": "legacy-device" }),
        )
        .await
        .expect("a capability rejection must not sever the shared link");
    assert_eq!(harnesses, serde_json::json!([]));
    assert_eq!(
        legacy.list_harnesses_calls.load(Ordering::Acquire),
        2,
        "one readiness probe and one forwarded call prove the existing link was reused"
    );

    core.shutdown().await;
}

/// M5: terminals are device-addressable — OpenTerminal/WriteTerminal forward as
/// unary calls and SubscribeTerminal proxies its stream through the relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_stream_proxies_over_the_relay() {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;

    let (relay_url, _relay) = fake_device_room().await;
    let dirs = tempfile::tempdir().expect("tempdir");
    let cwd = dirs.path().join("work");
    std::fs::create_dir_all(&cwd).expect("cwd");

    // Engine B hosts its device room; its chat row (via its space) pins the
    // terminal cwd.
    let core_b = assemble(&dirs.path().join("b"), "device-b");
    core_b
        .workspace
        .create_space(
            "space-term",
            "device-b",
            &cwd.to_string_lossy(),
            None,
            false,
        )
        .expect("space row on B");
    core_b
        .workspace
        .create_chat("chat-term", Some("space-term"), None, None, None)
        .expect("chat row on B");
    let _host = core_b.start_host_relay(&relay_url);
    let remote_session = paired_remote_session(&core_b).await;

    let core_a = assemble(&dirs.path().join("a"), "device-a");
    let mut link_config =
        LinkCacheConfig::new(relay_url.clone(), Arc::new(StaticToken("test-user".into())));
    link_config.probe_timeout = Duration::from_secs(5);
    attach_remote_session(&mut link_config, remote_session.clone());
    core_a.set_links(LinkCache::new(link_config));
    let client = luna_rpc::memory_client(core_a.rpc_service());

    // OpenTerminal forwards to B once the relay session is up.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let session = loop {
        match client
            .call(
                methods::OPEN_TERMINAL,
                serde_json::json!({
                    "chatId": "chat-term",
                    "cols": 80,
                    "rows": 24,
                    "targetDeviceId": "device-b",
                }),
            )
            .await
        {
            Ok(session) => break session,
            Err(err) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "relay never came up: {err}"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    let terminal_id = session["id"].as_str().expect("terminal id").to_string();
    assert_eq!(
        session["cwd"].as_str(),
        Some(&*cwd.to_string_lossy()),
        "cwd from B's chat row"
    );

    // SubscribeTerminal: the stream is proxied item-by-item through the relay.
    let mut stream = client
        .subscribe(
            methods::SUBSCRIBE_TERMINAL,
            serde_json::json!({ "terminalId": terminal_id, "targetDeviceId": "device-b" }),
        )
        .await
        .expect("remote subscribe");
    client
        .call(
            methods::WRITE_TERMINAL,
            serde_json::json!({
                "terminalId": terminal_id,
                "data": BASE64.encode("echo r3lay-$((20+2))\n"),
                "targetDeviceId": "device-b",
            }),
        )
        .await
        .expect("remote write");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut transcript = Vec::new();
    loop {
        let item = tokio::time::timeout_at(deadline, stream.recv())
            .await
            .expect("proxied terminal output before timeout")
            .expect("stream alive");
        if item["type"] == "data" {
            let bytes = BASE64
                .decode(item["data"].as_str().expect("data"))
                .expect("valid base64");
            transcript.extend(bytes);
        }
        if String::from_utf8_lossy(&transcript).contains("r3lay-22") {
            break;
        }
    }

    client
        .call(
            methods::CLOSE_TERMINAL,
            serde_json::json!({ "terminalId": terminal_id, "targetDeviceId": "device-b" }),
        )
        .await
        .expect("remote close");

    core_a.shutdown().await;
    core_b.shutdown().await;
}

/// L29 exposes workspace file reads to a paired `files-read` session; remote
/// writes stay local-only (denial owned by `remote_auth.rs`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_file_surface_proxies_over_the_relay() {
    let (relay_url, _relay) = fake_device_room().await;
    let dirs = tempfile::tempdir().expect("tempdir");
    let repo_b = dirs.path().join("repo-b");
    init_workspace_repo(&repo_b).await;

    let core_b = assemble(&dirs.path().join("b-files"), "device-b");
    core_b
        .workspace
        .create_space(
            "space-files",
            "device-b",
            &repo_b.to_string_lossy(),
            None,
            true,
        )
        .expect("space on B");
    core_b
        .workspace
        .create_chat("chat-files", Some("space-files"), None, None, None)
        .expect("chat on B");
    let _host = core_b.start_host_relay(&relay_url);
    let remote_session = paired_remote_session(&core_b).await;

    let core_a = assemble(&dirs.path().join("a-files"), "device-a");
    let mut link_config =
        LinkCacheConfig::new(relay_url, Arc::new(StaticToken("test-user".into())));
    link_config.probe_timeout = Duration::from_secs(5);
    attach_remote_session(&mut link_config, remote_session.clone());
    core_a.set_links(LinkCache::new(link_config));
    let client = luna_rpc::memory_client(core_a.rpc_service());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let listing = loop {
        match client
            .call(
                methods::LIST_WORKSPACE_DIRECTORY,
                serde_json::json!({
                    "chatId": "chat-files",
                    "targetDeviceId": "device-b",
                }),
            )
            .await
        {
            Ok(value) => break value,
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "relay never came up: {error}"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    assert!(
        listing["entries"]
            .as_array()
            .is_some_and(|entries| { entries.iter().any(|entry| entry["path"] == "src") })
    );

    let matches = client
        .call(
            methods::SEARCH_WORKSPACE_FILES,
            serde_json::json!({
                "chatId": "chat-files",
                "query": "remote",
                "targetDeviceId": "device-b",
            }),
        )
        .await
        .expect("remote search");
    assert_eq!(matches[0]["path"], "src/remote.rs");

    let read = client
        .call(
            methods::READ_WORKSPACE_FILE,
            serde_json::json!({
                "chatId": "chat-files",
                "path": "src/remote.rs",
                "targetDeviceId": "device-b",
            }),
        )
        .await
        .expect("remote read");
    assert!(
        read["text"]
            .as_str()
            .is_some_and(|text| text.contains("REMOTE"))
    );
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><rect width="20" height="20"/></svg>"#;
    std::fs::write(repo_b.join("remote-image.svg"), svg).unwrap();
    let image = client
        .call(
            methods::READ_WORKSPACE_IMAGE,
            serde_json::json!({
                "chatId": "chat-files", "path": "remote-image.svg", "targetDeviceId": "device-b",
                "expectedCheckoutId": read["checkoutId"], "offset": 0,
            }),
        )
        .await
        .expect("remote workspace image");
    assert_eq!(image["mimeType"], "image/svg+xml");
    assert_eq!(image["size"], svg.len());
    assert_eq!(image["done"], true);
    assert!(client.call(methods::READ_WORKSPACE_IMAGE, serde_json::json!({
        "chatId": "chat-files", "path": "remote-image.svg", "targetDeviceId": "device-b",
        "expectedCheckoutId": "wrong-checkout", "offset": 0,
    })).await.is_err());

    // The caller has no workspace for this chat: only the owner can read it.
    assert!(
        client
            .call(
                methods::READ_WORKSPACE_IMAGE,
                serde_json::json!({
                    "chatId": "chat-files", "path": "remote-image.svg",
                    "expectedCheckoutId": read["checkoutId"], "offset": 0,
                })
            )
            .await
            .is_err()
    );

    let large_svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><!--{}--><rect width="20" height="20" fill="red"/></svg>"#,
        " ".repeat(luna_proto::WORKSPACE_IMAGE_CHUNK_BYTES)
    );
    std::fs::write(repo_b.join("remote-image.svg"), &large_svg).unwrap();
    let image_request = serde_json::json!({
        "chatId": "chat-files", "path": "remote-image.svg", "targetDeviceId": "device-b",
        "expectedCheckoutId": read["checkoutId"], "offset": 0,
    });
    let first = client
        .call(methods::READ_WORKSPACE_IMAGE, image_request.clone())
        .await
        .unwrap();
    assert_eq!(first["checkoutId"], read["checkoutId"]);
    assert_eq!(first["done"], false);
    let mut continuation = image_request;
    continuation["offset"] = first["nextOffset"].clone();
    continuation["expectedContentHash"] = first["contentHash"].clone();
    let last = client
        .call(methods::READ_WORKSPACE_IMAGE, continuation.clone())
        .await
        .unwrap();
    assert_eq!(last["contentHash"], first["contentHash"]);
    assert_eq!(last["done"], true);
    {
        use base64::Engine as _;
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(first["data"].as_str().unwrap())
            .unwrap();
        bytes.extend(
            base64::engine::general_purpose::STANDARD
                .decode(last["data"].as_str().unwrap())
                .unwrap(),
        );
        assert_eq!(bytes, large_svg.as_bytes());
    }
    // Same length, different content: a continuation from the old generation
    // must fail on the owning device, even though the routing is still valid.
    std::fs::write(
        repo_b.join("remote-image.svg"),
        large_svg.replace("red", "tan"),
    )
    .unwrap();
    assert!(
        client
            .call(methods::READ_WORKSPACE_IMAGE, continuation.clone())
            .await
            .is_err()
    );
    continuation["expectedCheckoutId"] = "obsolete-checkout".into();
    assert!(
        client
            .call(methods::READ_WORKSPACE_IMAGE, continuation)
            .await
            .is_err()
    );

    let mut stream = client
        .subscribe(
            methods::WATCH_WORKSPACE_FILES,
            serde_json::json!({
                "chatId": "chat-files",
                "targetDeviceId": "device-b",
            }),
        )
        .await
        .expect("remote watch");
    let baseline = tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .expect("baseline timeout")
        .expect("watch alive");
    assert_eq!(baseline["resyncRequired"], true);
    std::fs::write(repo_b.join("remote-created.txt"), "created\n").expect("external write on B");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let item = tokio::time::timeout_at(deadline, stream.recv())
            .await
            .expect("remote watch timeout")
            .expect("remote watch alive");
        if item["changes"].as_array().is_some_and(|changes| {
            changes
                .iter()
                .any(|change| change["path"] == "remote-created.txt")
        }) {
            break;
        }
    }
    drop(stream);

    core_a.shutdown().await;
    core_b.shutdown().await;
}

#[tokio::test]
async fn remote_target_without_links_fails_clearly() {
    let dirs = tempfile::tempdir().expect("tempdir");
    let core = assemble(&dirs.path().join("solo"), "device-solo");
    let client = luna_rpc::memory_client(core.rpc_service());
    let err = client
        .call(
            methods::LIST_HARNESSES,
            serde_json::json!({ "targetDeviceId": "device-elsewhere" }),
        )
        .await
        .expect_err("offline forward must fail");
    assert!(
        err.to_string().contains("remote routing unavailable"),
        "got: {err}"
    );
    let err = client
        .call(
            methods::LIST_WORKSPACE_DIRECTORY,
            serde_json::json!({
                "chatId": "missing-local-chat",
                "targetDeviceId": "device-elsewhere",
            }),
        )
        .await
        .expect_err("workspace call must not fall back locally");
    assert!(
        err.to_string().contains("remote routing unavailable"),
        "got: {err}"
    );
    core.shutdown().await;
}
