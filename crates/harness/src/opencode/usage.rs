//! Usage, context occupancy and provider-owned compaction.

use super::*;

impl Server {
    /// Provider-owned compaction: `POST /api/session/{id}/compact` with a
    /// JSON object body (`null` is rejected). Admission only; the compaction
    /// lifecycle (`session.compaction.*`) is not awaited here.
    pub(super) async fn compact_session(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<(), HarnessError> {
        let (path, body) = compact_http_request(session_id);
        self.post_json(&path, directory, &body).await.map(|_| ())
    }
}

/// `session.compact` takes `{id?, delivery?}`; LUNA sends an empty object.
pub(super) fn compact_http_request(session_id: &str) -> (String, Value) {
    (format!("/api/session/{session_id}/compact"), json!({}))
}

pub(super) fn context_usage_event(
    info: &Value,
    context_windows: &HashMap<String, u64>,
) -> Option<AgentEvent> {
    let tokens = info.get("tokens")?;
    let counts: Vec<u64> = [
        tokens.get("input"),
        tokens.get("output"),
        tokens.pointer("/cache/read"),
        tokens.pointer("/cache/write"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_u64)
    .collect();
    let tokens = tokens
        .get("total")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .or_else(|| {
            (!counts.is_empty()).then(|| counts.into_iter().fold(0u64, u64::saturating_add))
        });
    // New assistant placeholders carry zero counters before the request runs.
    if tokens == Some(0) && info.pointer("/time/completed").is_none() {
        return None;
    }
    let window = info
        .get("providerID")
        .and_then(Value::as_str)
        .zip(info.get("modelID").and_then(Value::as_str))
        .and_then(|(provider, model)| context_windows.get(&format!("{provider}/{model}")).copied());
    (tokens.is_some() || window.is_some()).then_some(AgentEvent::ContextUsage { tokens, window })
}
