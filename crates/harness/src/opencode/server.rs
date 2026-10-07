//! The `opencode serve` child process and its HTTP plumbing: spawn,
//! readiness, auth, request helpers, error text and shutdown.

use super::*;

/// opencode loads plugins and MCP config before the server answers; cold
/// plugin-heavy starts can take minutes. Shared by chat startup and model
/// discovery (same boot either way).
pub(super) const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(300);
pub(super) const STARTUP_TIMEOUT_ENV: &str = "LUNA_OPENCODE_STARTUP_TIMEOUT_SECS";

/// Health-poll cadence while the server boots.
pub(super) const HEALTH_POLL: Duration = Duration::from_millis(150);

/// Bound on ordinary (non-SSE) HTTP calls: everything is loopback and the
/// only slow route is a cold /provider catalog. The synchronous per-turn
/// command endpoint deliberately bypasses this (its response can take the
/// whole turn and is ignored anyway).
pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn startup_timeout() -> Duration {
    std::env::var(STARTUP_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_STARTUP_TIMEOUT)
}

/// A kernel-assigned free localhost port, released for the child to claim.
pub(super) fn free_localhost_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .ok()
}

pub(super) struct Server {
    pub(super) child: Option<Child>,
    pub(super) base: String,
    /// `Authorization` header value (`Basic <b64>`), when we own the process.
    pub(super) auth: Option<String>,
    pub(super) client: reqwest::Client,
    pub(super) stderr_tail: crate::StderrTail,
    /// Wire generation, resolved once via the health endpoints.
    pub(super) protocol: tokio::sync::OnceCell<Protocol>,
}

/// The attached server's wire generation: the 1.x "v1" global namespace
/// (`/global/*`, `/session/*`, prompt_async) and the 2.x `/api/*` surface
/// (session-scoped model, prompt with delivery, interrupt).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Protocol {
    V1,
    V2,
}

impl Protocol {
    /// One readiness poll across both generations. 2.x answers
    /// `GET /api/health` with `{healthy, version}` and serves its web UI on
    /// `/global/health`; 1.x answers `GET /global/health` with
    /// `{healthy, version}` AND also serves `/api/health` — with
    /// `{"healthy":true}`, no version (both observed live). The version
    /// field is the only unambiguous discriminator. `None` = still booting.
    pub(super) async fn detect(server: &Server) -> Option<Self> {
        for (path, protocol) in [
            ("/api/health", Protocol::V2),
            ("/global/health", Protocol::V1),
        ] {
            if let Ok(resp) = server.get_raw(path).await
                && resp.status().is_success()
                && let Ok(v) = resp.json::<Value>().await
                && v.get("version").and_then(Value::as_str).is_some()
            {
                return Some(protocol);
            }
        }
        None
    }
}

impl Server {
    pub(super) fn attached(base: String) -> Self {
        Self {
            child: None,
            base: base.trim_end_matches('/').to_owned(),
            auth: None,
            client: http_client(),
            stderr_tail: crate::StderrTail::default(),
            protocol: tokio::sync::OnceCell::new(),
        }
    }

    /// The resolved wire generation: detected against the live server on
    /// first use (spawn seeds the cell during the readiness loop).
    pub(super) async fn protocol(&self) -> Protocol {
        *self
            .protocol
            .get_or_init(|| async { Protocol::detect(self).await.unwrap_or(Protocol::V1) })
            .await
    }

