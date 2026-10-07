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

/// How OpenCode admits a prompt into its inbox: `queue` waits for the turn
/// boundary, `steer` is incorporated at the next step boundary of the
/// running execution (PD-2 native steering).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PromptDelivery {
    Queue,
    Steer,
}

impl PromptDelivery {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Steer => "steer",
        }
    }
}

/// The caller id a LUNA message rides as in `session_id` (`msg_…`, required
/// by the v2 schema). The same LUNA message in the same session always maps
/// to the same OpenCode id, so a resend is reconciled by OpenCode instead of
/// duplicated, and the inbox id (equal to this id) correlates delivery back
/// to the LUNA message. OpenCode message ids are unique across ALL sessions
/// (a reuse elsewhere is a 409 conflict, observed live on 2.0.22), so the
/// id carries a short digest of the session. A message without a LUNA id
/// gets a fresh one.
pub(super) fn luna_prompt_id(session_id: &str, luna_id: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let Some(luna_id) = luna_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return format!("msg_luna_{}", uuid::Uuid::new_v4().simple());
    };
    let session = format!("{:x}", Sha256::digest(session_id.as_bytes()));
    let safe: String = luna_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(96)
        .collect();
    format!("msg_luna_{}_{safe}", &session[..10])
}

/// 2.x prompt body: the caller id, plain text, `{uri, name}` file
/// attachments and an explicit delivery. Model and variant do NOT ride
/// here — they were set on the session at run start
/// (`POST /api/session/{id}/model`).
pub(super) fn prompt_body_v2(
    prompt: &str,
    attachments: &[String],
    id: &str,
    delivery: PromptDelivery,
) -> Value {
    json!({
        "id": id,
        "text": prompt,
        "files": attachment_files(attachments),
        "delivery": delivery.as_str(),
    })
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
    /// The caller id (`msg_…`, see [`luna_prompt_id`]).
    pub(super) id: &'a str,
    pub(super) delivery: PromptDelivery,
}

/// What OpenCode said about a posted turn (the message-queue contract's
/// accepted / refused / unknown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PromptOutcome {
    /// A slash command went to the command endpoint. It takes no caller
    /// id; its failure reaches the loop as [`BusMsg::CommandFailed`].
    Command,
    /// Durably admitted (HTTP 200 with the inbox item). Not proof of
    /// incorporation: `session.inbox.delivered` is.
    Accepted {
        inbox_id: String,
        /// The item's admission time; much older than the POST means
        /// OpenCode reconciled an earlier admission of the same id.
        created_ms: Option<i64>,
    },
    /// Refused before admission (a 4xx tagged error): nothing was recorded.
    Rejected(String),
    /// The request may have reached OpenCode (transport failure, timeout,
    /// 5xx): never resent; readback decides.
    Unknown(String),
}

/// The slash command a turn's text invokes, if OpenCode knows it.
pub(super) fn known_command<'a>(prompt: &'a str, commands: &[SlashCommand]) -> Option<&'a str> {
    let rest = prompt.strip_prefix('/')?;
    let name = rest.split(char::is_whitespace).next().unwrap_or_default();
    (!name.is_empty() && commands.iter().any(|c| c.name == name)).then_some(name)
}

/// Send a turn: a leading `/command` known to the agent routes through the
/// command endpoint (the server does NOT parse slash text out of an
/// ordinary prompt); everything else is a prompt with a caller id and an
/// explicit delivery. The command endpoint is synchronous on the wire (204
/// only after the command's callback, which can take the whole turn), so it
/// rides a detached task; its failure reaches the run loop as
/// [`BusMsg::CommandFailed`] instead of a silent stall.
pub(super) async fn post_prompt(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
    commands: &[SlashCommand],
    prompt: &str,
    spec: TurnSpec<'_>,
    bus: &mpsc::Sender<BusMsg>,
) -> PromptOutcome {
    let TurnSpec {
        attachments,
        id,
        delivery,
    } = spec;
    if let Some(rest) = prompt.strip_prefix('/') {
        let mut split = rest.splitn(2, char::is_whitespace);
        let name = split.next().unwrap_or_default();
        let arguments = split.next().unwrap_or_default().trim().to_owned();
        if known_command(prompt, commands).is_some() {
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
            return PromptOutcome::Command;
        }
    }
    // Prompts carry the caller id, text, `{uri, name}` files and the
    // delivery; model/variant were set on the session at run start.
    let body = prompt_body_v2(prompt, attachments, id, delivery);
    let path = format!("/api/session/{session_id}/prompt");
    classify_prompt_response(&path, id, server.post_json_raw(&path, dir, &body).await)
}

/// Map a prompt POST onto accepted / refused / unknown. A tagged 4xx is a
/// refusal (the request was validated and rejected before admission); a
/// 5xx or a transport failure may follow a durable admission, so it is
/// unknown and must never be replayed.
pub(super) fn classify_prompt_response(
    path: &str,
    id: &str,
    response: Result<(reqwest::StatusCode, String), HarnessError>,
) -> PromptOutcome {
    match response {
        Ok((status, text)) if status.is_success() => {
            let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let item = unwrap_data(value);
            PromptOutcome::Accepted {
                inbox_id: item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_owned(),
                created_ms: item.pointer("/time/created").and_then(Value::as_i64),
            }
        }
        Ok((status, text)) if status.is_client_error() => {
            PromptOutcome::Rejected(post_error_message(path, status, &text))
        }
        Ok((status, text)) => PromptOutcome::Unknown(post_error_message(path, status, &text)),
        Err(error) => PromptOutcome::Unknown(error.to_string()),
    }
}
