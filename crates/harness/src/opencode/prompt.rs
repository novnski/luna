//! Session creation and turn submission: prompt and command bodies.

use super::*;

pub(super) async fn create_session(
    server: &Server,
    dir: Option<&str>,
    permission_rules: &Value,
) -> Result<String, HarnessError> {
    if server.protocol().await == Protocol::V2 {
        // 2.x takes the run directory in the BODY (`location.directory`) —
        // the header is ignored on this route (observed live, 2.0.3) — and
        // wraps the answer in `{data}`. Its schema is stable; the 1.x-only
        // lazy-migration crash below doesn't exist there.
        let body = match dir {
            Some(dir) => json!({
                "location": { "directory": dir },
                "permission": permission_rules,
            }),
            None => json!({ "permission": permission_rules }),
        };
        let created = server.post_json("/api/session", dir, &body).await?;
        return created
            .pointer("/data/id")
            .or_else(|| created.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                HarnessError::Protocol("opencode session create returned no id".into())
            });
    }
    // opencode 1.18.x lazily migrates a directory's legacy rows on its
    // FIRST directory-scoped request (`Project.migrateProjectId`). On a db
    // whose schema a newer 2.x install has rewritten, that migration throws
    // mid-request — observed live: `SQLiteError: no such column:
    // project_id` → `500 UnknownError` on `POST /session` — but the project
    // row it inserted commits BEFORE the throw, so the identical request
    // retried immediately succeeds. Retry once on any 5xx: this failure
    // class then costs one round-trip instead of a dead turn.
    for attempt in 0..2 {
        let body = json!({ "permission": permission_rules });
        let (status, text) = server.post_json_raw("/session", dir, &body).await?;
        if status.is_success() {
            let created = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
            return created
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    HarnessError::Protocol("opencode session create returned no id".into())
                });
        }
        if attempt == 0 && status.is_server_error() {
            tracing::debug!(
                target: "luna_harness::opencode",
                "POST /session answered {status}; retrying once (the lazy-migration crash self-heals)"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        return Err(HarnessError::Protocol(post_error_message(
            "/session", status, &text,
        )));
    }
    unreachable!("create_session retry loop returns from every path")
}

/// Build a `prompt_async` body: text part + attachment file parts.
pub(super) fn prompt_body(
    prompt: &str,
    model: Option<(&str, &str)>,
    variant: Option<&str>,
    attachments: &[String],
) -> Value {
    let mut parts = vec![json!({ "type": "text", "text": prompt })];
    for path in attachments {
        parts.push(json!({
            "type": "file",
            "mime": mime_for(path),
            "filename": std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            "url": format!("file://{path}"),
        }));
    }
    let mut body = serde_json::Map::new();
    body.insert("parts".into(), Value::Array(parts));
    if let Some((provider, model)) = model {
        body.insert(
            "model".into(),
            json!({ "providerID": provider, "modelID": model }),
        );
    }
    if let Some(variant) = variant {
        body.insert("variant".into(), Value::String(variant.to_owned()));
    }
    Value::Object(body)
}

/// 2.x prompt body: plain text plus `{uri, name}` file attachments. Model
/// and variant do NOT ride here — they were set on the session at run
/// start (`POST /api/session/{id}/model`).
pub(super) fn prompt_body_v2(prompt: &str, attachments: &[String]) -> Value {
    let files: Vec<Value> = attachments
        .iter()
        .map(|path| {
            json!({
                "uri": format!("file://{path}"),
                "name": std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
        })
        .collect();
    json!({ "text": prompt, "files": files })
}

pub(super) fn mime_for(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// What a posted turn carries besides its text (1.x folds model/variant
/// into the prompt body; 2.x ignores them there — set on the session).
pub(super) struct TurnSpec<'a> {
    pub(super) model: Option<&'a (String, String)>,
    pub(super) variant: Option<&'a str>,
    pub(super) attachments: &'a [String],
}

/// Send a turn: a leading `/command` known to the agent routes through the
/// command endpoint (the desktop parity — the server does NOT parse slash
/// text out of an ordinary prompt); everything else is a prompt.
/// Both are fire-and-forget for the loop: the command endpoint is
/// synchronous on the wire, so it rides a detached task and the bus
/// delivers the actual turn.
pub(super) async fn post_prompt(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
    commands: &[SlashCommand],
    prompt: &str,
    spec: TurnSpec<'_>,
) -> Result<(), HarnessError> {
    let TurnSpec {
        model,
        variant,
        attachments,
    } = spec;
    let protocol = server.protocol().await;
    if let Some(rest) = prompt.strip_prefix('/') {
        let mut split = rest.splitn(2, char::is_whitespace);
        let name = split.next().unwrap_or_default();
        let arguments = split.next().unwrap_or_default().trim().to_owned();
        if !name.is_empty() && commands.iter().any(|c| c.name == name) {
            // 1.x names the args `arguments`; 2.x `text`.
            let (path, cmd_body) = match protocol {
                Protocol::V1 => (
                    format!("/session/{session_id}/command"),
                    json!({ "command": name, "arguments": arguments }),
                ),
                Protocol::V2 => (
                    format!("/api/session/{session_id}/command"),
                    json!({ "command": name, "text": arguments }),
                ),
            };
            let server_base = server.base.clone();
            let auth = server.auth.clone();
            let dir_owned = dir.map(str::to_owned);
            let path_owned = path.clone();
            let protocol = server.protocol.clone();
            tokio::spawn(async move {
                let server = Server {
                    child: None,
                    base: server_base,
                    auth,
                    client: http_client(),
                    stderr_tail: crate::StderrTail::default(),
                    protocol,
                };
                // The command endpoint blocks for the whole turn; the bus
                // carries the real events, so this response is ignored —
                // but it must not be cut off mid-turn by CALL_TIMEOUT.
                let mut req = server
                    .request(reqwest::Method::POST, &path_owned)
                    .json(&cmd_body);
                req = server.scoped(req, dir_owned.as_deref()).await;
                if let Err(e) = req.send().await {
                    tracing::debug!(
                        target: "luna_harness::opencode",
                        "command turn failed: {e}"
                    );
                }
            });
            return Ok(());
        }
    }
    match protocol {
        Protocol::V1 => {
            let body = prompt_body(
                prompt,
                model.map(|(provider, model)| (provider.as_str(), model.as_str())),
                variant,
                attachments,
            );
            let path = format!("/session/{session_id}/prompt_async");
            server.post_json(&path, dir, &body).await.map(|_| ())
        }
        Protocol::V2 => {
            // 2.x prompts carry text + `{uri, name}` files; model/variant
            // were set on the session at run start.
            let body = prompt_body_v2(prompt, attachments);
            let path = format!("/api/session/{session_id}/prompt");
            server.post_json(&path, dir, &body).await.map(|_| ())
        }
    }
}
