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
    ) -> IssuedSession {
        let issued: IssuedPairing = local
            .call_as(
                methods::CREATE_REMOTE_PAIRING,
                json!({"label": label, "scopes": ["environment-read", "tasks-read"],
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

    /// The macOS application firewall (stealth mode) silently drops inbound
    /// connections to a test binary it has not approved, even from this
    /// machine; such a run is reported as skipped, and Linux CI runs these
    /// tests for real. A refused connection (nothing listening) still fails.
    async fn firewalled(bind: &str) -> bool {
        match tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(bind))
            .await
        {
            Ok(Ok(_)) => false,
            Ok(Err(error)) => panic!("the gateway is not listening on {bind}: {error}"),
            Err(_) => {
                eprintln!(
                    "SKIPPED: {bind} is unreachable from this unapproved test binary \
                     (macOS application firewall); Linux CI runs this test"
                );
                true
            }
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
        let (status, session) = post(&http, &bind, "/remote/v1/pair", None, &redeem).await;
        assert_eq!(status, 200, "{session}");
        let session: IssuedSession = serde_json::from_value(session).unwrap();
        let (replayed, _) = post(&http, &bind, "/remote/v1/pair", None, &redeem).await;
        assert_eq!(replayed, 403, "a used credential is refused");
        let oversized = json!({"padding": "x".repeat(20_000)});
        let (too_large, _) = post(&http, &bind, "/remote/v1/pair", None, &oversized).await;
        assert_eq!(too_large, 413);

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
        for (id, method) in [
            (2, methods::LIST_REMOTE_SESSIONS),
            (3, methods::EXECUTE_TASK_COMMAND),
        ] {
            let denied = call(&mut ws, id, method, json!({})).await.unwrap_err();
            assert!(denied.contains("local only"), "{method}: {denied}");
        }
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
        let a = pair(&local, &http, &bind, "phone a").await;
        let b = pair(&local, &http, &bind, "phone b").await;
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

        let status = local
            .call("SetRemoteAccess", json!({"enabled": false}))
            .await
            .unwrap();
        assert_eq!(
            (status["enabled"].clone(), status["listening"].clone()),
            (json!(false), json!(false))
        );
        assert!(closed(&mut wb).await, "disable closes every socket");
        assert!(
            tokio::net::TcpStream::connect(&bind).await.is_err(),
            "the listener is gone"
        );

        let again = enable(&local, &bind).await;
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
    /// a live session still checks in and a revoked one stays refused.
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
        let live = pair(&local, &http, &bind, "kept").await;
        let gone = pair(&local, &http, &bind, "revoked").await;
        local
            .call(
                methods::REVOKE_REMOTE_SESSION,
                json!({"id": gone.session.id}),
            )
            .await
            .unwrap();
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
        for (session, expected) in [(&live, 200), (&gone, 403)] {
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
}
