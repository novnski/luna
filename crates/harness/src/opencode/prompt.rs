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

/// Attachments as v2 `files` entries (`{uri, name}`).
pub(super) fn attachment_files(attachments: &[String]) -> Vec<Value> {
    attachments
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
        .collect()
}

/// 2.x prompt body: plain text plus `{uri, name}` file attachments. Model
/// and variant do NOT ride here — they were set on the session at run
/// start (`POST /api/session/{id}/model`).
pub(super) fn prompt_body_v2(prompt: &str, attachments: &[String]) -> Value {
    json!({ "text": prompt, "files": attachment_files(attachments) })
}

/// 2.x command body: `{name, text, files?}` (`session.command`).
pub(super) fn command_body(name: &str, text: &str, attachments: &[String]) -> Value {
    let mut body = json!({ "name": name, "text": text });
    if !attachments.is_empty() {
        body["files"] = Value::Array(attachment_files(attachments));
    }
    body
}

/// What a posted turn carries besides its text (model and variant are set
/// on the session, not the prompt).
pub(super) struct TurnSpec<'a> {
    pub(super) attachments: &'a [String],
}

/// Send a turn: a leading `/command` known to the agent routes through the
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
    let TurnSpec { attachments } = spec;
    if let Some(rest) = prompt.strip_prefix('/') {
        let mut split = rest.splitn(2, char::is_whitespace);
        let name = split.next().unwrap_or_default();
        let arguments = split.next().unwrap_or_default().trim().to_owned();
        // Skills share the composer catalog but are not commands.
        if !name.is_empty()
            && commands
                .iter()
                .any(|c| c.name == name && c.kind.is_command())
        {
            let path = format!("/api/session/{session_id}/command");
            let body = command_body(name, &arguments, attachments);
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
    // Prompts carry text + `{uri, name}` files; model/variant were set on
    // the session at run start.
    let body = prompt_body_v2(prompt, attachments);
    let path = format!("/api/session/{session_id}/prompt");
    server.post_json(&path, dir, &body).await.map(|_| ())
}
