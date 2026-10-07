//! Session creation and turn submission: prompt and command bodies.

use super::*;

/// `POST /api/session`: the run directory rides the BODY
/// (`location.directory`; the header is ignored on this route), the LUNA
/// permission mode rides `permissions` as a v2 ruleset. The create schema
/// is strict (`additionalProperties: false`), so any other key is a 400.
/// Not retried: a blind retry after a 5xx could mint a duplicate session.
pub(super) async fn create_session(
    server: &Server,
    dir: Option<&str>,
    permission_rules: &Value,
) -> Result<String, HarnessError> {
    let body = session_create_body(dir, permission_rules);
    let created = server.post_json("/api/session", dir, &body).await?;
    created
        .pointer("/data/id")
        .or_else(|| created.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| HarnessError::Protocol("opencode session create returned no id".into()))
}

pub(super) fn session_create_body(dir: Option<&str>, permission_rules: &Value) -> Value {
    match dir {
        Some(dir) => json!({
            "location": { "directory": dir },
            "permissions": permission_rules,
        }),
        None => json!({ "permissions": permission_rules }),
    }
}

/// Attachments as v2 `files` entries (`{uri, name}`), one per distinct
/// path: the server reads each `file:` URI itself when it admits the turn.
pub(super) fn attachment_files(attachments: &[String]) -> Vec<Value> {
    unique(attachments)
        .map(|path| {
            json!({
                "uri": file_uri(path),
                "name": std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// `file://` URL for an absolute host path. The server parses it with the
/// WHATWG URL parser and `fileURLToPath`, so `#`, `?`, `%` and spaces in a
/// path must be percent-encoded or the server reads another file.
pub(super) fn file_uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// 2.x prompt body: text plus `{uri, name}` file attachments and typed
/// `skills[{id}]` (only when selected). Natively sent images leave LUNA's
/// path trailer (OpenCode labels each file itself). Model and variant do NOT
/// ride here — they are set on the session (`POST /api/session/{id}/model`).
pub(super) fn prompt_body_v2(prompt: &str, attachments: &[String], skills: &[Value]) -> Value {
    let mut body = json!({
        "text": native_prompt_text(prompt, attachments),
        "files": attachment_files(attachments),
    });
    if !skills.is_empty() {
        body["skills"] = Value::Array(skills.to_vec());
    }
    body
}

/// 2.x command body: `{name, text, files?, skills?}` (`session.command`).
pub(super) fn command_body(
    name: &str,
    text: &str,
    attachments: &[String],
    skills: &[Value],
) -> Value {
    let mut body = json!({ "name": name, "text": native_prompt_text(text, attachments) });
    if !attachments.is_empty() {
        body["files"] = Value::Array(attachment_files(attachments));
    }
    if !skills.is_empty() {
        body["skills"] = Value::Array(skills.to_vec());
    }
    body
}

/// Re-check every image right before the server is told to read it (the
/// file may have changed since admission) and log what is sent: the same
/// type, size and hash must reach the provider on every route.
fn verify_files(attachments: &[String]) -> Result<(), HarnessError> {
    for path in unique(attachments) {
        let facts = check_image(path)?;
        tracing::info!(
            target: "luna_harness::opencode",
            "files[] {path} {} {}B sha256:{}",
            facts.media_type,
            facts.bytes,
            facts.sha256
        );
    }
    Ok(())
}

/// What a posted turn carries besides its text (model and variant are set
/// on the session, not the prompt): staged images and typed selections.
pub(super) struct TurnSpec<'a> {
    pub(super) attachments: &'a [String],
    pub(super) input_items: &'a [luna_proto::ComposerInput],
    /// The session's model (`provider/model`) for the image-capability
    /// check; `None` when the run did not choose one.
    pub(super) model: Option<&'a str>,
}

/// Send a turn:a leading `/command` known to the agent routes through the
/// command endpoint (the server does NOT parse slash text out of an
/// ordinary prompt); everything else is a prompt. The command endpoint is
/// synchronous on the wire (204 only after the command's callback, which can
/// take the whole turn), so it rides a detached task; its failure reaches
/// the run loop as [`BusMsg::CommandFailed`] instead of a silent stall.
pub(super) async fn post_prompt(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
    commands: &[SlashCommand],
    prompt: &str,
    spec: TurnSpec<'_>,
    bus: &mpsc::Sender<BusMsg>,
) -> Result<(), HarnessError> {
    let TurnSpec {
        attachments,
        input_items,
        model,
    } = spec;
    // Nothing is posted for a turn whose images or skills cannot be sent:
    // OpenCode would strip images for a text-only model without a word.
    if !attachments.is_empty()
        && let Some(model) = model
        && model_accepts_images(model) == Some(false)
    {
        return Err(text_only_model(model));
    }
    verify_files(attachments)?;
    let skills = server.skill_refs(dir, input_items).await?;
    if let Some(rest) = prompt.strip_prefix('/') {
        let mut split = rest.splitn(2, char::is_whitespace);
        let name = split.next().unwrap_or_default();
        let arguments = split.next().unwrap_or_default().trim().to_owned();
        if !name.is_empty() && commands.iter().any(|c| c.name == name) {
            let path = format!("/api/session/{session_id}/command");
            let body = command_body(name, &arguments, attachments, &skills);
            let command = name.to_owned();
            let server = server.detached();
            let dir_owned = dir.map(str::to_owned);
            let bus = bus.clone();
            tokio::spawn(async move {
                // Deliberately no CALL_TIMEOUT: the response can take the
                // whole turn; the bus carries the turn's events meanwhile.
                let failure = match server
                    .exchange(
                        reqwest::Method::POST,
                        &path,
                        dir_owned.as_deref(),
                        Some(&body),
                        None,
                    )
                    .await
                {
                    Ok((status, _)) if status.is_success() => None,
                    Ok((status, text)) => Some(http_error_message("POST", &path, status, &text)),
                    Err(error) => Some(error.to_string()),
                };
                if let Some(detail) = failure {
                    let _ = bus
                        .send(BusMsg::CommandFailed(format!(
                            "OpenCode could not run /{command}: {detail}"
                        )))
                        .await;
                }
            });
            return Ok(());
        }
    }
    // Prompts carry text, `{uri, name}` files and typed skills;
    // model/variant were set on the session at run start.
    let body = prompt_body_v2(prompt, attachments, &skills);
    let path = format!("/api/session/{session_id}/prompt");
    server.post_json(&path, dir, &body).await.map(|_| ())
}
