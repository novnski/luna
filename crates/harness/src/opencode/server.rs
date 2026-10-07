//! The `opencode serve` child process and its HTTP plumbing: spawn,
//! readiness, auth, request helpers, error text and shutdown.
//!
//! OpenCode 2 only (PD-15): readiness and generation come from the
//! authenticated `GET /api/info` (a JSON `version` that parses as 2.x; 200 =
//! ready, 503 = still booting, 500 = failed). The 1.x `/global/*` surface and
//! the removed 2.0.3-era `/api/health` are not probed.

use super::*;

/// opencode loads plugins and MCP config before the server answers; cold
/// plugin-heavy starts can take minutes. Shared by chat startup and model
/// discovery (same boot either way).
pub(super) const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(300);
pub(super) const STARTUP_TIMEOUT_ENV: &str = "LUNA_OPENCODE_STARTUP_TIMEOUT_SECS";

/// Readiness-poll cadence while the server boots.
pub(super) const HEALTH_POLL: Duration = Duration::from_millis(150);

/// Bound on the pre-SIGTERM interrupt of still-running executions.
pub(super) const SHUTDOWN_INTERRUPT_BUDGET: Duration = Duration::from_secs(3);

/// Bound on ordinary (non-SSE) HTTP calls: everything is loopback and the
/// only slow route is a cold model catalog. The synchronous per-turn
/// command endpoint deliberately bypasses this (its response can take the
/// whole turn).
pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn startup_timeout() -> Duration {
    std::env::var(STARTUP_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_STARTUP_TIMEOUT)
}

/// The `--stdio` readiness line: `{"url":"http://127.0.0.1:<port>"}`. Only
/// a loopback http url is accepted (LUNA never talks to anything else).
pub(super) fn ready_url(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    let url = value.get("url")?.as_str()?.trim_end_matches('/');
    let rest = url.strip_prefix("http://")?;
    let host = rest.split('/').next()?;
    let loopback = ["127.0.0.1:", "localhost:", "[::1]:"]
        .iter()
        .any(|prefix| host.starts_with(prefix));
    loopback.then(|| url.to_owned())
}

pub(super) struct Server {
    pub(super) child: Option<Child>,
    /// The child's stdin, held open as the `--stdio` ownership lease: the
    /// server exits when it reaches EOF (also when LUNA dies).
    pub(super) lease: Option<ChildStdin>,
    pub(super) base: String,
    /// `Authorization` header value (`Basic <b64>`), when we own the process.
    pub(super) auth: Option<String>,
    pub(super) client: reqwest::Client,
    pub(super) stderr_tail: crate::StderrTail,
    /// The server's `/api/info` version once readiness proved it (spawned
    /// servers only; an attached test server is assumed to speak 2.x).
    pub(super) version: Option<String>,
}

/// One `GET /api/info` readiness observation.
pub(super) enum InfoProbe {
    /// 200 with a 2.x version.
    Ready(String),
    /// Not answering yet, or 503 while the app graph starts.
    Booting,
    /// The server answered that it cannot start (500), or it is not an
    /// OpenCode 2 server. Final: waiting longer will not help.
    Failed(String),
}

impl Server {
    pub(super) fn attached(base: String) -> Self {
        Self {
            child: None,
            lease: None,
            base: base.trim_end_matches('/').to_owned(),
            auth: None,
            client: http_client(),
            stderr_tail: crate::StderrTail::default(),
            version: None,
        }
    }

    /// A non-owning handle on the same server for a detached task (replies,
    /// command turns): same base and auth, never shuts the process down.
    pub(super) fn detached(&self) -> Self {
        Self {
            child: None,
            lease: None,
            base: self.base.clone(),
            auth: self.auth.clone(),
            client: self.client.clone(),
            stderr_tail: crate::StderrTail::default(),
            version: self.version.clone(),
        }
    }

