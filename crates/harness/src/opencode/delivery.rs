//! Delivery truth for posted turns and native steers (OC-07): receipt
//! evidence, delivery readback, inbox cancellation, retry and finish-reason
//! notices.
//!
//! The prompt inbox is receipt evidence only; LUNA's durable queue stays
//! the one queue UI (PD-14). The message-queue contract's states map as:
//! - accepted: `POST …/prompt` 200 (durable admission) and
//!   `session.inbox.enqueued{inboxID}`;
//! - recorded: `session.inbox.delivered{inboxID}` (the native user message
//!   exists; not proof that a model request sampled it);
//! - unknown: a non-refusal POST failure whose readback could not decide;
//! - cancelled: `session.inbox.cancelled{inboxID}` (no Steered follows).

use super::*;

/// What readback says about one inbox id (= the caller's `msg_…` id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeliveryReadback {
    /// Admitted and still waiting in the inbox.
    Pending,
    /// A native message with that id exists: it was delivered.
    Delivered,
    /// Neither pending nor recorded: never admitted, or cancelled.
    Absent,
    /// The readback itself failed.
    Unknown(String),
}

/// Read the inbox (`GET …/inbox`), then the message (`GET …/message/{id}`).
pub(super) async fn check_delivery(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
    inbox_id: &str,
) -> DeliveryReadback {
    match server
        .get_json(&format!("/api/session/{session_id}/inbox"), dir)
        .await
    {
        Ok(inbox) => {
            let pending = unwrap_data(inbox).as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.get("id").and_then(Value::as_str) == Some(inbox_id))
            });
            if pending {
                return DeliveryReadback::Pending;
            }
        }
        Err(error) => return DeliveryReadback::Unknown(error.to_string()),
    }
    let path = format!("/api/session/{session_id}/message/{inbox_id}");
    match server
        .exchange(reqwest::Method::GET, &path, dir, None, Some(CALL_TIMEOUT))
        .await
    {
        // Delivered only when the answer IS that message: a 200 without it
        // proves nothing.
        Ok((status, text)) if status.is_success() => {
            let message = serde_json::from_str::<Value>(&text)
                .map(unwrap_data)
                .unwrap_or(Value::Null);
            if message.get("id").and_then(Value::as_str) == Some(inbox_id) {
                DeliveryReadback::Delivered
            } else {
                DeliveryReadback::Unknown(format!("GET {path} answered without the message"))
            }
        }
        Ok((status, _)) if status == reqwest::StatusCode::NOT_FOUND => DeliveryReadback::Absent,
        Ok((status, text)) => {
            DeliveryReadback::Unknown(http_error_message("GET", &path, status, &text))
        }
        Err(error) => DeliveryReadback::Unknown(error.to_string()),
    }
}

/// Withdraw an undelivered inbox item (`DELETE …/inbox/{id}`). The 204 is
/// returned for an already delivered or unknown id too, so only
/// `session.inbox.cancelled` proves the cancellation.
pub(super) async fn cancel_inbox(
    server: &Server,
    session_id: &str,
    dir: Option<&str>,
    inbox_id: &str,
) -> Result<(), String> {
    let path = format!("/api/session/{session_id}/inbox/{inbox_id}");
    match server
        .exchange(
            reqwest::Method::DELETE,
            &path,
            dir,
            None,
            Some(Duration::from_secs(5)),
        )
        .await
    {
        Ok((status, _)) if status.is_success() => Ok(()),
        Ok((status, text)) => Err(http_error_message("DELETE", &path, status, &text)),
        Err(error) => Err(error.to_string()),
    }
}

/// A native steer LUNA posted and has not seen delivered.
#[derive(Debug, Clone)]
pub(super) struct PendingSteer {
    /// The caller id the steer rode as (= its inbox id).
    pub(super) inbox_id: String,
    /// LUNA's message id: the exact routed-steer correlation for Steered.
    pub(super) luna_id: Option<String>,
    /// HTTP 200 seen (accepted); `false` while posting or after an
    /// unknown outcome.
    pub(super) accepted: bool,
}

/// The posting task's verdict on one steer, back to the run loop.
pub(super) struct SteerPosted {
    pub(super) inbox_id: String,
    pub(super) outcome: PromptOutcome,
}

/// Host-only receipt evidence (journaled, never transcript content).
pub(super) fn receipt_evidence(kind: &str, native_id: Option<&str>, payload: Value) -> AgentEvent {
    AgentEvent::ProviderEvidence {
        provider: HarnessId::Opencode,
        kind: format!("native:{kind}"),
        native_id: native_id.map(str::to_owned),
        payload,
    }
}

pub(super) fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// `session.retry.scheduled` → a transient Retrying notice (like Codex's
/// `error{willRetry}`), never an error part.
pub(super) fn retry_notice(props: &Value) -> AgentEvent {
    let attempt = props.get("attempt").and_then(Value::as_u64).unwrap_or(0);
    let message = props
        .get("message")
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .unwrap_or("provider error");
    let wait = props
        .get("at")
        .and_then(Value::as_i64)
        .map(|at| (at - unix_ms()).max(0) / 1000);
    let text = match wait {
        Some(seconds) => {
            format!("OpenCode is retrying (attempt {attempt}, next in {seconds}s): {message}")
        }
        None => format!("OpenCode is retrying (attempt {attempt}): {message}"),
    };
    let mut bounded: String = text.chars().take(500).collect();
    if bounded.len() < text.len() {
        bounded.push('…');
    }
    AgentEvent::ProviderNotice {
        notice: luna_proto::ProviderNotice {
            kind: luna_proto::ProviderNoticeKind::Retrying,
            message: bounded,
            at_ms: unix_ms(),
        },
    }
}

/// `session.step.ended` with finish `length` / `content-filter`: a visible
/// row, because the reply is cut off or withheld. The turn status is
/// unchanged.
pub(super) fn finish_notice(props: &Value) -> Option<AgentEvent> {
    let message = match props.get("finish").and_then(Value::as_str)? {
        "length" => {
            "The model stopped at its output limit (finish: length); the reply may be incomplete."
        }
        "content-filter" => {
            "The provider's content filter stopped the reply (finish: content-filter)."
        }
        _ => return None,
    };
    Some(AgentEvent::Error {
        message: message.to_owned(),
    })
}

/// A provider-side interruption that was not a user Stop, in words.
pub(super) fn interrupt_reason_message(reason: &str) -> Option<String> {
    let text = match reason {
        "user" | "" => return None,
        "shutdown" => "the OpenCode server shut down",
        "superseded" => "newer work superseded it",
        "inactivity" => "it was inactive for too long",
        "cancelled" => "the prompt was withdrawn from its inbox before delivery",
        "lost" => {
            "the execution ended without a recorded outcome while LUNA's event stream \
             was reconnecting (server restart or crash)"
        }
        "unknown" => "it was interrupted while LUNA's event stream was reconnecting",
        other => other,
    };
    Some(format!(
        "OpenCode interrupted the turn: {text} (reason: {reason})."
    ))
}
