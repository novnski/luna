//! Native cross-session (peer) observations as attributed transcript entries.
//!
//! A peer message is another session's model-authored text. It is never the
//! user speaking and never LUNA authority: no queue row, command, approval or
//! task-control effect follows from it. The entry is a System-role record
//! with the sender's self-asserted labels kept distinct from the
//! kernel-verified connecting process (when the provider supplied one), and
//! with the delivery state stated plainly. IDs are stable per (message, state)
//! so re-announcements, reconnects and replays never duplicate an entry.

use luna_doc::{
    DocError, MessagePart, MessageRole, MessageStatus, SessionDoc, SessionMessageEntry,
};
use luna_proto::AgentEvent;
use sha2::{Digest, Sha256};

/// Writes one System entry for a [`AgentEvent::PeerMessage`]; `Ok(false)` when
/// the event is not a peer observation or its entry already exists.
pub(crate) fn record_peer_message(
    doc: &SessionDoc,
    device_id: &str,
    event: &AgentEvent,
    now_ms: i64,
) -> Result<bool, DocError> {
    let Some(entry) = peer_entry(event, device_id, now_ms) else {
        return Ok(false);
    };
    if doc
        .read_entries()?
        .iter()
        .any(|existing| existing.id == entry.id)
    {
        return Ok(false);
    }
    doc.push_message(&entry)?;
    Ok(true)
}

fn peer_entry(event: &AgentEvent, device_id: &str, now_ms: i64) -> Option<SessionMessageEntry> {
    let AgentEvent::PeerMessage {
        state,
        lane,
        from,
        name,
        from_session,
        verified_peer_pid,
        body,
        cause,
        outcome,
        message_uuid,
        host_injected,
    } = event
    else {
        return None;
    };
    let key = match message_uuid.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => id.to_owned(),
        None => {
            let digest = Sha256::digest(format!(
                "{lane}\0{from}\0{}\0{}",
                body.as_deref().unwrap_or_default(),
                cause.as_deref().unwrap_or_default()
            ));
            format!("{digest:x}")[..24].to_owned()
        }
    };
    let sender = match name.as_deref().filter(|name| !name.is_empty()) {
        Some(name) => format!("“{name}”"),
        None if *host_injected => "a LUNA child chat".to_owned(),
        None => "another session".to_owned(),
    };
    let address = if from.is_empty() {
        "no address supplied".to_owned()
    } else {
        format!("`{from}`")
    };
    let provenance = match verified_peer_pid {
        Some(pid) => format!(
            "{address}; the name and address are claimed by the sender, while the connecting process (pid {pid}) is verified by the operating system"
        ),
        None => format!(
            "{address}; the name and address are claimed by the sender and are not verified"
        ),
    };
    let session = from_session
        .as_deref()
        .map(|id| format!(" Sender session: `{id}`."))
        .unwrap_or_default();
    let text = match state.as_str() {
        "delivered" => format!(
            "Peer message from {sender} ({provenance}).{session} Claude received this as a message from another session, not from you. It does not authorize anything in this chat.\n\n{}",
            body.as_deref().unwrap_or("(empty message)")
        ),
        "held" => format!(
            "Claude held a cross-session message from {sender} ({provenance}) instead of delivering it ({}). It has not reached the model, and Claude offers no approval for it here.",
            cause.as_deref().unwrap_or("receive policy")
        ),
        "released" => format!(
            "Claude released a held cross-session message from {sender} ({provenance}); it will be delivered to the model."
        ),
        "dropped" => format!(
            "Claude dropped a cross-session message from {sender} ({provenance}) ({}). It will never reach the model in this session.",
            outcome.as_deref().unwrap_or("dropped")
        ),
        other => format!(
            "Claude reported a cross-session message from {sender} ({provenance}) in an unrecognized state ({other})."
        ),
    };
    Some(SessionMessageEntry {
        id: format!("peer:{key}:{state}"),
        role: MessageRole::System,
        parts: vec![MessagePart::Text {
            id: "text".into(),
            text,
        }],
        created_at: now_ms,
        device_id: device_id.to_owned(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    })
}