    /// Spawn `opencode serve --stdio --port 0`, the official private-server
    /// launch (`cli/src/services/standalone.ts`):
    /// - the per-run Basic password rides `OPENCODE_PASSWORD` (plus the
    ///   legacy `OPENCODE_SERVER_PASSWORD`); in `--stdio` mode the server
    ///   deletes both from its environment, so agent tool processes cannot
    ///   read it and self-approve over the API.
    /// - stdin is piped and held as the ownership lease: the server exits on
    ///   EOF, so it dies with LUNA even when LUNA cannot clean up.
    /// - the OS picks the port; the server prints `{"url": ...}` on stdout
    ///   once boot completes — readiness without a port race or polling.
    /// - `GET /api/info` then proves a ready 2.x server (503 = keep waiting).
    pub(super) async fn spawn(
        exe: &std::path::Path,
        cwd: Option<&str>,
        startup: Duration,
    ) -> Result<Self, HarnessError> {
        let password = uuid::Uuid::new_v4().to_string();
        let mut cmd = Command::new(exe);
        cmd.arg("serve")
            .arg("--stdio")
            .arg("--port")
            .arg("0")
            .env("OPENCODE_PASSWORD", &password)
            .env("OPENCODE_SERVER_PASSWORD", &password)
            .env("OPENCODE_CLIENT", "luna")
            // Read by OpenCode builds that gate the embedded web UI (set by
            // the desktop app too); a no-op where unsupported (2.0.22).
            .env("OPENCODE_DISABLE_EMBEDDED_WEB_UI", "1");
        crate::compose_child_path(&mut cmd, exe);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(exe.display().to_string())
            } else {
                HarnessError::Io(e)
            }
        })?;
        let lease = child.stdin.take();
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
        // stdout: the one `{"url"}` readiness line, then drained for the
        // process lifetime so a chatty build can never block on a full pipe.
        let (url_tx, mut url_rx) = tokio::sync::oneshot::channel::<String>();
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(async move {
                let mut url_tx = Some(url_tx);
                let mut lines = tokio::io::BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(url) = ready_url(&line)
                        && let Some(tx) = url_tx.take()
                    {
                        let _ = tx.send(url);
                        continue;
                    }
                    tracing::debug!(target: "luna_harness::opencode", "stdout: {line}");
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
            lease,
            base: String::new(),
            auth: Some(auth),
            client: http_client(),
            stderr_tail,
            version: None,
        };

        // Readiness, bounded by one budget: the url line (minutes on
        // plugin-heavy cold starts), then a 2.x `/api/info`. An exited child
        // fails fast with its stderr tail instead of burning the budget.
        let deadline = tokio::time::Instant::now() + startup;
        let timed_out = |server_phase: &str| {
            HarnessError::Protocol(format!(
                "opencode serve did not {server_phase} within {}s (raise {} if this \
                 machine's plugin load is genuinely slow)",
                startup.as_secs(),
                STARTUP_TIMEOUT_ENV,
            ))
        };
        let mut stdout_closed = false;
        loop {
            if let Some(status) = server.exited() {
                return Err(server.crashed(status));
            }
            if stdout_closed {
                // No url will come; the exit check above reports why on the
                // next pass (or the deadline does).
                tokio::time::sleep(HEALTH_POLL).await;
            } else {
                match tokio::time::timeout(HEALTH_POLL, &mut url_rx).await {
                    Ok(Ok(url)) => {
                        server.base = url;
                        break;
                    }
                    Ok(Err(_)) => stdout_closed = true,
                    Err(_) => {}
                }
            }
            if tokio::time::Instant::now() >= deadline {
                server.shutdown(Duration::from_secs(1)).await;
                return Err(timed_out("print its listening url"));
            }
        }
        loop {
            if let Some(status) = server.exited() {
                return Err(server.crashed(status));
            }
            match server.probe_info().await {
                InfoProbe::Ready(version) => {
                    server.version = Some(version);
                    break;
                }
                InfoProbe::Failed(message) => {
                    server.shutdown(Duration::from_secs(1)).await;
                    return Err(HarnessError::Protocol(message));
                }
                InfoProbe::Booting => {}
            }
            if tokio::time::Instant::now() >= deadline {
                server.shutdown(Duration::from_secs(1)).await;
                return Err(timed_out("become ready"));
            }
            tokio::time::sleep(HEALTH_POLL).await;
        }
        // The API contract LUNA depends on, before anything is sent.
        if let Err(error) = ensure_compatible(&server).await {
            server.shutdown(Duration::from_secs(1)).await;
            return Err(error);
        }
        Ok(server)
    }

    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.as_mut()?.try_wait().ok().flatten()
    }

    fn crashed(&self, status: std::process::ExitStatus) -> HarnessError {
        HarnessError::Protocol(crate::crash_message(
            "opencode serve",
            Some(status),
            &self.stderr_tail,
        ))
    }

    /// One bounded `GET /api/info`. OpenCode 2 answers it with
    /// `{version, pid, urls, paths}` even before its app graph is ready: the
    /// STATUS is the readiness (200 ready, 503 booting, 500 failed) and the
    /// version is the generation. Anything without a JSON version — the 1.x
    /// web UI, an empty 404 — is not an OpenCode 2 server.
    pub(super) async fn probe_info(&self) -> InfoProbe {
        let Ok(resp) = self.get_raw("/api/info").await else {
            return InfoProbe::Booting;
        };
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let version = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("version").and_then(Value::as_str).map(str::to_owned));
        match version {
            Some(version) if resolve::version_major(&version) == Some(resolve::REQUIRED_MAJOR) => {
                if status.is_success() {
                    InfoProbe::Ready(version)
                } else if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                    InfoProbe::Booting
                } else {
                    InfoProbe::Failed(format!(
                        "opencode serve {version} failed to start: {status} {}",
                        truncate_body(&body)
                    ))
                }
            }
            Some(version) => InfoProbe::Failed(format!(
                "OpenCode 2 is required, but the server reports version {version}"
            )),
            None if status == reqwest::StatusCode::UNAUTHORIZED => InfoProbe::Failed(
                "opencode serve rejected LUNA's server credentials (401 on /api/info)".into(),
            ),
            // Not listening yet answers as a transport error above; a server
            // that answers without a version is the wrong generation.
            None if status.is_success() || status == reqwest::StatusCode::NOT_FOUND => {
                InfoProbe::Failed(format!(
                    "not an OpenCode 2 server: /api/info answered {status} without a version \
                     (OpenCode 1.x is no longer supported; install: curl -fsSL \
                     https://opencode.ai/install | bash)"
                ))
            }
            None => InfoProbe::Booting,
        }
    }

    pub(super) fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut req = self.client.request(method, format!("{}{path}", self.base));
        if let Some(auth) = &self.auth {
            req = req.header(reqwest::header::AUTHORIZATION, auth.clone());
        }
        req
    }

    /// Bounded readiness probe: the boot window may accept a connection and
    /// park the request, so a probe abandons after 2 s and re-polls.
    pub(super) async fn get_raw(&self, path: &str) -> Result<reqwest::Response, reqwest::Error> {
        self.request(reqwest::Method::GET, path)
            .timeout(Duration::from_secs(2))
            .send()
            .await
    }

    /// GET with the session's directory scope (the server's per-request
    /// location selector, the `x-opencode-directory` header).
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
            .send()
            .await
            .map_err(|e| HarnessError::Protocol(format!("opencode GET {path}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(HarnessError::Protocol(http_error_message(
                "GET", path, status, &body,
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

    /// POST with NO request body (`session.interrupt` takes none).
    pub(super) async fn post_empty(
        &self,
        path: &str,
        directory: Option<&str>,
    ) -> Result<Value, HarnessError> {
        let req = self
            .request(reqwest::Method::POST, path)
            .timeout(CALL_TIMEOUT);
        let resp = self
            .scoped(req, directory)
            .send()
            .await
            .map_err(|e| HarnessError::Protocol(format!("opencode POST {path}: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
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
        let req = self
            .request(reqwest::Method::PATCH, path)
            .timeout(CALL_TIMEOUT)
            .json(body);
        let response =
            self.scoped(req, directory).send().await.map_err(|error| {
                HarnessError::Protocol(format!("opencode PATCH {path}: {error}"))
            })?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(HarnessError::Protocol(http_error_message(
                "PATCH", path, status, &text,
            )));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// POST returning `(status, raw body)`; only transport failures error.
    /// HTTP status handling is the caller's.
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
            .send()
            .await
            .map_err(|e| HarnessError::Protocol(format!("opencode POST {path}: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, text))
    }

    /// Location scope: the server's per-request location selector. The 2.x
    /// query validation rejects unknown params, so the header alone scopes
    /// the request (percent-encoded, as the official SDK sends it).
    pub(super) fn scoped(
        &self,
        req: reqwest::RequestBuilder,
        directory: Option<&str>,
    ) -> reqwest::RequestBuilder {
        match directory {
            Some(dir) => req.header("x-opencode-directory", encode_directory(dir)),
            None => req,
        }
    }

    /// Tear down an owned server. SIGTERM and stdin EOF both end a running
    /// execution as `interrupted{reason: "shutdown"}` and KEEP its restart
    /// claim, which the user's managed OpenCode service later resumes
    /// unobserved. So first interrupt whatever still executes here (bounded;
    /// a user interrupt releases the claim), then release the stdin lease,
    /// then SIGTERM → SIGKILL after `kill_grace`.
    pub(super) async fn shutdown(&mut self, kill_grace: Duration) {
        let running = self
            .child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)));
        if running && !self.base.is_empty() {
            let _ = tokio::time::timeout(SHUTDOWN_INTERRUPT_BUDGET, self.interrupt_active()).await;
        }
        drop(self.lease.take());
        if let Some(child) = self.child.as_mut() {
            shutdown_child(child, kill_grace).await;
        }
    }

    /// Interrupt every execution this server process owns
    /// (`GET /api/session/active` is process-local).
    async fn interrupt_active(&self) {
        let Ok(active) = self.get_json("/api/session/active", None).await else {
            return;
        };
        let active = unwrap_data(active);
        let Some(sessions) = active.as_object() else {
            return;
        };
        for session_id in sessions.keys() {
            match self.interrupt_session(session_id, None).await {
                Ok(interrupted) => tracing::debug!(
                    target: "luna_harness::opencode",
                    "interrupted {session_id} before shutdown (interrupted={interrupted})"
                ),
                Err(error) => tracing::debug!(
                    target: "luna_harness::opencode",
                    "could not interrupt {session_id} before shutdown: {error}"
                ),
            }
        }
    }

    /// Session lookup for resume (`GET /api/session/{id}` → `{data}`).
    pub(super) async fn session_info(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<Value, HarnessError> {
        let info = self
            .get_json(&format!("/api/session/{session_id}"), directory)
            .await?;
        Ok(unwrap_data(info))
    }

    /// Is the session mid-turn? `GET /api/session/active` lists the running
    /// sessions this server process owns. `Err` = can't tell — the caller
    /// treats that as still running.
    pub(super) async fn session_running(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<bool, HarnessError> {
        let map = unwrap_data(self.get_json("/api/session/active", directory).await?);
        Ok(map.get(session_id).is_some())
    }

    /// Interrupt the session's active execution (`POST …/interrupt`, no
    /// body). `Ok(false)` = nothing to interrupt: the session was idle, or
    /// another process owns the execution — no terminal frame will follow.
    pub(super) async fn interrupt_session(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<bool, HarnessError> {
        let answer = self
            .post_empty(&format!("/api/session/{session_id}/interrupt"), directory)
            .await?;
        Ok(unwrap_data(answer)
            .get("interrupted")
            .and_then(Value::as_bool)
            .unwrap_or(true))
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

/// Diagnostics clip: long enough for a tagged error envelope with its
/// message, bounded so a stray HTML page can't flood a chip.
const BODY_CLIP: usize = 1000;

pub(super) fn truncate_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.len() <= BODY_CLIP {
        trimmed.to_owned()
    } else {
        let mut end = BODY_CLIP;
        while end > 0 && !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &trimmed[..end])
    }
}

/// The human part of a 2.x tagged error envelope
/// (`{_tag, message, kind?, field?}`), e.g. `CommandNotFoundError: Command
/// not found: nope`. `None` when the body isn't such an envelope.
pub(super) fn describe_error_envelope(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body.trim()).ok()?;
    let tag = value
        .get("_tag")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str);
    let message = value
        .get("message")
        .or_else(|| value.pointer("/data/message"))
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty());
    let mut text = match (tag, message) {
        (Some(tag), Some(message)) => format!("{tag}: {message}"),
        (Some(tag), None) => tag.to_owned(),
        (None, Some(message)) => message.to_owned(),
        (None, None) => return None,
    };
    let detail: Vec<String> = ["kind", "field"]
        .into_iter()
        .filter_map(|key| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(|v| format!("{key} {v}"))
        })
        .collect();
    if !detail.is_empty() {
        text.push_str(&format!(" ({})", detail.join(", ")));
    }
    Some(truncate_body(&text))
}

/// Failure text for any HTTP call: the decoded error envelope when there is
/// one, else the clipped body.
pub(super) fn http_error_message(
    method: &str,
    path: &str,
    status: reqwest::StatusCode,
    body: &str,
) -> String {
    let detail = describe_error_envelope(body).unwrap_or_else(|| truncate_body(body));
    let base = format!("opencode {method} {path}: {status} {detail}");
    if status.is_server_error() {
        // OpenCode's own 5xx bodies say only "Check server logs for
        // details."; point at the log that holds the cause.
        format!(
            "{base} (server-side fault; the body's ref keys the cause in \
             ~/.local/share/opencode/log/opencode.log)"
        )
    } else {
        base
    }
}

/// POST failure text (see [`http_error_message`]).
pub(super) fn post_error_message(path: &str, status: reqwest::StatusCode, body: &str) -> String {
    http_error_message("POST", path, status, body)
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

/// 2.x wraps most responses in `{data}`; unwrap when present.
pub(super) fn unwrap_data(value: Value) -> Value {
    value
        .get("data")
        .cloned()
        .filter(|d| d.is_object() || d.is_array())
        .unwrap_or(value)
}