    /// Spawn `opencode serve` on a free loopback port with a per-run Basic
    /// password, and wait for a generation-bearing health answer (which
    /// also resolves [`Protocol`]).
    pub(super) async fn spawn(
        exe: &std::path::Path,
        cwd: Option<&str>,
        startup: Duration,
    ) -> Result<Self, HarnessError> {
        let port = free_localhost_port().ok_or_else(|| {
            HarnessError::Protocol("no free localhost port for opencode serve".into())
        })?;
        let password = uuid::Uuid::new_v4().to_string();
        let mut cmd = Command::new(exe);
        cmd.arg("serve")
            .arg("--port")
            .arg(port.to_string())
            .arg("--hostname")
            .arg("127.0.0.1")
            .env("OPENCODE_SERVER_PASSWORD", &password)
            .env("OPENCODE_CLIENT", "luna");
        crate::compose_child_path(&mut cmd, exe);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(exe.display().to_string())
            } else {
                HarnessError::Io(e)
            }
        })?;
        let stderr_tail = crate::StderrTail::default();
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "luna_harness::opencode", "stderr: {line}");
                    tail.push(&line);
                }
            });
        }

        use base64::Engine as _;
        let auth = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))
        );
        let mut server = Self {
            child: Some(child),
            base: format!("http://127.0.0.1:{port}"),
            auth: Some(auth),
            client: http_client(),
            stderr_tail,
            protocol: tokio::sync::OnceCell::new(),
        };

        // Readiness: the server binds a few seconds into the process's life
        // (minutes on plugin-heavy cold starts). An exited child fails fast
        // with its stderr tail instead of burning the whole budget.
        let deadline = tokio::time::Instant::now() + startup;
        loop {
            if let Some(child) = server.child.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                return Err(HarnessError::Protocol(crate::crash_message(
                    "opencode serve",
                    Some(status),
                    &server.stderr_tail,
                )));
            }
            if let Some(protocol) = Protocol::detect(&server).await {
                let _ = server.protocol.set(protocol);
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                server.shutdown(Duration::from_secs(1)).await;
                return Err(HarnessError::Protocol(format!(
                    "opencode serve did not become healthy within {}s (raise {} if this \
                     machine's plugin load is genuinely slow)",
                    startup.as_secs(),
                    STARTUP_TIMEOUT_ENV,
                )));
            }
            tokio::time::sleep(HEALTH_POLL).await;
        }
        Ok(server)
    }

    pub(super) fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut req = self.client.request(method, format!("{}{path}", self.base));
        if let Some(auth) = &self.auth {
            req = req.header(reqwest::header::AUTHORIZATION, auth.clone());
        }
        req
    }

    /// Bounded health probe. opencode's boot window ACCEPTS connections but
    /// parks the request until the app is ready — and a request parked early
    /// enough is never answered at all (observed live, 1.18.18), so an
    /// unbounded send() wedges the whole startup. Abandon and re-poll.
    pub(super) async fn get_raw(&self, path: &str) -> Result<reqwest::Response, reqwest::Error> {
        self.request(reqwest::Method::GET, path)
            .timeout(Duration::from_secs(2))
            .send()
            .await
    }

    /// GET with the session's directory scope (the server's per-request
    /// instance selector; both carriers set, matching the official SDK).
    pub(super) async fn get_json(
        &self,
        path: &str,
        directory: Option<&str>,
    ) -> Result<Value, HarnessError> {
        self.get_response(path, directory)
            .await?
            .json()
            .await
            .map_err(|e| HarnessError::Protocol(format!("opencode GET {path}: {e}")))
    }

    pub(super) async fn get<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        path: &str,
        directory: Option<&str>,
    ) -> Result<T, HarnessError> {
        let response = self.get_response(path, directory).await?;
        decode_json_response(response)
            .await
            .map_err(|e| HarnessError::Protocol(format!("opencode GET {path}: {e}")))
    }

    pub(super) async fn get_response(
        &self,
        path: &str,
        directory: Option<&str>,
    ) -> Result<reqwest::Response, HarnessError> {
        let req = self
            .request(reqwest::Method::GET, path)
            .timeout(CALL_TIMEOUT);
        let resp = self
            .scoped(req, directory)
            .await
            .send()
            .await
            .map_err(|e| HarnessError::Protocol(format!("opencode GET {path}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(HarnessError::Protocol(format!(
                "opencode GET {path}: {status} {}",
                truncate_body(&body)
            )));
        }
        Ok(resp)
    }

    pub(super) async fn post_json(
        &self,
        path: &str,
        directory: Option<&str>,
        body: &Value,
    ) -> Result<Value, HarnessError> {
        let (status, text) = self.post_json_raw(path, directory, body).await?;
        if !status.is_success() {
            return Err(HarnessError::Protocol(post_error_message(
                path, status, &text,
            )));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub(super) async fn patch_json(
        &self,
        path: &str,
        directory: Option<&str>,
        body: &Value,
    ) -> Result<Value, HarnessError> {
        let mut request = self
            .client
            .patch(format!("{}{}", self.base, path))
            .json(body);
        if let Some(directory) = directory {
            request = request.header("x-opencode-directory", directory);
        }
        if let Some(auth) = &self.auth {
            request = request.header("authorization", auth);
        }
        let response = request
            .send()
            .await
            .map_err(|error| HarnessError::Protocol(format!("opencode PATCH {path}: {error}")))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(HarnessError::Protocol(post_error_message(
                path, status, &text,
            )));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// POST returning `(status, raw body)`; only transport failures error.
    /// HTTP status handling is the caller's (the lazy-migration retry in
    /// [`create_session`] needs to see 5xx, not a preformatted message).
    pub(super) async fn post_json_raw(
        &self,
        path: &str,
        directory: Option<&str>,
        body: &Value,
    ) -> Result<(reqwest::StatusCode, String), HarnessError> {
        let req = self
            .request(reqwest::Method::POST, path)
            .timeout(CALL_TIMEOUT)
            .json(body);
        let resp = self
            .scoped(req, directory)
            .await
            .send()
            .await
            .map_err(|e| HarnessError::Protocol(format!("opencode POST {path}: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, text))
    }

    /// Directory scope: the server's per-request instance selector. V1 takes
    /// it as a query param (plus header, matching the official SDK); V2's
    /// strict query validation rejects unknown params — there the header
    /// alone scopes the request (verified live, 2.0.3).
    pub(super) async fn scoped(
        &self,
        mut req: reqwest::RequestBuilder,
        directory: Option<&str>,
    ) -> reqwest::RequestBuilder {
        if let Some(dir) = directory {
            req = req.header("x-opencode-directory", encode_directory(dir));
            if self.protocol().await == Protocol::V1 {
                req = req.query(&[("directory", dir)]);
            }
        }
        req
    }

    pub(super) async fn shutdown(&mut self, kill_grace: Duration) {
        if let Some(child) = self.child.as_mut() {
            shutdown_child(child, kill_grace).await;
        }
    }

    /// Session lookup for resume; both wires answer with the info object
    /// (2.x wrapped in `{data}`).
    pub(super) async fn session_info(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<Value, HarnessError> {
        let path = match self.protocol().await {
            Protocol::V1 => format!("/session/{session_id}"),
            Protocol::V2 => format!("/api/session/{session_id}"),
        };
        let info = self.get_json(&path, directory).await?;
        Ok(unwrap_data(info))
    }

    /// Is the session mid-turn? V1 keeps a status map on `/session/status`;
    /// V2 lists running sessions on `/api/session/active`. `Err` = can't
    /// tell — the caller treats that as still running.
    pub(super) async fn session_running(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<bool, HarnessError> {
        let path = match self.protocol().await {
            Protocol::V1 => "/session/status",
            Protocol::V2 => "/api/session/active",
        };
        let map = unwrap_data(self.get_json(path, directory).await?);
        Ok(map.get(session_id).is_some())
    }

    /// End the live turn.
    pub(super) async fn abort_session(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<Value, HarnessError> {
        let path = match self.protocol().await {
            Protocol::V1 => format!("/session/{session_id}/abort"),
            Protocol::V2 => format!("/api/session/{session_id}/interrupt"),
        };
        self.post_json(&path, directory, &Value::Null).await
    }
}

pub(super) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        // No global timeout: the SSE stream lives for the whole run and
        // sync calls are bounded per call site.
        .build()
        .unwrap_or_default()
}

/// The official SDK sends `x-opencode-directory` percent-encoded.
pub(super) fn encode_directory(dir: &str) -> String {
    let mut out = String::with_capacity(dir.len());
    for b in dir.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(super) fn truncate_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.len() <= 300 {
        trimmed.to_owned()
    } else {
        let mut end = 300;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &trimmed[..end])
    }
}

/// POST failure text; 5xx adds where the body's `ref` resolves — opencode's
/// own error bodies say only "Check server logs for details.", which sends
/// users to us instead of the log that holds the actual cause.
pub(super) fn post_error_message(path: &str, status: reqwest::StatusCode, body: &str) -> String {
    let base = format!("opencode POST {path}: {status} {}", truncate_body(body));
    if status.is_server_error() {
        format!(
            "{base} (server-side fault; the body's ref keys the cause in \
             ~/.local/share/opencode/log/opencode.log)"
        )
    } else {
        base
    }
}

/// Decode large catalogs without collecting a second, full HTTP body first.
/// The blocking parser reads through a 64KiB buffer; network reads stay on
/// Tokio. Dropping the caller also interrupts an outstanding body read.
pub(super) async fn decode_json_response<T: serde::de::DeserializeOwned + Send + 'static>(
    response: reqwest::Response,
) -> Result<T, String> {
    let cancel = tokio_util::sync::CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let stream = response
        .bytes_stream()
        .map(|chunk| chunk.map_err(std::io::Error::other))
        .take_until(cancel.cancelled_owned());
    let reader = tokio_util::io::StreamReader::new(Box::pin(stream));
    let reader = tokio_util::io::SyncIoBridge::new(reader);
    tokio::task::spawn_blocking(move || {
        serde_json::from_reader(std::io::BufReader::with_capacity(64 * 1024, reader))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Both wires wrap responses in `{data}` on 2.x only; unwrap when present.
pub(super) fn unwrap_data(value: Value) -> Value {
    value
        .get("data")
        .cloned()
        .filter(|d| d.is_object() || d.is_array())
        .unwrap_or(value)
}
