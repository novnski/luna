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

/// The direct gateway at its real boundary: a client that pins the host's
/// SPKI, as the phone does, over HTTPS and WebSocket.
#[cfg(unix)]
mod gateway {
    use std::net::{IpAddr, SocketAddr};
    use std::sync::Arc;
    use std::time::Duration;

    use futures::{SinkExt, StreamExt};
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    use super::*;

    type Socket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// The gateway refuses loopback, so tests bind this machine's route
    /// address. The UDP connect sends nothing.
    fn lan_ip() -> IpAddr {
        // A VPN may own the default route without accepting inbound LAN
        // traffic. A concrete fixture interface still uses production bind
        // rules and the real gateway; it never enables loopback.
        if let Ok(address) = std::env::var("LUNA_REMOTE_TEST_IP") {
            return address
                .parse()
                .expect("LUNA_REMOTE_TEST_IP is an IP address");
        }
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        socket
            .connect("192.0.2.1:9")
            .expect("a non-loopback route; the gateway refuses loopback");
        socket.local_addr().unwrap().ip()
    }

    /// A port free on the route address and on loopback (the gateway's rule).
    fn free_bind() -> String {
        let ip = lan_ip();
        loop {
            let port = std::net::TcpListener::bind((ip, 0))
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                return SocketAddr::new(ip, port).to_string();
            }
        }
    }

    #[derive(Debug)]
    struct Pin(String, Arc<rustls::crypto::CryptoProvider>);

    impl ServerCertVerifier for Pin {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            let (_, certificate) = x509_parser::parse_x509_certificate(end_entity.as_ref())
                .map_err(|_| rustls::Error::General("unparsable certificate".into()))?;
            let spki = hex(&Sha256::digest(certificate.public_key().raw));
            if spki == self.0 {
                Ok(ServerCertVerified::assertion())
            } else {
                Err(rustls::Error::General("pin mismatch".into()))
            }
        }
        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.1.signature_verification_algorithms,
            )
        }
        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.1.signature_verification_algorithms,
            )
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.1.signature_verification_algorithms.supported_schemes()
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn tls(pin: &str) -> Arc<rustls::ClientConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pin(pin.into(), provider)))
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(config)
    }

    fn http(pin: &str) -> reqwest::Client {
        reqwest::Client::builder()
            .use_preconfigured_tls((*tls(pin)).clone())
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap()
    }

    async fn post(
        http: &reqwest::Client,
        bind: &str,
        path: &str,
        bearer: Option<&str>,
        body: &Value,
    ) -> (u16, Value) {
        let mut request = http.post(format!("https://{bind}{path}")).json(body);
        if let Some(token) = bearer {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.expect("the gateway answers");
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn socket(
        bind: &str,
        pin: &str,
        ticket: &str,
        origin: Option<&str>,
    ) -> Result<Socket, tokio_tungstenite::tungstenite::Error> {
        let mut request = format!("wss://{bind}/remote/v1/ws")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Ticket {ticket}").parse().unwrap());
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
        }
        tokio_tungstenite::connect_async_tls_with_config(
            request,
            None,
            false,
            Some(tokio_tungstenite::Connector::Rustls(tls(pin))),
        )
        .await
        .map(|(socket, _)| socket)
    }

    /// One unary call over the socket: `Ok(ok)` or `Err(err)`.
    async fn call(
        socket: &mut Socket,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        socket
            .send(Message::Text(
                json!({"id": id, "method": method, "params": params}).to_string(),
            ))
            .await
            .map_err(|error| format!("closed: {error}"))?;
        loop {
            let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
                .await
                .map_err(|_| "no reply".to_string())?;
            match message {
                Some(Ok(Message::Text(text))) => {
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    if frame["id"] != id {
                        continue;
                    }
                    if let Some(error) = frame.get("err") {
                        return Err(error.as_str().unwrap_or_default().to_owned());
                    }
                    return Ok(frame["ok"].clone());
                }
                Some(Ok(_)) => continue,
                Some(Err(error)) => return Err(format!("closed: {error}")),
                None => return Err("closed".into()),
            }
        }
    }

    /// Waits for the server to close the socket (a positive event).
    async fn closed(socket: &mut Socket) -> bool {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match socket.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => {}
                }
            }
        })
        .await
        .is_ok()
    }

    async fn pair(
        local: &luna_rpc::RpcClient,
        http: &reqwest::Client,
        bind: &str,
        label: &str,
        session_ttl_secs: u64,
    ) -> IssuedSession {
        let issued: IssuedPairing = local
            .call_as(
                methods::CREATE_REMOTE_PAIRING,
                json!({"label": label, "scopes": ["environment-read", "tasks-read"],
                    "pairingTtlSecs": 300, "sessionTtlSecs": session_ttl_secs}),
            )
            .await
            .unwrap();
        let (status, session) = post(
            http,
            bind,
            "/remote/v1/pair",
            None,
            &json!({"target": issued.target, "credential": issued.credential, "clientLabel": label}),
        )
        .await;
        assert_eq!(status, 200, "pairing over the gateway: {session}");
        serde_json::from_value(session).unwrap()
    }

    async fn ticket(http: &reqwest::Client, bind: &str, session: &IssuedSession) -> String {
        let (status, ticket) = post(
            http,
            bind,
            "/remote/v1/socket-ticket",
            Some(&session.session_token),
            &json!({"target": session.target}),
        )
        .await;
        assert_eq!(status, 200, "{ticket}");
        ticket["ticket"].as_str().unwrap().to_owned()
    }

    /// A Mac's default VPN route or firewall may drop this probe. An
    /// explicitly selected LAN interface and Linux CI must execute the
    /// campaign; neither can report an unreachable gateway as a pass.
    async fn firewalled(bind: &str) -> bool {
        match tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(bind))
            .await
        {
            Ok(Ok(_)) => false,
            Ok(Err(error)) => panic!("the gateway is not listening on {bind}: {error}"),
            Err(_)
                if cfg!(target_os = "macos")
                    && std::env::var_os("LUNA_REMOTE_TEST_IP").is_none() =>
            {
                eprintln!(
                    "SKIPPED: default-route {bind} is unreachable on this Mac \
                     (VPN routing or application firewall); select a real LAN \
                     interface with LUNA_REMOTE_TEST_IP or use Linux CI"
                );
                true
            }
            Err(_) => panic!("the required LAN gateway probe timed out on {bind}"),
        }
    }

    async fn enable(local: &luna_rpc::RpcClient, bind: &str) -> Value {
        local
            .call("SetRemoteAccess", json!({"enabled": true, "bind": bind}))
            .await
            .expect("remote access enables")
    }

    /// B1–B3, B7, RP-02, RP-03: off until enabled; then the pinned host pairs
    /// once, admits a ticketed socket for that session's scopes only, and
    /// refuses replays, browser origins, oversized bodies and local-only methods.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_access_is_off_until_enabled_then_admits_only_its_paired_session() {
        let root = tempfile::tempdir().unwrap();
        let core = assemble(root.path());
        let local = luna_rpc::memory_client(core.rpc_service());
        let status = local.call("GetRemoteAccess", json!({})).await.unwrap();
        assert_eq!(
            (status["enabled"].clone(), status["listening"].clone()),
            (json!(false), json!(false))
        );

        let bind = free_bind();
        let port = bind.rsplit(':').next().unwrap();
        let loopback = local
            .call(
                "SetRemoteAccess",
                json!({"enabled": true, "bind": format!("127.0.0.1:{port}")}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(loopback.contains("loopback"), "{loopback}");

        let status = enable(&local, &bind).await;
        assert_eq!(status["listening"], true, "{status}");
        if firewalled(&bind).await {
            core.shutdown().await;
            return;
        }
        let pin = status["spkiSha256"].as_str().unwrap().to_owned();
        let http = http(&pin);
        let descriptor: Value = http
            .get(format!("https://{bind}/remote/v1/descriptor"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(descriptor["spkiSha256"], pin);
        assert_eq!(descriptor["target"], status["target"]);
        assert!(
            crate::gateway::http(&"0".repeat(64))
                .get(format!("https://{bind}/remote/v1/descriptor"))
                .send()
                .await
                .is_err(),
            "a wrong pin fails at TLS"
        );

        let issued: IssuedPairing = local
            .call_as(
                methods::CREATE_REMOTE_PAIRING,
                json!({"label": "phone", "scopes": ["environment-read"],
                    "pairingTtlSecs": 300, "sessionTtlSecs": 3600}),
            )
            .await
            .unwrap();
        let redeem = json!({"target": issued.target, "credential": issued.credential, "clientLabel": "phone"});
        for (field, value, reason) in [
            (
                "deviceId",
                json!("another-host"),
                "target environment mismatch",
            ),
            (
                "environmentId",
                json!("another-profile"),
                "target environment mismatch",
            ),
            (
                "workspaceScope",
                json!("local"),
                "target environment mismatch",
            ),
            (
                "protocolVersion",
                json!(999),
                "remote protocol or capability mismatch",
            ),
        ] {
            let mut wrong = redeem.clone();
            wrong["target"][field] = value;
            let (code, body) = post(&http, &bind, "/remote/v1/pair", None, &wrong).await;
            assert_eq!(code, 403, "wrong {field}: {body}");
            assert!(body["error"].as_str().unwrap().contains(reason), "{body}");
        }
        let (malformed, body) = post(
            &http,
            &bind,
            "/remote/v1/pair",
            None,
            &json!({"target": issued.target, "credential": 42, "clientLabel": "phone"}),
        )
        .await;
        assert_eq!(malformed, 400);
        assert_eq!(body["error"], "invalid pairing request");
        let (status, session) = post(&http, &bind, "/remote/v1/pair", None, &redeem).await;
        assert_eq!(
            status, 200,
            "failed target attempts did not consume the pairing"
        );
        let session: IssuedSession = serde_json::from_value(session).unwrap();
        let (replayed, _) = post(&http, &bind, "/remote/v1/pair", None, &redeem).await;
        assert_eq!(replayed, 403, "a used credential is refused");
        let oversized = json!({"padding": "x".repeat(20_000)});
        let (too_large, _) = post(&http, &bind, "/remote/v1/pair", None, &oversized).await;
        assert_eq!(too_large, 413);

        for cancelled in [true, false] {
            let short: IssuedPairing = local
                .call_as(
                    methods::CREATE_REMOTE_PAIRING,
                    json!({"label": "short", "scopes": ["environment-read"],
                    "pairingTtlSecs": 1, "sessionTtlSecs": 3600}),
                )
                .await
                .unwrap();
            if cancelled {
                let revoked = local
                    .call(
                        methods::REVOKE_REMOTE_PAIRING,
                        json!({"id": short.pairing.id}),
                    )
                    .await
                    .unwrap();
                assert_eq!(revoked["revoked"], true);
            } else {
                // The actual public expiry deadline, without a clock seam.
                let remaining = short
                    .pairing
                    .expires_at_ms
                    .saturating_sub(chrono::Utc::now().timestamp_millis() as u64);
                tokio::time::sleep(Duration::from_millis(remaining + 1)).await;
            }
            let (code, body) = post(
                &http,
                &bind,
                "/remote/v1/pair",
                None,
                &json!({"target": short.target, "credential": short.credential,
                    "clientLabel": "short"}),
            )
            .await;
            assert_eq!(code, 403, "cancelled={cancelled}: {body}");
            assert!(
                body["error"]
                    .as_str()
                    .unwrap()
                    .contains("pairing unavailable, expired, or already used"),
                "{body}"
            );
        }
        let recovered = pair(&local, &http, &bind, "fresh after refusals", 3600).await;
        assert_ne!(recovered.session.id, session.session.id);

        let (checked, view) = post(
            &http,
            &bind,
            "/remote/v1/session/check",
            Some(&session.session_token),
            &json!({"target": session.target}),
        )
        .await;
        assert_eq!(checked, 200, "{view}");
        assert!(
            view["lastUsedAtMs"].is_u64(),
            "the check records a use: {view}"
        );

        let first = ticket(&http, &bind, &session).await;
        let mut tampered = first.clone().into_bytes();
        let last = tampered.last_mut().unwrap();
        *last = if *last == b'0' { b'1' } else { b'0' };
        let tampered = String::from_utf8(tampered).unwrap();
        match socket(&bind, &pin, &tampered, None).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(response.status().as_u16(), 401);
                let body: Value =
                    serde_json::from_slice(response.body().as_deref().unwrap()).unwrap();
                assert!(
                    body["error"]
                        .as_str()
                        .unwrap()
                        .contains("invalid socket ticket signature"),
                    "{body}"
                );
            }
            _ => panic!("a tampered signed capability must fail at the HTTP upgrade"),
        }
        assert!(
            socket(&bind, &pin, &first, Some("https://evil.example"))
                .await
                .is_err(),
            "a browser origin is refused"
        );
        let mut ws = socket(&bind, &pin, &first, None)
            .await
            .expect("a ticketed socket opens");
        let info = call(&mut ws, 1, methods::ENGINE_INFO, json!({}))
            .await
            .unwrap();
        assert_eq!(info["deviceId"], core.device_id);
        let task = "remote-denial-task";
        core.workspace
            .create_chat(task, None, Some(&core.device_id), None, None)
            .unwrap();
        let snapshot: luna_proto::TaskCommandRead = local
            .call_as(methods::GET_TASK_COMMAND_SNAPSHOT, json!({"taskId": task}))
            .await
            .unwrap();
        let command_id = "remote-denied-rename";
        let command = json!({"commandId": command_id, "taskId": task,
            "expectedTaskRevision": snapshot.snapshot.task_revision,
            "actorId": "forged-actor", "clientId": "forged-client",
            "clientSessionId": "forged-session",
            "intent": {"kind": "rename", "title": "local command still works"}});
        for (id, method, params) in [
            (2, methods::LIST_REMOTE_SESSIONS, json!({})),
            (3, methods::EXECUTE_TASK_COMMAND, command.clone()),
            (
                5,
                methods::MUTATE,
                json!({"op": "renameChat", "chatId": task,
                "title": "remote must not change this"}),
            ),
        ] {
            let denied = call(&mut ws, id, method, params).await.unwrap_err();
            assert!(denied.contains("local only"), "{method}: {denied}");
        }
        let receipt = local
            .call(
                methods::GET_TASK_COMMAND_RECEIPT,
                json!({"commandId": command_id}),
            )
            .await
            .unwrap();
        assert_eq!(
            receipt,
            Value::Null,
            "a denied remote command creates no receipt"
        );
        assert!(
            core.store
                .local_executable_hash(task, luna_sync::LocalExecutableKind::Command, command_id)
                .unwrap()
                .is_none(),
            "a denied remote command creates no executable admission"
        );
        assert_eq!(
            core.workspace.chat(task).unwrap().unwrap().title,
            None,
            "neither denied mutation changed the task"
        );
        let applied = local
            .call(methods::EXECUTE_TASK_COMMAND, command)
            .await
            .unwrap();
        assert_eq!(
            applied["status"], "applied",
            "the refused command was valid locally: {applied}"
        );
        assert_eq!(
            core.workspace.chat(task).unwrap().unwrap().title.as_deref(),
            Some("local command still works")
        );
        let denied = call(
            &mut ws,
            4,
            methods::GET_TASK_COMMAND_RECEIPT,
            json!({"commandId": "x"}),
        )
        .await
        .unwrap_err();
        assert!(denied.contains("lacks required scope"), "{denied}");
        assert!(
            socket(&bind, &pin, &first, None).await.is_err(),
            "a ticket opens one socket only"
        );
        match socket(&bind, &pin, "malformed-ticket", None).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(response.status().as_u16(), 401)
            }
            _ => panic!("a malformed ticket must be refused at the HTTP upgrade"),
        }

        core.shutdown().await;
        assert!(closed(&mut ws).await, "shutdown closes remote sockets");
    }

    /// B5, B6, RP-05: revoking one session closes only its socket; disabling
    /// closes the rest and stops the listener; re-enabling keeps the pin.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn revoke_closes_only_its_session_and_disable_closes_everything() {
        let root = tempfile::tempdir().unwrap();
        let core = assemble(root.path());
        let local = luna_rpc::memory_client(core.rpc_service());
        let bind = free_bind();
        let pin = enable(&local, &bind).await["spkiSha256"]
            .as_str()
            .unwrap()
            .to_owned();
        if firewalled(&bind).await {
            core.shutdown().await;
            return;
        }
        let http = http(&pin);
        let a = pair(&local, &http, &bind, "phone a", 3600).await;
        let b = pair(&local, &http, &bind, "phone b", 3600).await;
        let mut wa = socket(&bind, &pin, &ticket(&http, &bind, &a).await, None)
            .await
            .unwrap();
        let mut wb = socket(&bind, &pin, &ticket(&http, &bind, &b).await, None)
            .await
            .unwrap();
        call(&mut wa, 1, methods::ENGINE_INFO, json!({}))
            .await
            .unwrap();
        call(&mut wb, 1, methods::ENGINE_INFO, json!({}))
            .await
            .unwrap();
        let outstanding = ticket(&http, &bind, &a).await;

        local
            .call(methods::REVOKE_REMOTE_SESSION, json!({"id": a.session.id}))
            .await
            .unwrap();
        assert!(closed(&mut wa).await, "the revoked session's socket closes");
        call(&mut wb, 2, methods::ENGINE_INFO, json!({}))
            .await
            .expect("the other session keeps working");
        let (refused, _) = post(
            &http,
            &bind,
            "/remote/v1/session/check",
            Some(&a.session_token),
            &json!({"target": a.target}),
        )
        .await;
        assert_eq!(refused, 403);

        // RP-06: issued capabilities retain their short lifetime, but do not
        // bypass live-session admission after revocation.
        let mut revoked = socket(&bind, &pin, &outstanding, None)
            .await
            .expect("a pre-revoke ticket still upgrades before its expiry");
        let denied = call(&mut revoked, 1, methods::ENGINE_INFO, json!({}))
            .await
            .expect_err("a revoked session cannot call through an outstanding ticket");
        assert!(
            denied.contains("closed") || denied.contains("revoked"),
            "{denied}"
        );
        assert!(closed(&mut revoked).await, "the revoked connection closes");
        assert!(
            socket(&bind, &pin, &outstanding, None).await.is_err(),
            "the pre-revoke ticket was consumed exactly once"
        );
        local
            .call(methods::ENGINE_INFO, json!({}))
            .await
            .expect("revoke leaves local work available");

        // RM-02 D2: disabling invalidates unspent bootstraps, while an
        // already paired, unrevoked session remains eligible after re-enable.
        let pending: IssuedPairing = local
            .call_as(
                methods::CREATE_REMOTE_PAIRING,
                json!({"label":"pending", "scopes":["environment-read"],
                "pairingTtlSecs":300,"sessionTtlSecs":3600}),
            )
            .await
            .unwrap();

        let status = local
            .call("SetRemoteAccess", json!({"enabled": false}))
            .await
            .unwrap();
        assert_eq!(
            (status["enabled"].clone(), status["listening"].clone()),
            (json!(false), json!(false))
        );
        assert!(closed(&mut wb).await, "disable closes every socket");
        let released = std::net::TcpListener::bind(&bind)
            .expect("disable releases the kernel listener, even behind a stealth firewall");
        drop(released);
        assert!(
            !matches!(
                tokio::time::timeout(
                    Duration::from_secs(2),
                    tokio::net::TcpStream::connect(&bind)
                )
                .await,
                Ok(Ok(_))
            ),
            "the listener is gone"
        );

        let again = enable(&local, &bind).await;
        let (refused, _) = post(&http, &bind, "/remote/v1/pair", None,
            &json!({"target":pending.target,"credential":pending.credential,"clientLabel":"pending"})).await;
        assert_eq!(refused, 403, "disable invalidates pending pairing codes");
        assert_eq!(
            again["spkiSha256"], pin,
            "the identity, and so the pin, is stable"
        );
        let (checked, _) = post(
            &http,
            &bind,
            "/remote/v1/session/check",
            Some(&b.session_token),
            &json!({"target": b.target}),
        )
        .await;
        assert_eq!(checked, 200, "an unrevoked session survives disable");
        core.shutdown().await;
    }

    /// RP-19: enabled remote access resumes after a restart with the same pin;
    /// a live session still checks in and revoked/expired ones stay refused.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_access_resumes_after_restart_with_the_same_pin() {
        let root = tempfile::tempdir().unwrap();
        let core = assemble(root.path());
        let local = luna_rpc::memory_client(core.rpc_service());
        let bind = free_bind();
        let pin = enable(&local, &bind).await["spkiSha256"]
            .as_str()
            .unwrap()
            .to_owned();
        if firewalled(&bind).await {
            core.shutdown().await;
            return;
        }
        let http = http(&pin);
        let live = pair(&local, &http, &bind, "kept", 3600).await;
        let gone = pair(&local, &http, &bind, "revoked", 3600).await;
        let expired = pair(&local, &http, &bind, "expired", 1).await;
        local
            .call(
                methods::REVOKE_REMOTE_SESSION,
                json!({"id": gone.session.id}),
            )
            .await
            .unwrap();
        let remaining = expired
            .session
            .expires_at_ms
            .saturating_sub(chrono::Utc::now().timestamp_millis() as u64);
        tokio::time::sleep(Duration::from_millis(remaining + 1)).await;
        shutdown(core).await;

        let reopened = assemble(root.path());
        let local = luna_rpc::memory_client(reopened.rpc_service());
        let status = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let status = local.call("GetRemoteAccess", json!({})).await.unwrap();
                if status["listening"] == true {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("enabled remote access resumes on start");
        assert_eq!(status["spkiSha256"], pin);
        assert_eq!(
            status["target"],
            serde_json::to_value(&live.target).unwrap()
        );
        for (session, expected) in [(&live, 200), (&gone, 403), (&expired, 403)] {
            let (code, _) = post(
                &http,
                &bind,
                "/remote/v1/session/check",
                Some(&session.session_token),
                &json!({"target": session.target}),
            )
            .await;
            assert_eq!(code, expected, "{}", session.session.client_label);
        }
        reopened.shutdown().await;
    }

    /// RM-02 P1–P4/P7: the portable enrollment owner drives the real pinned
    /// gateway, waits for platform persistence, then authenticates. This is
    /// transport/core proof; physical Keychain and UI have their own campaign.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enrollment_preflights_identity_before_spending_a_code_and_commits_only_after_storage()
    {
        use luna_mobile::pairing::{Enrollment, EnrollmentAction};
        use luna_proto::remote_control::{HostTicket, PairingTicket};

        let root = tempfile::tempdir().unwrap();
        let core = assemble(root.path());
        let local = luna_rpc::memory_client(core.rpc_service());
        let bind = free_bind();
        let status = enable(&local, &bind).await;
        if firewalled(&bind).await {
            shutdown(core).await;
            return;
        }
        let pin = status["spkiSha256"].as_str().unwrap();
        let http = http(pin);
        let issued: IssuedPairing = local
            .call_as(
                methods::CREATE_REMOTE_PAIRING,
                json!({"label": "enrollment", "scopes": ["environment-read", "tasks-read"],
                "pairingTtlSecs": 300, "sessionTtlSecs": 3600}),
            )
            .await
            .unwrap();
        let ticket = PairingTicket {
            version: 1,
            host_name: Some("Verification Mac".into()),
            host: HostTicket {
                version: 1,
                target: issued.target.clone(),
                endpoints: vec![format!("https://{bind}")],
                spki_sha256: pin.into(),
            },
            pairing_id: issued.pairing.id.clone(),
            credential: issued.credential.clone(),
            expires_at_ms: issued.pairing.expires_at_ms,
            scopes: vec!["environment-read".into(), "tasks-read".into()],
        };
        let route = luna_mobile::probe::RouteObservation {
            remote_address: Some(bind.clone()),
            local_address: None,
            interface: "other".into(),
            proxied: false,
            tls_version: Some("TLSv1.3".into()),
        };
        let enrollment = Enrollment::new(ticket.encode().unwrap()).unwrap();
        let EnrollmentAction::Request { request: first } = enrollment.start(0, "phone".into())
        else {
            panic!("a confirmation starts descriptor preflight")
        };
        assert_eq!(first.method, "GET");
        assert!(
            first.body.is_empty() && first.authorization.is_none(),
            "the descriptor carries no credential"
        );
        let descriptor = http
            .get(&first.url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let mut wrong: Value = serde_json::from_slice(&descriptor).unwrap();
        wrong["target"]["environmentId"] = json!("foreign-profile");
        assert!(matches!(
            enrollment.complete(
                first.request_id,
                200,
                serde_json::to_vec(&wrong).unwrap(),
                route.clone()
            ),
            EnrollmentAction::Rejected { .. }
        ));
        let pending: Vec<luna_rpc::remote_auth::PairingView> = local
            .call_as(methods::LIST_REMOTE_PAIRINGS, json!({}))
            .await
            .unwrap();
        assert!(
            pending.iter().any(|row| row.id == issued.pairing.id),
            "wrong-target preflight never spends the code"
        );

        let EnrollmentAction::Request { request: first } = enrollment.start(0, "phone".into())
        else {
            panic!("a new confirmed preflight may recover before redemption")
        };
        let EnrollmentAction::Request { request: redeem } =
            enrollment.complete(first.request_id, 200, descriptor.to_vec(), route.clone())
        else {
            panic!("exact pinned descriptor permits redemption")
        };
        assert_eq!(redeem.method, "POST");
        assert!(
            !redeem.url.contains(&issued.credential),
            "secrets are never in HTTP URLs"
        );
        let response = http
            .post(&redeem.url)
            .body(redeem.body.clone())
            .header("content-type", "application/json")
            .send()
            .await
            .unwrap();
        let EnrollmentAction::Save { request_id, mac } = enrollment.complete(
            redeem.request_id,
            response.status().as_u16(),
            response.bytes().await.unwrap().to_vec(),
            route.clone(),
        ) else {
            panic!("the issued session must be saved before authentication")
        };
        assert!(
            matches!(
                enrollment.persisted(request_id + 1, true),
                EnrollmentAction::Ignore
            ),
            "a stale persistence callback cannot commit this session"
        );
        let restored = Enrollment::restore(mac.clone()).unwrap();
        let EnrollmentAction::Request { request: check } = enrollment.persisted(request_id, true)
        else {
            panic!("only successful platform storage permits authenticated readback")
        };
        let response = http
            .post(&check.url)
            .body(check.body)
            .header("content-type", "application/json")
            .header("authorization", check.authorization.unwrap())
            .send()
            .await
            .unwrap();
        assert!(matches!(
            enrollment.complete(
                check.request_id,
                response.status().as_u16(),
                response.bytes().await.unwrap().to_vec(),
                route.clone()
            ),
            EnrollmentAction::Connected { .. }
        ));
        assert!(
            matches!(
                enrollment.complete(redeem.request_id, 200, vec![], route.clone()),
                EnrollmentAction::Ignore
            ),
            "a late redemption cannot change connected state"
        );
        local
            .call(
                methods::REVOKE_REMOTE_SESSION,
                json!({"id": mac.session_id}),
            )
            .await
            .unwrap();
        let EnrollmentAction::Request { request: preflight } = restored.start(0, "phone".into())
        else {
            panic!("restart validates the same pinned host before reconnecting")
        };
        let EnrollmentAction::Request { request: check } = restored.complete(
            preflight.request_id,
            200,
            descriptor.to_vec(),
            route.clone(),
        ) else {
            panic!("restart authenticates the saved session")
        };
        let response = http
            .post(&check.url)
            .body(check.body)
            .header("content-type", "application/json")
            .header("authorization", check.authorization.unwrap())
            .send()
            .await
            .unwrap();
        assert!(matches!(
            restored.complete(
                check.request_id,
                response.status().as_u16(),
                response.bytes().await.unwrap().to_vec(),
                route.clone()
            ),
            EnrollmentAction::RePair { .. }
        ));
        // P4: each fault follows a real successful, one-use host redemption.
        // These are controlled platform callbacks, not physical OS refusal.
        for fault in [
            "lost-reply",
            "oversized-reply",
            "storage-refusal",
            "cancel-save",
        ] {
            let issued: IssuedPairing = local
                .call_as(
                    methods::CREATE_REMOTE_PAIRING,
                    json!({"label":fault,"scopes":["environment-read","tasks-read"],
                    "pairingTtlSecs":300,"sessionTtlSecs":3600}),
                )
                .await
                .unwrap();
            let ticket = PairingTicket {
                pairing_id: issued.pairing.id,
                credential: issued.credential,
                expires_at_ms: issued.pairing.expires_at_ms,
                ..ticket.clone()
            };
            let owner = Enrollment::new(ticket.encode().unwrap()).unwrap();
            let EnrollmentAction::Request { request: preflight } = owner.start(0, fault.into())
            else {
                panic!("confirmed preflight")
            };
            let body = http
                .get(preflight.url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            let EnrollmentAction::Request { request: redeem } =
                owner.complete(preflight.request_id, 200, body.to_vec(), route.clone())
            else {
                panic!("verified redemption")
            };
            let response = http
                .post(redeem.url)
                .header("content-type", "application/json")
                .body(redeem.body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            let body = response.bytes().await.unwrap().to_vec();
            let session: Value = serde_json::from_slice(&body).unwrap();
            match fault {
                "lost-reply" => assert!(matches!(
                    owner.fail(
                        redeem.request_id,
                        luna_mobile::probe::TransportFailure::Network {
                            detail: "controlled lost response".into()
                        }
                    ),
                    EnrollmentAction::Uncertain { .. }
                )),
                "oversized-reply" => assert!(matches!(
                    owner.complete(
                        redeem.request_id,
                        200,
                        vec![b'x'; 16 * 1024 + 1],
                        route.clone()
                    ),
                    EnrollmentAction::Uncertain { .. }
                )),
                _ => {
                    let EnrollmentAction::Save { request_id, .. } =
                        owner.complete(redeem.request_id, 200, body, route.clone())
                    else {
                        panic!("storage is required")
                    };
                    if fault == "cancel-save" {
                        owner.cancel();
                        assert!(matches!(
                            owner.persisted(request_id, true),
                            EnrollmentAction::Ignore
                        ));
                    } else {
                        assert!(matches!(
                            owner.persisted(request_id, false),
                            EnrollmentAction::RePair { .. }
                        ));
                    }
                }
            }
            assert!(
                !owner.retry_available(),
                "a spent code cannot silently retry: {fault}"
            );
            let sessions: Vec<SessionView> = local
                .call_as(methods::LIST_REMOTE_SESSIONS, json!({}))
                .await
                .unwrap();
            let row = sessions
                .iter()
                .find(|row| row.id == session["id"].as_str().unwrap())
                .unwrap();
            assert!(
                row.last_used_at_ms.is_none(),
                "failed enrollment never authenticates: {fault}"
            );
            local
                .call(methods::REVOKE_REMOTE_SESSION, json!({"id":row.id}))
                .await
                .unwrap();
        }
        // Optional native extension of this same real gateway owner. A
        // prebuilt XCTest runner consumes private fixtures and writes public
        // observations. No app launch argument or production QA seam is used.
        if let Some(path) = std::env::var_os("LUNA_NATIVE_PAIRING_FIXTURE") {
            use std::os::unix::fs::PermissionsExt;
            let fixture = PathBuf::from(path);
            assert_eq!(fixture.file_name().unwrap(), "rm02-pairing.json");
            assert!(!fixture.exists(), "refuse a stale native fixture");
            let native: IssuedPairing = local
                .call_as(
                    methods::CREATE_REMOTE_PAIRING,
                    json!({"label":"native UI","scopes":["environment-read","tasks-read"],
                    "pairingTtlSecs":300,"sessionTtlSecs":3600}),
                )
                .await
                .unwrap();
            let native_id = native.pairing.id.clone();
            let first = PairingTicket {
                host_name: Some("RM02 QA first Mac".into()),
                pairing_id: native.pairing.id,
                credential: native.credential,
                expires_at_ms: native.pairing.expires_at_ms,
                ..ticket.clone()
            };
            let second_root = tempfile::tempdir().unwrap();
            let second_core = assemble(second_root.path());
            let second_local = luna_rpc::memory_client(second_core.rpc_service());
            let second_bind = free_bind();
            let second_status = enable(&second_local, &second_bind).await;
            let native: IssuedPairing = second_local
                .call_as(
                    methods::CREATE_REMOTE_PAIRING,
                    json!({"label":"native UI","scopes":["environment-read","tasks-read"],
                    "pairingTtlSecs":300,"sessionTtlSecs":3600}),
                )
                .await
                .unwrap();
            let second_id = native.pairing.id.clone();
            let second = PairingTicket {
                host_name: Some("RM02 QA second Mac".into()),
                host: HostTicket {
                    target: native.target,
                    endpoints: vec![format!("https://{second_bind}")],
                    spki_sha256: second_status["spkiSha256"].as_str().unwrap().into(),
                    version: 1,
                },
                pairing_id: native.pairing.id,
                credential: native.credential,
                expires_at_ms: native.pairing.expires_at_ms,
                scopes: first.scopes.clone(),
                version: 1,
            };
            let mut wrong_pin = first.clone();
            wrong_pin.host.spki_sha256 = second.host.spki_sha256.clone();
            let mut wrong_profile = first.clone();
            wrong_profile.host.endpoints = second.host.endpoints.clone();
            wrong_profile.host.spki_sha256 = second.host.spki_sha256.clone();
            std::fs::write(&fixture, serde_json::to_vec(&json!({"code":first.encode().unwrap(),
                    "name":first.host_name, "secondCode":second.encode().unwrap(), "secondName":second.host_name,
                    "wrongPinCode":wrong_pin.encode().unwrap(),"wrongProfileCode":wrong_profile.encode().unwrap(),
                    // IOS-00: the live socket's typed read must name this device.
                    "deviceId":first.host.target.device_id})).unwrap()).unwrap();
            std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o600)).unwrap();
            let directory = fixture.parent().unwrap();
            wait_native_file(&directory.join("rm02-revoke-ready.json")).await;
            let a: Vec<SessionView> = local
                .call_as(methods::LIST_REMOTE_SESSIONS, json!({}))
                .await
                .unwrap();
            let first_session = a
                .iter()
                .find(|row| row.pairing_id.as_deref() == Some(&native_id))
                .unwrap();
            assert!(
                !first_session.revoked && first_session.last_used_at_ms.is_some(),
                "native Forget preserved server authority"
            );
            let b: Vec<SessionView> = second_local
                .call_as(methods::LIST_REMOTE_SESSIONS, json!({}))
                .await
                .unwrap();
            let second_session = b
                .iter()
                .find(|row| row.pairing_id.as_deref() == Some(&second_id))
                .unwrap();
            assert!(
                second_session.last_used_at_ms.is_some(),
                "the other native host remains authenticated"
            );
            second_local
                .call(
                    methods::REVOKE_REMOTE_SESSION,
                    json!({"id":second_session.id}),
                )
                .await
                .unwrap();
            std::fs::write(directory.join("rm02-revoked.json"), b"{}").unwrap();
            let result = directory.join("rm02-native-result.json");
            wait_native_file(&result).await;
            let result: Value = serde_json::from_slice(&std::fs::read(result).unwrap()).unwrap();
            for field in [
                "paired",
                "restored",
                "forgotten",
                "cameraFallback",
                "forgetPreservedOtherHost",
                "revokedDenied",
                "wrongPinRejected",
                "wrongProfileRejected",
            ] {
                assert_eq!(result[field], true, "native observation missing: {field}");
            }
            local
                .call(
                    methods::REVOKE_REMOTE_SESSION,
                    json!({"id":first_session.id}),
                )
                .await
                .unwrap();
            shutdown(second_core).await;
        }
        shutdown(core).await;
    }

    /// IOS-00 P01, core half: the mobile SDK session (`luna_mobile::session`)
    /// against the real gateway. The pinned descriptor and the SDK's typed
    /// `engineInfo` read name the host; a saved profile the descriptor
    /// contradicts is refused before the bearer is offered; 40 cancelled
    /// watches on one socket fit the gateway's 32 in-flight slots only if
    /// every cancellation reaches the engine; a cancelled read leaves the
    /// socket usable; a released session ends its watch and closes its
    /// transport; a revoke closes the open socket and the next connect must
    /// re-pair. Failure contract: docs/pending-tasks/ios-parity/ios-00/README.md.
    /// With LUNA_NATIVE_SESSION_FIXTURE the same host then serves the native
    /// campaign (`LunaTests/RemoteSessionTests`), which replaces the Rust
    /// stand-in below with the shipped Swift transport.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mobile_session_reads_typed_identity_and_every_cancellation_reaches_the_engine() {
        use luna_mobile::runtime::Cancellation;
        use luna_mobile::session::{
            RemoteSession, RemoteSessionError, RemoteSessionState, SessionCloseReason,
            TaskCommandChange,
        };

        let root = tempfile::tempdir().unwrap();
        let core = assemble(root.path());
        let local = luna_rpc::memory_client(core.rpc_service());
        let bind = free_bind();
        let status = enable(&local, &bind).await;
        if firewalled(&bind).await {
            shutdown(core).await;
            return;
        }
        let pin = status["spkiSha256"].as_str().unwrap().to_owned();
        let http = http(&pin);
        let engine: luna_proto::EngineInfo = local
            .call_as(methods::ENGINE_INFO, json!({}))
            .await
            .unwrap();
        let issued = pair(&local, &http, &bind, "ios00", 3600).await;
        let mac = saved_mac(&issued, &bind, &pin, "IOS00 QA Mac");
        // No command has touched this task: the engine's durable store
        // answers its zero snapshot, and the watch is a real engine stream.
        let task_id = "ios00-watched-task";

        // F6: a contradicting host never sees the bearer.
        let mut foreign = issued.clone();
        foreign.target.environment_id = "foreign-profile".into();
        let transport = PlatformStandIn::new(&pin, &bind);
        let session = RemoteSession::new(
            saved_mac(&foreign, &bind, &pin, "IOS00 QA foreign"),
            0,
            transport.clone(),
            None,
        )
        .unwrap();
        transport.attach(&session);
        let refused = session.connect(None).await.unwrap_err();
        assert!(
            matches!(refused, RemoteSessionError::IdentityMismatch { .. }),
            "{refused:?}"
        );
        assert_eq!(
            transport.paths(),
            ["/remote/v1/descriptor"],
            "no socket ticket was requested for a contradicting host"
        );
        assert!(matches!(
            session.state(),
            RemoteSessionState::Closed {
                reason: SessionCloseReason::Rejected,
                ..
            }
        ));
        drop(session);

        // The typed descriptor and engine read over one socket.
        let transport = PlatformStandIn::new(&pin, &bind);
        let session = RemoteSession::new(mac.clone(), 0, transport.clone(), None).unwrap();
        transport.attach(&session);
        let host = session.connect(None).await.unwrap();
        assert_eq!(host.device_id, engine.device_id);
        assert_eq!(host.environment_id, issued.target.environment_id);
        assert_eq!(host.catalog_version, luna_rpc::operations::CATALOG_VERSION);
        assert_eq!(host.api_version, luna_rpc::operations::CATALOG_VERSION);
        assert!(
            matches!(session.connect(None).await, Err(RemoteSessionError::Busy)),
            "one connect per session"
        );
        assert_eq!(
            transport.paths(),
            [
                "/remote/v1/descriptor",
                "/remote/v1/socket-ticket",
                "/remote/v1/ws"
            ]
        );
        assert!(
            transport
                .urls
                .lock()
                .unwrap()
                .iter()
                .all(|url| !url.contains(&issued.session_token)),
            "no secret travels in a URL"
        );

        // F8: each watch holds one of the socket's 32 in-flight slots until
        // its engine stream is cancelled; a lost cancellation fails by 33.
        for round in 1..=40 {
            let watch = session
                .watch_task_commands(task_id.into(), None)
                .await
                .unwrap_or_else(|error| panic!("round {round}: {error}"));
            match watch.next().await.unwrap() {
                Some(TaskCommandChange::Snapshot { state, .. }) => {
                    assert_eq!(state.task_id, task_id)
                }
                other => panic!("round {round}: a watch starts with its snapshot, got {other:?}"),
            }
            watch.cancel();
            let ended = tokio::time::timeout(Duration::from_secs(5), watch.next())
                .await
                .expect("a cancelled watch ends");
            assert_eq!(ended, Ok(None), "round {round}");
        }

        // F9: a cancelled read reports cancellation and the socket stays usable.
        let cancelled = Cancellation::new();
        cancelled.cancel();
        assert_eq!(
            session.engine_info(Some(cancelled)).await,
            Err(RemoteSessionError::Cancelled)
        );
        assert_eq!(
            session.engine_info(None).await.unwrap().device_id,
            engine.device_id
        );
        assert_eq!(
            transport.sockets.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "every read and watch shared the one socket"
        );

        // F11: releasing the session ends its watch and closes its transport.
        let orphan = session
            .watch_task_commands(task_id.into(), None)
            .await
            .unwrap();
        assert!(matches!(
            orphan.next().await,
            Ok(Some(TaskCommandChange::Snapshot { .. }))
        ));
        drop(session);
        let ended = tokio::time::timeout(Duration::from_secs(5), orphan.next())
            .await
            .expect("a released session ends its watch");
        assert_eq!(ended, Ok(None));
        transport.closed().await;

        // F12: a revoke closes the open socket; the next connect must re-pair.
        let changes = Arc::new(Changes(tokio::sync::watch::Sender::new(0)));
        let transport = PlatformStandIn::new(&pin, &bind);
        let session =
            RemoteSession::new(mac.clone(), 0, transport.clone(), Some(changes.clone())).unwrap();
        transport.attach(&session);
        session.connect(None).await.unwrap();
        local
            .call(
                methods::REVOKE_REMOTE_SESSION,
                json!({"id": issued.session.id}),
            )
            .await
            .unwrap();
        let state = settled(&session, &changes, |state| {
            matches!(state, RemoteSessionState::Closed { .. })
        })
        .await;
        assert!(
            matches!(
                state,
                RemoteSessionState::Closed {
                    reason: SessionCloseReason::Remote,
                    ..
                }
            ),
            "{state:?}"
        );
        let transport = PlatformStandIn::new(&pin, &bind);
        let again = RemoteSession::new(mac, 0, transport.clone(), None).unwrap();
        transport.attach(&again);
        assert!(matches!(
            again.connect(None).await,
            Err(RemoteSessionError::RePair { .. })
        ));
        assert!(matches!(
            again.state(),
            RemoteSessionState::Closed {
                reason: SessionCloseReason::RePair,
                ..
            }
        ));

        // Optional native extension: the shipped Swift transport, UniFFI core
        // and SDK against this same host, coordinated through private files
        // in the hosted test's Documents. No production QA seam is used.
        if let Some(path) = std::env::var_os("LUNA_NATIVE_SESSION_FIXTURE") {
            use std::os::unix::fs::PermissionsExt;
            let fixture = PathBuf::from(path);
            assert_eq!(fixture.file_name().unwrap(), "ios00-session.json");
            assert!(!fixture.exists(), "refuse a stale native fixture");
            let native = pair(&local, &http, &bind, "IOS00 native", 3600).await;
            let access = saved_mac(&native, &bind, &pin, "IOS00 QA Mac");
            std::fs::write(
                &fixture,
                serde_json::to_vec(&json!({
                    "hostName": access.host_name, "hostTicket": access.host_ticket,
                    "sessionId": access.session_id, "sessionToken": access.session_token,
                    "expiresAtMs": access.expires_at_ms, "scopes": access.scopes,
                    "deviceId": engine.device_id, "taskId": task_id,
                }))
                .unwrap(),
            )
            .unwrap();
            std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o600)).unwrap();
            let directory = fixture.parent().unwrap();
            wait_native_file(&directory.join("ios00-revoke-ready.json")).await;
            let sessions: Vec<SessionView> = local
                .call_as(methods::LIST_REMOTE_SESSIONS, json!({}))
                .await
                .unwrap();
            let row = sessions
                .iter()
                .find(|row| row.id == native.session.id)
                .unwrap();
            assert!(
                !row.revoked && row.last_used_at_ms.is_some(),
                "the native sockets opened with this grant"
            );
            local
                .call(
                    methods::REVOKE_REMOTE_SESSION,
                    json!({"id": native.session.id}),
                )
                .await
                .unwrap();
            std::fs::write(directory.join("ios00-revoked.json"), b"{}").unwrap();
            let result = directory.join("ios00-native-result.json");
            wait_native_file(&result).await;
            let result: Value = serde_json::from_slice(&std::fs::read(result).unwrap()).unwrap();
            for field in [
                "typedRead",
                "resumedOnMain",
                "swiftCancelEndedRead",
                "socketReusedAfterCancel",
                "releaseEndedWatch",
                "remoteCloseDeliveredOnMain",
                "rePairAfterRevoke",
            ] {
                assert_eq!(result[field], true, "native observation missing: {field}");
            }
            assert_eq!(result["deviceId"], engine.device_id.as_str());
            assert_eq!(result["watchCycles"], 40);
            assert_eq!(result["apiVersion"], luna_rpc::operations::CATALOG_VERSION);
        }
        shutdown(core).await;
    }

    fn saved_mac(
        session: &IssuedSession,
        bind: &str,
        pin: &str,
        name: &str,
    ) -> luna_mobile::pairing::SavedMac {
        let host = luna_proto::remote_control::HostTicket {
            version: 1,
            target: session.target.clone(),
            endpoints: vec![format!("https://{bind}")],
            spki_sha256: pin.into(),
        };
        luna_mobile::pairing::SavedMac {
            host_name: name.into(),
            host_ticket: serde_json::to_string(&host).unwrap(),
            session_id: session.session.id.clone(),
            session_token: session.session_token.clone(),
            expires_at_ms: session.session.expires_at_ms,
            scopes: session
                .session
                .scopes
                .iter()
                .map(|scope| {
                    serde_json::to_value(scope)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .collect(),
            client_label: name.into(),
        }
    }

    /// The phone's platform transport, in Rust: pinned HTTPS and one pinned
    /// WebSocket reporting into the session exactly as the app's
    /// `PinnedSocketTransport` does. It decides nothing; the core does.
    struct PlatformStandIn {
        pin: String,
        bind: String,
        session: std::sync::Mutex<std::sync::Weak<luna_mobile::session::RemoteSession>>,
        urls: std::sync::Mutex<Vec<String>>,
        socket: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Message>>>,
        sockets: std::sync::atomic::AtomicUsize,
        closed: tokio::sync::watch::Sender<bool>,
    }

    impl PlatformStandIn {
        fn new(pin: &str, bind: &str) -> Arc<Self> {
            Arc::new(Self {
                pin: pin.into(),
                bind: bind.into(),
                session: std::sync::Mutex::new(std::sync::Weak::new()),
                urls: std::sync::Mutex::new(Vec::new()),
                socket: std::sync::Mutex::new(None),
                sockets: std::sync::atomic::AtomicUsize::new(0),
                closed: tokio::sync::watch::Sender::new(false),
            })
        }

        fn attach(&self, session: &Arc<luna_mobile::session::RemoteSession>) {
            *self.session.lock().unwrap() = Arc::downgrade(session);
        }

        /// The request paths in order (no host, no secret).
        fn paths(&self) -> Vec<String> {
            self.urls
                .lock()
                .unwrap()
                .iter()
                .map(|url| {
                    url.split_once("://")
                        .and_then(|(_, rest)| rest.find('/').map(|at| rest[at..].to_owned()))
                        .unwrap_or_default()
                })
                .collect()
        }

        fn route(&self) -> luna_mobile::probe::RouteObservation {
            luna_mobile::probe::RouteObservation {
                remote_address: Some(self.bind.clone()),
                local_address: None,
                interface: "other".into(),
                proxied: false,
                tls_version: None,
            }
        }

        /// Resolves once the core told the transport to close.
        async fn closed(&self) {
            let mut closed = self.closed.subscribe();
            let _ = tokio::time::timeout(Duration::from_secs(5), closed.wait_for(|closed| *closed))
                .await
                .expect("the core closed its transport");
        }
    }

    impl luna_mobile::session::SocketTransport for PlatformStandIn {
        fn send_request(&self, request: luna_mobile::pairing::EnrollmentRequest) {
            self.urls.lock().unwrap().push(request.url.clone());
            let session = self.session.lock().unwrap().clone();
            let client = http(&self.pin);
            let route = self.route();
            tokio::spawn(async move {
                let method = reqwest::Method::from_bytes(request.method.as_bytes()).unwrap();
                let mut call = client.request(method, &request.url);
                if !request.body.is_empty() {
                    call = call
                        .header("content-type", "application/json")
                        .body(request.body.clone());
                }
                if let Some(authorization) = &request.authorization {
                    call = call.header("authorization", authorization);
                }
                let reply = match call.send().await {
                    Ok(response) => {
                        let status = response.status().as_u16();
                        let body = response
                            .bytes()
                            .await
                            .map(|body| body.to_vec())
                            .unwrap_or_default();
                        Ok((status, body))
                    }
                    Err(error) => Err(error.to_string()),
                };
                let Some(session) = session.upgrade() else {
                    return;
                };
                match reply {
                    Ok((status, body)) => {
                        session.request_completed(request.request_id, status, body, route)
                    }
                    Err(detail) => session.request_failed(
                        request.request_id,
                        luna_mobile::probe::TransportFailure::Network { detail },
                    ),
                }
            });
        }

        fn open_socket(&self, request: luna_mobile::session::SocketOpenRequest) {
            self.urls.lock().unwrap().push(request.url.clone());
            self.sockets
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let session = self.session.lock().unwrap().clone();
            let (frames, mut outbound) = tokio::sync::mpsc::unbounded_channel::<Message>();
            *self.socket.lock().unwrap() = Some(frames);
            let tls = tls(&self.pin);
            tokio::spawn(async move {
                let mut upgrade = request.url.as_str().into_client_request().unwrap();
                upgrade
                    .headers_mut()
                    .insert("authorization", request.authorization.parse().unwrap());
                let opened = tokio_tungstenite::connect_async_tls_with_config(
                    upgrade,
                    None,
                    false,
                    Some(tokio_tungstenite::Connector::Rustls(tls)),
                )
                .await;
                let socket = match opened {
                    Ok((socket, _)) => socket,
                    Err(error) => {
                        if let Some(session) = session.upgrade() {
                            session.socket_closed(
                                request.socket_id,
                                luna_mobile::probe::TransportFailure::Network {
                                    detail: error.to_string(),
                                },
                            );
                        }
                        return;
                    }
                };
                match session.upgrade() {
                    Some(session) => session.socket_opened(request.socket_id),
                    None => return,
                }
                let (mut sink, mut stream) = socket.split();
                loop {
                    tokio::select! {
                        frame = outbound.recv() => match frame {
                            Some(frame) => if sink.send(frame).await.is_err() { break },
                            None => {
                                let _ = sink.send(Message::Close(None)).await;
                                break;
                            }
                        },
                        message = stream.next() => match message {
                            Some(Ok(Message::Text(text))) => match session.upgrade() {
                                Some(session) => session.receive(request.socket_id, text).await,
                                None => break,
                            },
                            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                            Some(Ok(_)) => {}
                        },
                    }
                }
                if let Some(session) = session.upgrade() {
                    session.socket_closed(
                        request.socket_id,
                        luna_mobile::probe::TransportFailure::Network {
                            detail: "The Mac closed the connection".into(),
                        },
                    );
                }
            });
        }

        fn send_frame(&self, _socket_id: u64, frame: String) {
            if let Some(frames) = &*self.socket.lock().unwrap() {
                let _ = frames.send(Message::Text(frame));
            }
        }

        fn close(&self) {
            self.socket.lock().unwrap().take();
            self.closed.send_replace(true);
        }
    }

    /// Counts the core's change notifications so a wait follows real events.
    struct Changes(tokio::sync::watch::Sender<u64>);

    impl luna_mobile::session::RemoteSessionListener for Changes {
        fn on_change(&self) {
            self.0.send_modify(|count| *count += 1);
        }
    }

    async fn settled(
        session: &luna_mobile::session::RemoteSession,
        changes: &Changes,
        done: impl Fn(&luna_mobile::session::RemoteSessionState) -> bool,
    ) -> luna_mobile::session::RemoteSessionState {
        let mut seen = changes.0.subscribe();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let state = session.state();
                if done(&state) {
                    return state;
                }
                seen.changed()
                    .await
                    .expect("the listener outlives the wait");
            }
        })
        .await
        .expect("the session reached the expected state")
    }

    async fn wait_native_file(path: &Path) {
        tokio::time::timeout(Duration::from_secs(360), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("native UI did not deliver its observation; inspect the XCTest artifact");
    }
}
