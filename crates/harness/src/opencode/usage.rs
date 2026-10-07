//! Usage, context occupancy and provider-owned compaction.

use super::*;

impl Server {
    /// Provider-owned compaction. The legacy wire calls its documented
    /// `summarize` endpoint with the selected provider/model; the current wire
    /// exposes a dedicated compact operation.
    pub(super) async fn compact_session(
        &self,
        session_id: &str,
        directory: Option<&str>,
        model: Option<&str>,
    ) -> Result<(), HarnessError> {
        let (path, body) = compact_http_request(self.protocol().await, session_id, model)?;
        self.post_json(&path, directory, &body).await.map(|_| ())
    }
}

pub(super) fn compact_http_request(
    protocol: Protocol,
    session_id: &str,
    model: Option<&str>,
) -> Result<(String, Value), HarnessError> {
    match protocol {
        Protocol::V1 => {
            let (provider, model) =
                model
                    .and_then(|model| model.split_once('/'))
                    .ok_or_else(|| {
                        HarnessError::Protocol(
                            "OpenCode compaction needs the session's provider/model id".into(),
                        )
                    })?;
            Ok((
                format!("/session/{session_id}/summarize"),
                json!({ "providerID": provider, "modelID": model }),
            ))
        }
        Protocol::V2 => Ok((format!("/api/session/{session_id}/compact"), Value::Null)),
    }
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
