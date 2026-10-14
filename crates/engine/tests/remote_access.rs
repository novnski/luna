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

/// RM-04: the provider boundary for the mobile outbox campaign.
#[cfg(unix)]
mod remote_provider;

/// RM-04: [`assemble`] with `registry` as the engine's providers.
#[cfg(unix)]
fn assemble_with(root: &Path, registry: Arc<HarnessRegistry>) -> EngineCore {
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
        registry,
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

    /// RM-03 P1–P11, core half: the mobile runtime
    /// (`RemoteSession::following`) over the real gateway follows this Mac's
    /// own tasks. Its registry and a followed task's detail carry the host's
    /// task and entry ids and durable command position; Mac changes arrive as
    /// deltas; suspend and resume or a lost and returned network reconnect
    /// with a fresh ticket and resume from the held cursor (`current`, no
    /// resend); a change made while the phone was away is repaired with a
    /// snapshot; a saved cache comes back `cached` and is confirmed without a
    /// resend, while another host's cache is refused; a deleted task turns
    /// `removed`; at most four tasks are followed; a Mac restart is a new
    /// generation repaired by snapshot with every task identity kept; a
    /// revoke ends the runtime instead of retrying. Failure contract:
    /// docs/pending-tasks/control-this-mac/rm-03/README.md. With
    /// LUNA_NATIVE_RUNTIME_FIXTURE the restarted host then serves the native
    /// campaign (`LunaTests/RemoteTasksRuntimeTests`) through the app's
    /// shipped model, transport and lifecycle adapters.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mobile_runtime_follows_mac_tasks_across_suspend_network_restart_and_cursor_repair() {
        use luna_mobile::follow::{ProjectionChange, ProjectionFreshness};
        use luna_mobile::session::{RemoteSession, RemoteSessionState, SessionCloseReason};

        let root = tempfile::tempdir().unwrap();
        let mut core = assemble(root.path());
        let mut local = luna_rpc::memory_client(core.rpc_service());
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
        seed_rm03_tasks(&core, &local, root.path()).await;
        let host_tasks: std::collections::BTreeSet<String> = core
            .workspace
            .watch_chats()
            .borrow()
            .iter()
            .filter(|chat| chat.space_id.as_deref() == Some(RM03_SPACE))
            .map(|chat| chat.id.clone())
            .collect();
        assert_eq!(host_tasks.len(), 3, "the fixture seeded three listed tasks");
        let issued = pair(&local, &http, &bind, "rm03", 3600).await;
        let mac = saved_mac(&issued, &bind, &pin, "RM03 QA Mac");

        // P1: the cheap registry names this Mac and lists exactly its own
        // tasks, by the host's ids, never the removed project's.
        let changes = Arc::new(Changes(tokio::sync::watch::Sender::new(0)));
        let transport = PlatformStandIn::new(&pin, &bind);
        let session = RemoteSession::following(
            mac.clone(),
            0,
            transport.clone(),
            Some(changes.clone()),
            None,
        )
        .unwrap();
        transport.attach(&session);
        let host = session.connect(None).await.unwrap();
        assert_eq!(host.device_id, engine.device_id);
        let first = projected(&session, &changes, |tasks| {
            tasks.freshness == ProjectionFreshness::Live
        })
        .await;
        assert_eq!(first.device_id, engine.device_id);
        assert_eq!(first.last_change, ProjectionChange::Snapshot);
        assert_eq!(rm03_ids(&first), host_tasks, "the same Mac task identities");
        assert!(
            first.tasks.iter().all(|task| task.task_id != RM03_HIDDEN),
            "a removed project's task is not listed"
        );
        assert_eq!(first.total_tasks as usize, first.tasks.len());
        assert!(
            first.capabilities.iter().any(
                |capability| capability == luna_proto::remote_projection::REMOTE_PROJECTIONS_V1
            )
        );
        assert!(
            first
                .projects
                .iter()
                .any(|project| project.project_id == RM03_SPACE && project.name == "RM03 project")
                && first
                    .projects
                    .iter()
                    .all(|project| project.project_id != RM03_REMOVED_SPACE),
            "projects by name only, removed projects omitted: {:?}",
            first.projects
        );

        // P2: the detailed history of one followed task: its entries by the
        // host's ids, and the durable command position the host reads.
        session.follow_task(RM03_A.into()).unwrap();
        let detail = followed(&session, &changes, RM03_A, |task| {
            task.freshness == ProjectionFreshness::Live
        })
        .await;
        let entry_ids: Vec<&str> = detail
            .entries
            .iter()
            .map(|entry| entry.entry_id.as_str())
            .collect();
        assert_eq!(entry_ids, ["rm03-a-1", "rm03-a-2"]);
        assert_eq!(detail.entries[0].text, "First RM03 prompt");
        assert_eq!(
            detail.task.as_ref().unwrap().title.as_deref(),
            Some("RM03 alpha")
        );
        let commands: luna_proto::TaskCommandRead = local
            .call_as(
                methods::GET_TASK_COMMAND_SNAPSHOT,
                json!({"taskId": RM03_A}),
            )
            .await
            .unwrap();
        assert_eq!(detail.task_revision, commands.snapshot.task_revision);
        assert_eq!(detail.task_sequence, commands.snapshot.task_sequence);

        // P3: Mac changes arrive as deltas against the held cursor.
        core.workspace
            .rename_chat(RM03_B, "RM03 beta renamed")
            .unwrap();
        let renamed = projected(&session, &changes, |tasks| {
            title(tasks, RM03_B) == Some("RM03 beta renamed")
        })
        .await;
        assert_eq!(renamed.last_change, ProjectionChange::Delta);
        assert!(renamed.revision > first.revision);
        assert_eq!(renamed.generation, first.generation);
        core.doc_host
            .open(RM03_A)
            .unwrap()
            .write_user_message("rm03-a-3", "Third RM03 prompt", rm03_now() + 3)
            .unwrap();
        let grown = followed(&session, &changes, RM03_A, |task| task.entries.len() == 3).await;
        assert_eq!(grown.last_change, ProjectionChange::Delta);
        assert_eq!(grown.entries[2].entry_id, "rm03-a-3");

        // P4: suspend closes the socket and labels everything stale; resume
        // re-verifies on a new socket and the unchanged cursor is `current`.
        session.suspend();
        assert_eq!(session.state(), RemoteSessionState::Suspended);
        let away = session.remote_tasks().unwrap();
        assert_eq!(away.freshness, ProjectionFreshness::Stale);
        assert_eq!(
            session.remote_task(RM03_A.into()).unwrap().freshness,
            ProjectionFreshness::Stale
        );
        session.resume();
        let resumed = projected(&session, &changes, |tasks| {
            tasks.freshness == ProjectionFreshness::Live
        })
        .await;
        assert_eq!(
            resumed.last_change,
            ProjectionChange::Current,
            "nothing was resent"
        );
        assert_eq!(
            (resumed.generation.as_str(), resumed.revision),
            (renamed.generation.as_str(), renamed.revision)
        );
        let detail = followed(&session, &changes, RM03_A, |task| {
            task.freshness == ProjectionFreshness::Live
        })
        .await;
        assert_eq!(detail.last_change, ProjectionChange::Current);

        // P5: a change made while the phone was away is a cursor the host
        // no longer serves: the registry is repaired with a snapshot.
        session.suspend();
        core.workspace
            .rename_chat(RM03_C, "RM03 gamma while away")
            .unwrap();
        session.resume();
        let repaired = projected(&session, &changes, |tasks| {
            tasks.freshness == ProjectionFreshness::Live
        })
        .await;
        assert_eq!(repaired.last_change, ProjectionChange::Snapshot);
        assert_eq!(title(&repaired, RM03_C), Some("RM03 gamma while away"));
        assert!(repaired.revision > resumed.revision);
        assert_eq!(rm03_ids(&repaired), host_tasks);

        // P6: no network closes the socket and waits; the network's return
        // reconnects at once and resumes from the cursor.
        session.network_changed(false);
        assert_eq!(session.state(), RemoteSessionState::WaitingForNetwork);
        assert_eq!(
            session.remote_tasks().unwrap().freshness,
            ProjectionFreshness::Stale
        );
        session.network_changed(true);
        let returned = projected(&session, &changes, |tasks| {
            tasks.freshness == ProjectionFreshness::Live
        })
        .await;
        assert_eq!(returned.last_change, ProjectionChange::Current);
        let sockets = transport.sockets.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(sockets, 4, "the first socket plus three reconnects");
        let paths = transport.paths();
        assert_eq!(
            paths
                .iter()
                .filter(|path| *path == "/remote/v1/socket-ticket")
                .count(),
            sockets,
            "every reconnect bought a fresh ticket with the saved bearer: {paths:?}"
        );

        // P9: a deleted task turns `removed` on its followed detail and
        // leaves the registry; at most four tasks are followed, and
        // re-following resumes from the held cursor.
        session.follow_task(RM03_C.into()).unwrap();
        followed(&session, &changes, RM03_C, |task| {
            task.freshness == ProjectionFreshness::Live
        })
        .await;
        assert!(core.workspace.delete_chat(RM03_C).unwrap());
        let deleted = followed(&session, &changes, RM03_C, |task| task.removed).await;
        assert_eq!(deleted.last_change, ProjectionChange::Removed);
        assert!(deleted.entries.is_empty() && deleted.task.is_none());
        let shrunk = projected(&session, &changes, |tasks| {
            tasks.tasks.iter().all(|task| task.task_id != RM03_C)
        })
        .await;
        assert_eq!(shrunk.last_change, ProjectionChange::Delta);
        for missing in ["rm03-missing-1", "rm03-missing-2", "rm03-missing-3"] {
            session.follow_task(missing.into()).unwrap();
        }
        assert_eq!(
            session.followed_tasks(),
            [RM03_C, "rm03-missing-1", "rm03-missing-2", "rm03-missing-3"],
            "the least recently followed task stopped"
        );
        let missing = followed(&session, &changes, "rm03-missing-3", |task| task.removed).await;
        assert_eq!(missing.last_change, ProjectionChange::Removed);
        let released = session
            .remote_task(RM03_A.into())
            .expect("an unfollowed task keeps its cached detail");
        assert_eq!(
            released.freshness,
            ProjectionFreshness::Stale,
            "a detail nobody follows is not live"
        );
        session.follow_task(RM03_A.into()).unwrap();
        let refollowed = followed(&session, &changes, RM03_A, |task| {
            task.freshness == ProjectionFreshness::Live
        })
        .await;
        assert_eq!(refollowed.last_change, ProjectionChange::Current);
        assert_eq!(
            refollowed.revision, released.revision,
            "resumed from its cursor"
        );

        // P7: the phone restarts. Its saved cache comes back `cached` before
        // any connection, and the first socket confirms it with `current`.
        let cache = session.cache_bytes();
        // `luna_sdk::MAX_CACHE_BYTES`; the engine tests do not link the SDK.
        assert!(!cache.is_empty() && cache.len() <= 4 * 1024 * 1024);
        let before_restart = session.remote_tasks().unwrap();
        drop(session);
        transport.closed().await;
        let changes = Arc::new(Changes(tokio::sync::watch::Sender::new(0)));
        let transport = PlatformStandIn::new(&pin, &bind);
        let restarted = RemoteSession::following(
            mac.clone(),
            0,
            transport.clone(),
            Some(changes.clone()),
            Some(cache.clone()),
        )
        .unwrap();
        transport.attach(&restarted);
        assert_eq!(restarted.cache_note(), None);
        let cached = restarted.remote_tasks().unwrap();
        assert_eq!(cached.freshness, ProjectionFreshness::Cached);
        assert_eq!(cached.last_change, ProjectionChange::Restored);
        assert_eq!(cached.tasks, before_restart.tasks);
        assert_eq!(
            restarted.remote_task(RM03_A.into()).unwrap().freshness,
            ProjectionFreshness::Cached
        );
        assert!(transport.paths().is_empty(), "restoring touched no network");
        restarted.follow_task(RM03_A.into()).unwrap();
        restarted.connect(None).await.unwrap();
        let confirmed = projected(&restarted, &changes, |tasks| {
            tasks.freshness == ProjectionFreshness::Live
        })
        .await;
        assert_eq!(confirmed.last_change, ProjectionChange::Current);
        assert_eq!(confirmed.revision, before_restart.revision);

        // P8: a cache saved for another host or profile is never shown.
        let mut foreign = issued.clone();
        foreign.target.environment_id = "foreign-profile".into();
        let other = RemoteSession::following(
            saved_mac(&foreign, &bind, &pin, "RM03 QA foreign"),
            0,
            PlatformStandIn::new(&pin, &bind),
            None,
            Some(cache.clone()),
        )
        .unwrap();
        assert!(other.remote_tasks().is_none() && other.remote_task(RM03_A.into()).is_none());
        assert!(other.cache_note().is_some(), "the refusal is explained");
        drop(other);

        // P10: the Mac restarts (same profile and pin). The runtime
        // reconnects by itself; the new generation is repaired by snapshot
        // and every task keeps its identity.
        shutdown(core).await;
        projected(&restarted, &changes, |tasks| {
            tasks.freshness == ProjectionFreshness::Stale
        })
        .await;
        core = assemble(root.path());
        local = luna_rpc::memory_client(core.rpc_service());
        let reborn = projected_within(&restarted, &changes, Duration::from_secs(90), |tasks| {
            tasks.freshness == ProjectionFreshness::Live && tasks.generation != confirmed.generation
        })
        .await;
        assert_eq!(reborn.last_change, ProjectionChange::Snapshot);
        assert_eq!(
            reborn.tasks, confirmed.tasks,
            "the same tasks, rows and order"
        );
        let detail = followed(&restarted, &changes, RM03_A, |task| {
            task.freshness == ProjectionFreshness::Live
                && task.generation.as_deref() == Some(reborn.generation.as_str())
        })
        .await;
        assert_eq!(detail.last_change, ProjectionChange::Snapshot);
        assert_eq!(detail.entries.len(), 3);

        // P11: a revoke closes the socket; the next attempt is refused and
        // the runtime ends instead of retrying.
        local
            .call(
                methods::REVOKE_REMOTE_SESSION,
                json!({"id": issued.session.id}),
            )
            .await
            .unwrap();
        let state = settled(&restarted, &changes, |state| {
            matches!(state, RemoteSessionState::Closed { .. })
        })
        .await;
        assert!(
            matches!(
                state,
                RemoteSessionState::Closed {
                    reason: SessionCloseReason::RePair,
                    ..
                }
            ),
            "{state:?}"
        );
        transport.closed().await;
        assert_eq!(
            restarted.remote_tasks().unwrap().freshness,
            ProjectionFreshness::Stale,
            "data seen before the revoke stays labelled, never live"
        );

        if let Some(path) = std::env::var_os("LUNA_NATIVE_RUNTIME_FIXTURE") {
            rm03_native_campaign(PathBuf::from(path), root.path(), core, &bind, &pin).await;
            return;
        }
        shutdown(core).await;
    }

    const RM03_SPACE: &str = "rm03-space";
    const RM03_REMOVED_SPACE: &str = "rm03-removed-space";
    const RM03_A: &str = "rm03-task-a";
    const RM03_B: &str = "rm03-task-b";
    const RM03_C: &str = "rm03-task-c";
    const RM03_HIDDEN: &str = "rm03-task-hidden";

    fn rm03_now() -> i64 {
        chrono::Utc::now().timestamp_millis()
    }

    /// One project with three tasks (two transcript entries on the first),
    /// and a removed project whose task must stay hidden.
    async fn seed_rm03_tasks(core: &EngineCore, local: &luna_rpc::RpcClient, root: &Path) {
        let folder = |name: &str| {
            let path = root.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path.canonicalize().unwrap().to_string_lossy().to_string()
        };
        let cwd = folder("rm03-project");
        let hidden_cwd = folder("rm03-removed-project");
        core.workspace
            .create_space(
                RM03_SPACE,
                &core.device_id,
                &cwd,
                Some("RM03 project".into()),
                false,
            )
            .unwrap();
        core.workspace
            .create_space(
                RM03_REMOVED_SPACE,
                &core.device_id,
                &hidden_cwd,
                None,
                false,
            )
            .unwrap();
        for (task, space, path, title) in [
            (RM03_A, RM03_SPACE, &cwd, "RM03 alpha"),
            (RM03_B, RM03_SPACE, &cwd, "RM03 beta"),
            (RM03_C, RM03_SPACE, &cwd, "RM03 gamma"),
            (RM03_HIDDEN, RM03_REMOVED_SPACE, &hidden_cwd, "RM03 hidden"),
        ] {
            local
                .call(
                    methods::MUTATE,
                    json!({"op": "createChat", "chatId": task, "spaceId": space, "cwd": path}),
                )
                .await
                .unwrap();
            core.workspace.rename_chat(task, title).unwrap();
        }
        assert!(core.workspace.remove_space(RM03_REMOVED_SPACE).unwrap());
        let handle = core.doc_host.open(RM03_A).unwrap();
        handle
            .write_user_message("rm03-a-1", "First RM03 prompt", rm03_now())
            .unwrap();
        handle
            .write_user_message("rm03-a-2", "Second RM03 prompt", rm03_now() + 1)
            .unwrap();
    }

    fn rm03_ids(
        tasks: &luna_mobile::follow::RemoteTasksProjection,
    ) -> std::collections::BTreeSet<String> {
        tasks
            .tasks
            .iter()
            .map(|task| task.task_id.clone())
            .collect()
    }

    fn title<'a>(
        tasks: &'a luna_mobile::follow::RemoteTasksProjection,
        task_id: &str,
    ) -> Option<&'a str> {
        tasks
            .tasks
            .iter()
            .find(|task| task.task_id == task_id)?
            .title
            .as_deref()
    }

    async fn projected(
        session: &luna_mobile::session::RemoteSession,
        changes: &Changes,
        done: impl Fn(&luna_mobile::follow::RemoteTasksProjection) -> bool,
    ) -> luna_mobile::follow::RemoteTasksProjection {
        projected_within(session, changes, Duration::from_secs(30), done).await
    }

    /// Waits on the session's change notifications (never a timer) until
    /// its registry projection satisfies `done`. Reading the state re-arms
    /// the coalesced notification.
    async fn projected_within(
        session: &luna_mobile::session::RemoteSession,
        changes: &Changes,
        within: Duration,
        done: impl Fn(&luna_mobile::follow::RemoteTasksProjection) -> bool,
    ) -> luna_mobile::follow::RemoteTasksProjection {
        let mut seen = changes.0.subscribe();
        let waited = tokio::time::timeout(within, async {
            loop {
                let _ = session.state();
                if let Some(tasks) = session.remote_tasks()
                    && done(&tasks)
                {
                    return tasks;
                }
                seen.changed()
                    .await
                    .expect("the listener outlives the wait");
            }
        })
        .await;
        waited.unwrap_or_else(|_| {
            panic!(
                "the registry projection did not reach the expected state; state {:?}, issue {:?}, last {:?}",
                session.state(),
                session.projection_issue(),
                session.remote_tasks()
            )
        })
    }

    async fn followed(
        session: &luna_mobile::session::RemoteSession,
        changes: &Changes,
        task_id: &str,
        done: impl Fn(&luna_mobile::follow::RemoteTaskProjection) -> bool,
    ) -> luna_mobile::follow::RemoteTaskProjection {
        let mut seen = changes.0.subscribe();
        let waited = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let _ = session.state();
                if let Some(task) = session.remote_task(task_id.into())
                    && done(&task)
                {
                    return task;
                }
                seen.changed()
                    .await
                    .expect("the listener outlives the wait");
            }
        })
        .await;
        waited.unwrap_or_else(|_| {
            panic!(
                "task {task_id}'s projection did not reach the expected state; state {:?}, last {:?}",
                session.state(),
                session.remote_task(task_id.into())
            )
        })
    }

    /// The native half (LUNA_NATIVE_RUNTIME_FIXTURE): the app pairs through
    /// its own model from a `luna://pair` code minted by CreateRemotePairing,
    /// then the host and `LunaTests/RemoteTasksRuntimeTests` take turns
    /// through private cue files in the hosted test's Documents.
    async fn rm03_native_campaign(
        fixture: PathBuf,
        root: &Path,
        core: EngineCore,
        bind: &str,
        pin: &str,
    ) {
        use luna_proto::remote_control::{HostTicket, PairingTicket};
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(fixture.file_name().unwrap(), "rm03-runtime.json");
        assert!(!fixture.exists(), "refuse a stale native fixture");
        let directory = fixture.parent().unwrap().to_path_buf();
        let mut core = core;
        let mut local = luna_rpc::memory_client(core.rpc_service());
        let engine: luna_proto::EngineInfo = local
            .call_as(methods::ENGINE_INFO, json!({}))
            .await
            .unwrap();
        let issued: IssuedPairing = local
            .call_as(
                methods::CREATE_REMOTE_PAIRING,
                json!({"label": "RM03 native", "scopes": ["environment-read", "tasks-read"],
                    "pairingTtlSecs": 300, "sessionTtlSecs": 3600}),
            )
            .await
            .unwrap();
        let code = PairingTicket {
            version: 1,
            host_name: Some("RM03 QA Mac".into()),
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
        }
        .encode()
        .unwrap();
        let tasks: Vec<Value> = core
            .workspace
            .watch_chats()
            .borrow()
            .iter()
            .filter(|chat| chat.space_id.as_deref() == Some(RM03_SPACE))
            .map(|chat| json!({"taskId": chat.id, "title": chat.title}))
            .collect();
        std::fs::write(
            &fixture,
            serde_json::to_vec(&json!({
                "code": code, "name": "RM03 QA Mac", "deviceId": engine.device_id,
                "tasks": tasks, "followTaskId": RM03_A,
                "entryIds": ["rm03-a-1", "rm03-a-2", "rm03-a-3"],
                "renameTaskId": RM03_B, "renamedTitle": "RM03 beta native",
                "awayTaskId": RM03_A, "awayTitle": "RM03 alpha while backgrounded",
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o600)).unwrap();

        wait_native_file(&directory.join("rm03-native-live.json")).await;
        core.workspace
            .rename_chat(RM03_B, "RM03 beta native")
            .unwrap();
        std::fs::write(directory.join("rm03-host-renamed.json"), b"{}").unwrap();

        wait_native_file(&directory.join("rm03-native-suspended.json")).await;
        core.workspace
            .rename_chat(RM03_A, "RM03 alpha while backgrounded")
            .unwrap();
        std::fs::write(directory.join("rm03-host-changed.json"), b"{}").unwrap();

        wait_native_file(&directory.join("rm03-native-restarted.json")).await;
        shutdown(core).await;
        core = assemble(root);
        local = luna_rpc::memory_client(core.rpc_service());
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let status = local.call("GetRemoteAccess", json!({})).await.unwrap();
                if status["listening"] == true {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("enabled remote access resumes on start");
        std::fs::write(directory.join("rm03-host-restarted.json"), b"{}").unwrap();

        let result = directory.join("rm03-native-result.json");
        wait_native_file(&result).await;
        let result: Value = serde_json::from_slice(&std::fs::read(result).unwrap()).unwrap();
        for field in [
            "pairedFromLink",
            "sameTaskIdentities",
            "detailEntries",
            "liveDelta",
            "staleWhileBackgrounded",
            "repairedAfterForeground",
            "waitingWithoutNetwork",
            "currentAfterNetwork",
            "cachedAfterRestart",
            "confirmedAfterRestart",
            "repairedAfterMacRestart",
            "resultsOnMain",
            "cacheRemovedOnForget",
        ] {
            assert_eq!(result[field], true, "native observation missing: {field}");
        }
        assert_eq!(result["deviceId"], engine.device_id.as_str());
        let sessions: Vec<SessionView> = local
            .call_as(methods::LIST_REMOTE_SESSIONS, json!({}))
            .await
            .unwrap();
        let native = sessions
            .iter()
            .find(|row| row.pairing_id.as_deref() == Some(issued.pairing.id.as_str()))
            .expect("the native phone redeemed the link");
        assert!(native.last_used_at_ms.is_some());
        // Forget on the phone is local; end the grant on the host too.
        local
            .call(methods::REVOKE_REMOTE_SESSION, json!({"id": native.id}))
            .await
            .unwrap();
        shutdown(core).await;
    }

    const IOS02_ALPHA: &str = "ios02-alpha";
    const IOS02_BETA: &str = "ios02-beta";
    const IOS02_PINNED: &str = "ios02-task-pinned";
    const IOS02_SELECT: &str = "ios02-task-select";
    const IOS02_DELETE: &str = "ios02-task-delete";
    const IOS02_ARCHIVED: &str = "ios02-task-archived";
    const IOS02_OTHER: &str = "ios02-task-other";

    /// IOS-02 P03, the host half of a native-only campaign. A disposable
    /// engine with two projects and five tasks (one pinned, one archived,
    /// one with unseen activity and two messages) is paired by the shipped
    /// app through its own UI; on the UI test's cues it deletes the task the
    /// phone has open, stops, and restarts with the same pin and grant, then
    /// checks the public result and revokes the grant. It needs the iOS
    /// Simulator and `LunaUITests/AdaptiveShellUITests`, so CI skips it:
    /// `scripts/ios/qa-native-session-sim.py --campaign ios02` runs both.
    /// Failure contract: docs/pending-tasks/ios-parity/ios-02/README.md.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "native IOS-02 campaign: scripts/ios/qa-native-session-sim.py --campaign ios02"]
    async fn ios02_native_shell_serves_the_phone_and_deletes_stops_and_restarts_on_cue() {
        use luna_proto::remote_control::{HostTicket, PairingTicket};
        use std::os::unix::fs::PermissionsExt;

        let fixture = PathBuf::from(
            std::env::var_os("LUNA_NATIVE_SHELL_FIXTURE")
                .expect("LUNA_NATIVE_SHELL_FIXTURE names the UI test runner's fixture file"),
        );
        assert_eq!(fixture.file_name().unwrap(), "ios02-shell.json");
        assert!(!fixture.exists(), "refuse a stale native fixture");
        let directory = fixture.parent().unwrap().to_path_buf();
        let root = tempfile::tempdir().unwrap();
        let mut core = assemble(root.path());
        let mut local = luna_rpc::memory_client(core.rpc_service());
        let bind = free_bind();
        let status = enable(&local, &bind).await;
        assert!(
            !firewalled(&bind).await,
            "the native campaign needs this Mac's LAN address in LUNA_REMOTE_TEST_IP"
        );
        let pin = status["spkiSha256"].as_str().unwrap().to_owned();
        let engine: luna_proto::EngineInfo = local
            .call_as(methods::ENGINE_INFO, json!({}))
            .await
            .unwrap();
        seed_ios02_tasks(&core, &local, root.path()).await;
        let issued: IssuedPairing = local
            .call_as(
                methods::CREATE_REMOTE_PAIRING,
                json!({"label": "IOS02 native", "scopes": ["environment-read", "tasks-read"],
                    "pairingTtlSecs": 600, "sessionTtlSecs": 3600}),
            )
            .await
            .unwrap();
        let code = PairingTicket {
            version: 1,
            host_name: Some("IOS02 QA Mac".into()),
            host: HostTicket {
                version: 1,
                target: issued.target.clone(),
                endpoints: vec![format!("https://{bind}")],
                spki_sha256: pin,
            },
            pairing_id: issued.pairing.id.clone(),
            credential: issued.credential.clone(),
            expires_at_ms: issued.pairing.expires_at_ms,
            scopes: vec!["environment-read".into(), "tasks-read".into()],
        }
        .encode()
        .unwrap();
        let mut fields = json!({
            "code": code, "name": "IOS02 QA Mac", "deviceId": engine.device_id,
            "alphaProject": "IOS02 Alpha project", "betaProject": "IOS02 Beta project",
            "selectEntryIds": ["ios02-select-1", "ios02-select-2"],
            "selectEntries": ["First IOS02 prompt", "Second IOS02 prompt"]
        });
        for (key, value) in [
            ("pinnedTaskId", IOS02_PINNED),
            ("pinnedTitle", "IOS02 pinned plan"),
            ("selectTaskId", IOS02_SELECT),
            ("selectTitle", "IOS02 selected task"),
            ("deleteTaskId", IOS02_DELETE),
            ("deleteTitle", "IOS02 task to delete"),
            ("archivedTaskId", IOS02_ARCHIVED),
            ("archivedTitle", "IOS02 archived task"),
            ("otherTaskId", IOS02_OTHER),
            ("otherTitle", "IOS02 other task"),
        ] {
            fields[key] = value.into();
        }
        std::fs::write(&fixture, serde_json::to_vec(&fields).unwrap()).unwrap();
        std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o600)).unwrap();

        // The task the phone has open is deleted on the Mac.
        wait_native_file(&directory.join("ios02-native-delete.json")).await;
        assert!(core.workspace.delete_chat(IOS02_DELETE).unwrap());
        std::fs::write(directory.join("ios02-host-deleted.json"), b"{}").unwrap();

        // The Mac goes away while the phone shows its tasks.
        wait_native_file(&directory.join("ios02-native-stop.json")).await;
        shutdown(core).await;
        std::fs::write(directory.join("ios02-host-stopped.json"), b"{}").unwrap();

        // It comes back on the same route with the same pin and grant.
        wait_native_file(&directory.join("ios02-native-restart.json")).await;
        core = assemble(root.path());
        local = luna_rpc::memory_client(core.rpc_service());
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let status = local.call("GetRemoteAccess", json!({})).await.unwrap();
                if status["listening"] == true {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("enabled remote access resumes on start");
        std::fs::write(directory.join("ios02-host-restarted.json"), b"{}").unwrap();

        let result = directory.join("ios02-native-result.json");
        wait_native_file(&result).await;
        let result: Value = serde_json::from_slice(&std::fs::read(result).unwrap()).unwrap();
        for field in [
            "pairedThroughTheApp",
            "listedHostTasks",
            "pinnedFirst",
            "voiceOverLabels",
            "filters",
            "projectScope",
            "keyboardSearch",
            "liveDetail",
            "unreadKept",
            "selectionSurvivedRotation",
            "draftSurvivedRotation",
            "restoredSelectionAndDraft",
            "keyboardNextTask",
            "deletedWhileOpenTruthful",
            "draftKeptAfterDelete",
            "restoredDeletedRouteTruthful",
            "staleWhenMacStopped",
            "cachedWhenRelaunchedOffline",
            "retryStaysTruthful",
            "keyboardReconnect",
            "liveAfterRestart",
            "keyboardSettings",
            "appearancePersisted",
            "largeTextControlsReachable",
            "accessibilityAudit",
            "forgotten",
        ] {
            assert_eq!(result[field], true, "native observation missing: {field}");
        }
        assert_eq!(result["deviceId"], engine.device_id.as_str());
        let sessions: Vec<SessionView> = local
            .call_as(methods::LIST_REMOTE_SESSIONS, json!({}))
            .await
            .unwrap();
        let native = sessions
            .iter()
            .find(|row| row.pairing_id.as_deref() == Some(issued.pairing.id.as_str()))
            .expect("the app redeemed the code through its own UI");
        // Forget on the phone is local; end the grant on the host too.
        local
            .call(methods::REVOKE_REMOTE_SESSION, json!({"id": native.id}))
            .await
            .unwrap();
        shutdown(core).await;
    }

    /// Two projects and five tasks with distinct activity times, so the
    /// Mac's own order is pinned first, then the newest: one pinned, one
    /// archived, one with unseen activity and two messages, the rest seen.
    async fn seed_ios02_tasks(core: &EngineCore, local: &luna_rpc::RpcClient, root: &Path) {
        let folder = |name: &str| {
            let path = root.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path.canonicalize().unwrap().to_string_lossy().to_string()
        };
        let alpha = folder("ios02-alpha");
        let beta = folder("ios02-beta");
        for (space, path, name) in [
            (IOS02_ALPHA, &alpha, "IOS02 Alpha project"),
            (IOS02_BETA, &beta, "IOS02 Beta project"),
        ] {
            core.workspace
                .create_space(space, &core.device_id, path, Some(name.into()), false)
                .unwrap();
        }
        let now = rm03_now();
        for (task, space, path, title, minutes) in [
            (IOS02_PINNED, IOS02_ALPHA, &alpha, "IOS02 pinned plan", 50),
            (IOS02_SELECT, IOS02_ALPHA, &alpha, "IOS02 selected task", 1),
            (IOS02_DELETE, IOS02_BETA, &beta, "IOS02 task to delete", 2),
            (IOS02_ARCHIVED, IOS02_BETA, &beta, "IOS02 archived task", 3),
            (IOS02_OTHER, IOS02_BETA, &beta, "IOS02 other task", 4),
        ] {
            local
                .call(
                    methods::MUTATE,
                    json!({"op": "createChat", "chatId": task, "spaceId": space, "cwd": path}),
                )
                .await
                .unwrap();
            core.workspace.rename_chat(task, title).unwrap();
            core.workspace
                .set_chat_activity(task, Some(now - minutes * 60_000), None)
                .unwrap();
            if task != IOS02_SELECT {
                core.workspace
                    .mark_chat_seen(task, chrono::Utc::now())
                    .unwrap();
            }
        }
        assert!(
            core.workspace
                .set_chat_archived(IOS02_ARCHIVED, true)
                .unwrap()
        );
        let read: luna_proto::TaskCommandRead = local
            .call_as(
                methods::GET_TASK_COMMAND_SNAPSHOT,
                json!({"taskId": IOS02_PINNED}),
            )
            .await
            .unwrap();
        let mut envelope = json!({
            "commandId": "ios02-seed-pin", "actorId": "ios02-seed", "clientId": "ios02-seed",
            "clientSessionId": "ios02-seed", "taskId": IOS02_PINNED,
            "intent": {"kind": "setPinned", "pinned": true},
            "expectedTaskRevision": read.snapshot.task_revision,
        });
        if let Some(generation) = read.snapshot.binding_generation {
            envelope["expectedBindingGeneration"] = generation.into();
        }
        let receipt = local
            .call(methods::EXECUTE_TASK_COMMAND, envelope)
            .await
            .unwrap();
        assert_eq!(receipt["status"], "applied", "{receipt}");
        let handle = core.doc_host.open(IOS02_SELECT).unwrap();
        handle
            .write_user_message("ios02-select-1", "First IOS02 prompt", now)
            .unwrap();
        handle
            .write_user_message("ios02-select-2", "Second IOS02 prompt", now + 1)
            .unwrap();
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
        /// The open socket's id and outbound frames; RM-03 opens one after another.
        socket: std::sync::Mutex<Option<(u64, tokio::sync::mpsc::UnboundedSender<Message>)>>,
        sockets: std::sync::atomic::AtomicUsize,
        closed: tokio::sync::watch::Sender<bool>,
        /// RM-04: no network at all, as a phone out of coverage: every
        /// request and socket fails at once.
        offline: Arc<std::sync::atomic::AtomicBool>,
        /// RM-04: the next reply to this method is lost with its socket
        /// (and, with `true`, the phone then goes offline).
        lose_reply_to: std::sync::Mutex<Option<(String, bool)>>,
        lost: Arc<std::sync::Mutex<std::collections::HashSet<u64>>>,
        /// RM-04: the command id of every `ExecuteRemoteTaskCommand` sent.
        commands_sent: std::sync::Mutex<Vec<String>>,
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
                offline: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                lose_reply_to: std::sync::Mutex::new(None),
                lost: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
                commands_sent: std::sync::Mutex::new(Vec::new()),
            })
        }

        /// RM-04: lose the next reply to `method` together with its socket,
        /// as a phone that walks out of coverage mid-call; `then_offline`
        /// keeps it out of coverage afterwards.
        fn lose_next_reply(&self, method: &str, then_offline: bool) {
            *self.lose_reply_to.lock().unwrap() = Some((method.into(), then_offline));
        }

        fn set_offline(&self, offline: bool) {
            self.offline
                .store(offline, std::sync::atomic::Ordering::SeqCst);
        }

        /// RM-04: how many times this transport sent `command_id`.
        fn sent(&self, command_id: &str) -> usize {
            self.commands_sent
                .lock()
                .unwrap()
                .iter()
                .filter(|sent| *sent == command_id)
                .count()
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
            if self.offline.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::spawn(async move {
                    if let Some(session) = session.upgrade() {
                        session.request_failed(
                            request.request_id,
                            luna_mobile::probe::TransportFailure::Network {
                                detail: "This phone is offline".into(),
                            },
                        );
                    }
                });
                return;
            }
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
            // A new socket replaces (and so closes) any previous one.
            *self.socket.lock().unwrap() = Some((request.socket_id, frames));
            let tls = tls(&self.pin);
            let offline = self.offline.clone();
            let lost = self.lost.clone();
            tokio::spawn(async move {
                if offline.load(std::sync::atomic::Ordering::SeqCst) {
                    if let Some(session) = session.upgrade() {
                        session.socket_closed(
                            request.socket_id,
                            luna_mobile::probe::TransportFailure::Network {
                                detail: "This phone is offline".into(),
                            },
                        );
                    }
                    return;
                }
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
                            // RM-04: a reply chosen to be lost ends its socket
                            // undelivered, as a phone leaving coverage does.
                            Some(Ok(Message::Text(text)))
                                if serde_json::from_str::<Value>(&text)
                                    .ok()
                                    .and_then(|frame| frame["id"].as_u64())
                                    .is_some_and(|id| lost.lock().unwrap().remove(&id)) =>
                            {
                                break
                            }
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

        fn send_frame(&self, socket_id: u64, frame: String) {
            if let Ok(value) = serde_json::from_str::<Value>(&frame)
                && let Some(method) = value["method"].as_str()
            {
                if method == methods::EXECUTE_REMOTE_TASK_COMMAND
                    && let Some(command_id) = value["params"]["commandId"].as_str()
                {
                    self.commands_sent.lock().unwrap().push(command_id.into());
                }
                let mut lose = self.lose_reply_to.lock().unwrap();
                if lose.as_ref().is_some_and(|(lost, _)| lost == method)
                    && let Some(id) = value["id"].as_u64()
                {
                    let (_, then_offline) = lose.take().unwrap();
                    self.lost.lock().unwrap().insert(id);
                    if then_offline {
                        self.set_offline(true);
                    }
                }
            }
            if let Some((id, frames)) = &*self.socket.lock().unwrap()
                && *id == socket_id
            {
                let _ = frames.send(Message::Text(frame));
            }
        }

        fn close_socket(&self, socket_id: u64) {
            let mut socket = self.socket.lock().unwrap();
            if socket.as_ref().is_some_and(|(id, _)| *id == socket_id) {
                socket.take();
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

    /// RM-04: a gateway pairing with `scopes`.
    async fn pair_scoped(
        local: &luna_rpc::RpcClient,
        http: &reqwest::Client,
        bind: &str,
        label: &str,
        scopes: &[&str],
    ) -> IssuedSession {
        let issued: IssuedPairing = local
            .call_as(
                methods::CREATE_REMOTE_PAIRING,
                json!({"label": label, "scopes": scopes,
                    "pairingTtlSecs": 300, "sessionTtlSecs": 3600}),
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

    fn outbox_entry(
        session: &luna_mobile::session::RemoteSession,
        command_id: &str,
    ) -> luna_mobile::session::commands::OutboxEntryRecord {
        session
            .outbox()
            .into_iter()
            .find(|entry| entry.command_id == command_id)
            .unwrap_or_else(|| panic!("command {command_id} is in the outbox"))
    }

    /// Waits on the session's change notifications (never a timer) until
    /// the outbox entry satisfies `done`.
    async fn outboxed(
        session: &luna_mobile::session::RemoteSession,
        changes: &Changes,
        command_id: &str,
        done: impl Fn(&luna_mobile::session::commands::OutboxEntryRecord) -> bool,
    ) -> luna_mobile::session::commands::OutboxEntryRecord {
        let mut seen = changes.0.subscribe();
        let waited = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let _ = session.state();
                if let Some(entry) = session
                    .outbox()
                    .into_iter()
                    .find(|entry| entry.command_id == command_id)
                    && done(&entry)
                {
                    return entry;
                }
                seen.changed()
                    .await
                    .expect("the listener outlives the wait");
            }
        })
        .await;
        waited.unwrap_or_else(|_| {
            panic!(
                "command {command_id} did not reach the expected state; state {:?}, issue {:?}, outbox {:?}",
                session.state(),
                session.outbox_issue(),
                session.outbox()
            )
        })
    }

    fn user_entries(task: &luna_mobile::follow::RemoteTaskProjection) -> Vec<String> {
        task.entries
            .iter()
            .filter(|entry| entry.role == "user")
            .map(|entry| entry.entry_id.clone())
            .collect()
    }

    /// RM-04 F9, F16–F21 (RP-10, RP-15, RP-20), the client half: the mobile
    /// runtime's outbox over the real gateway, TLS pin and remote guard, with
    /// the provider boundary recorded. A message composed for a task the Mac
    /// has not created yet stays a draft, through a phone restart, until the
    /// create is applied; a live control is refused without a connection; a
    /// reply lost with its socket is read back by id on the next socket and
    /// never resent, also across a phone restart while out of coverage; a
    /// queued message's delivery is followed while no view is open; the
    /// host's transcript shows each message once, in order, after a
    /// resnapshot; a live stop ends the running turn; and a message accepted
    /// before a revocation still runs after it, while the revoked runtime
    /// ends and refuses new commands. Failure contract:
    /// docs/pending-tasks/control-this-mac/rm-04/README.md.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mobile_outbox_delivers_commands_across_lost_replies_suspension_restart_and_revocation()
    {
        use luna_mobile::follow::ProjectionFreshness;
        use luna_mobile::session::commands::{OutboxDeliveryState, OutboxEntryStatus};
        use luna_mobile::session::{
            RemoteSession, RemoteSessionError, RemoteSessionState, SessionCloseReason,
        };

        let root = tempfile::tempdir().unwrap();
        let provider = super::remote_provider::Provider::new();
        let core = assemble_with(root.path(), provider.registry(true));
        let local = luna_rpc::memory_client(core.rpc_service());
        let bind = free_bind();
        let status = enable(&local, &bind).await;
        if firewalled(&bind).await {
            shutdown(core).await;
            return;
        }
        let pin = status["spkiSha256"].as_str().unwrap().to_owned();
        let http = http(&pin);
        let project = root.path().join("rm04-project");
        std::fs::create_dir_all(&project).unwrap();
        let project = project.canonicalize().unwrap();
        core.workspace
            .create_space(
                "rm04-space",
                &core.device_id,
                project.to_str().unwrap(),
                Some("RM04 project".into()),
                false,
            )
            .unwrap();
        let issued = pair_scoped(
            &local,
            &http,
            &bind,
            "rm04",
            &["environment-read", "tasks-read", "tasks-operate"],
        )
        .await;
        let mac = saved_mac(&issued, &bind, &pin, "RM04 QA Mac");
        let changes = Arc::new(Changes(tokio::sync::watch::Sender::new(0)));
        let transport = PlatformStandIn::new(&pin, &bind);
        let session = RemoteSession::following(
            mac.clone(),
            0,
            transport.clone(),
            Some(changes.clone()),
            None,
        )
        .unwrap();
        transport.attach(&session);
        session.connect(None).await.unwrap();
        assert_eq!(session.outbox_issue(), None);

        // F17: composed while away, for a task the Mac has not created yet,
        // the first message stays a draft behind its create.
        session.suspend();
        let task = session.create_task("rm04-space".into(), None).unwrap();
        let first = session
            .send_message(
                task.clone(),
                "rm04: drafted before create".into(),
                false,
                false,
                None,
                None,
            )
            .await
            .unwrap();
        let create = session.outbox()[0].clone();
        assert_eq!(
            (create.kind.as_str(), create.status),
            ("createTask", OutboxEntryStatus::Ready)
        );
        let draft = outbox_entry(&session, &first);
        assert_eq!(draft.status, OutboxEntryStatus::Draft);
        assert_eq!(
            draft.blocked_by.as_deref(),
            Some(create.command_id.as_str())
        );
        // F20: a live control needs a live connection; nothing is queued.
        assert!(matches!(
            session.stop_task(task.clone(), None).await,
            Err(RemoteSessionError::NotOpen)
        ));
        assert_eq!(session.outbox().len(), 2);

        // F18: the outbox survives a phone restart; another session's copy
        // of it is refused.
        let saved = session.outbox_bytes();
        drop(session);
        transport.closed().await;
        let mut stranger = mac.clone();
        stranger.session_id = "rm04-another-session".into();
        let other =
            RemoteSession::following(stranger, 0, PlatformStandIn::new(&pin, &bind), None, None)
                .unwrap();
        assert!(other.restore_outbox(saved.clone()).is_some());
        drop(other);
        let transport = PlatformStandIn::new(&pin, &bind);
        let session = RemoteSession::following(
            mac.clone(),
            0,
            transport.clone(),
            Some(changes.clone()),
            None,
        )
        .unwrap();
        transport.attach(&session);
        assert_eq!(session.restore_outbox(saved), None);
        assert_eq!(
            outbox_entry(&session, &first).status,
            OutboxEntryStatus::Draft
        );
        session.connect(None).await.unwrap();
        outboxed(&session, &changes, &create.command_id, |entry| {
            entry.status == OutboxEntryStatus::Applied
        })
        .await;
        let started = outboxed(&session, &changes, &first, |entry| {
            entry.status == OutboxEntryStatus::Applied
        })
        .await;
        assert_eq!(started.receipt_status.as_deref(), Some("applied"));
        provider.ran("rm04: drafted before create").await;
        assert!(core.workspace.chat(&task).unwrap().is_some());

        // F9 (RP-10): the reply is lost with its socket. The next socket
        // reads the receipt back by id; the command is never resent.
        transport.lose_next_reply(methods::EXECUTE_REMOTE_TASK_COMMAND, false);
        let lost = session
            .send_message(
                task.clone(),
                "rm04: reply lost".into(),
                false,
                false,
                None,
                None,
            )
            .await
            .unwrap();
        outboxed(&session, &changes, &lost, |entry| {
            entry.status == OutboxEntryStatus::Applied
        })
        .await;
        assert_eq!(transport.sent(&lost), 1, "read back by id, never resent");

        // F19: its delivery is followed with no view open: queued behind the
        // running turn, delivered when that turn ends.
        outboxed(&session, &changes, &lost, |entry| {
            entry.delivery == Some(OutboxDeliveryState::Queued)
        })
        .await;
        provider.finish("rm04: drafted before create");
        provider.ran("rm04: reply lost").await;
        outboxed(&session, &changes, &lost, |entry| {
            entry.delivery == Some(OutboxDeliveryState::Delivered)
        })
        .await;

        // F18: in flight when the phone leaves coverage and restarts. It
        // comes back uncertain and is read back by id, never resent.
        transport.lose_next_reply(methods::EXECUTE_REMOTE_TASK_COMMAND, true);
        let at_restart = session
            .send_message(
                task.clone(),
                "rm04: in flight at restart".into(),
                false,
                false,
                None,
                None,
            )
            .await
            .unwrap();
        outboxed(&session, &changes, &at_restart, |entry| {
            entry.status == OutboxEntryStatus::Uncertain
        })
        .await;
        let saved = session.outbox_bytes();
        drop(session);
        transport.closed().await;
        let transport = PlatformStandIn::new(&pin, &bind);
        let session = RemoteSession::following(
            mac.clone(),
            0,
            transport.clone(),
            Some(changes.clone()),
            None,
        )
        .unwrap();
        transport.attach(&session);
        assert_eq!(session.restore_outbox(saved), None);
        assert_eq!(
            outbox_entry(&session, &at_restart).status,
            OutboxEntryStatus::Uncertain
        );
        session.connect(None).await.unwrap();
        outboxed(&session, &changes, &at_restart, |entry| {
            entry.status == OutboxEntryStatus::Applied
        })
        .await;
        assert_eq!(transport.sent(&at_restart), 0, "read back, never resent");

        // F21 (RP-20): the Mac's transcript holds each accepted message once,
        // in order, and converges to the same after a resnapshot.
        session.follow_task(task.clone()).unwrap();
        provider.finish("rm04: reply lost");
        provider.ran("rm04: in flight at restart").await;
        let order = vec![first.clone(), lost.clone(), at_restart.clone()];
        followed(&session, &changes, &task, |detail| {
            detail.freshness == ProjectionFreshness::Live && user_entries(detail) == order
        })
        .await;
        session.suspend();
        session.resume();
        followed(&session, &changes, &task, |detail| {
            detail.freshness == ProjectionFreshness::Live && user_entries(detail) == order
        })
        .await;

        // A live stop ends the running turn.
        let stop = session.stop_task(task.clone(), None).await.unwrap();
        outboxed(&session, &changes, &stop, |entry| {
            matches!(
                entry.status,
                OutboxEntryStatus::Accepted | OutboxEntryStatus::Applied
            )
        })
        .await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while core.sessions.active_turn_id(&task).is_some() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the stop ended the running turn");

        // F16 (RP-15): a message accepted before a revocation still runs
        // after it; the revoked runtime ends and refuses new commands.
        let held = session
            .send_message(
                task.clone(),
                "rm04: held at revoke".into(),
                false,
                false,
                None,
                None,
            )
            .await
            .unwrap();
        provider.ran("rm04: held at revoke").await;
        outboxed(&session, &changes, &held, |entry| {
            entry.status == OutboxEntryStatus::Applied
        })
        .await;
        let behind = session
            .send_message(
                task.clone(),
                "rm04: accepted before revoke".into(),
                false,
                false,
                None,
                None,
            )
            .await
            .unwrap();
        outboxed(&session, &changes, &behind, |entry| {
            entry.status == OutboxEntryStatus::Applied
        })
        .await;
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
                    reason: SessionCloseReason::RePair | SessionCloseReason::Remote,
                    ..
                }
            ),
            "{state:?}"
        );
        assert!(matches!(
            session
                .send_message(
                    task.clone(),
                    "rm04: after revoke".into(),
                    false,
                    false,
                    None,
                    None
                )
                .await,
            Err(RemoteSessionError::Closed { .. })
        ));
        provider.finish("rm04: held at revoke");
        provider.ran("rm04: accepted before revoke").await;
        for prompt in [
            "rm04: drafted before create",
            "rm04: reply lost",
            "rm04: in flight at restart",
            "rm04: held at revoke",
            "rm04: accepted before revoke",
        ] {
            assert_eq!(
                provider.runs_of(prompt).len(),
                1,
                "one provider run of {prompt}"
            );
        }
        assert!(provider.runs_of("rm04: after revoke").is_empty());
        provider.finish(super::remote_provider::ALL);
        shutdown(core).await;
    }
}
