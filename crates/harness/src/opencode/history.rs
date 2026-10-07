//! Native history: message listing, fork planning, the fork call and
//! the readback that maps source checkpoints onto the fork.

use super::*;

impl Server {
    pub(super) async fn session_messages(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<Vec<Value>, HarnessError> {
        let path = match self.protocol().await {
            Protocol::V1 => format!("/session/{session_id}/message"),
            Protocol::V2 => format!("/api/session/{session_id}/message"),
        };
        let value = unwrap_data(self.get_json(&path, directory).await?);
        value.as_array().cloned().ok_or_else(|| {
            HarnessError::Protocol("opencode session messages response was not an array".into())
        })
    }

    pub(super) async fn fork_session_staged(
        &self,
        source_session_id: &str,
        directory: Option<&str>,
        boundary: &ProviderForkBoundary,
        progress: &ProviderForkProgressReporter,
    ) -> Result<ProviderForkResult, HarnessError> {
        let source_messages = self.session_messages(source_session_id, directory).await?;
        let (expected_source, message_id) = opencode_fork_plan(&source_messages, boundary)?;
        let path = match self.protocol().await {
            Protocol::V1 => format!("/session/{source_session_id}/fork"),
            Protocol::V2 => format!("/api/session/{source_session_id}/fork"),
        };
        let body = message_id
            .map(|message_id| json!({ "messageID": message_id }))
            .unwrap_or_else(|| json!({}));
        progress(ProviderForkProgress::MutationStarted)?;
        let value = unwrap_data(self.post_json(&path, directory, &body).await?);
        let provider_session_id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| HarnessError::Protocol("opencode fork returned no session id".into()))?
            .to_owned();
        if provider_session_id == source_session_id {
            return Err(HarnessError::Protocol(
                "opencode fork returned the source session id".into(),
            ));
        }
        progress(ProviderForkProgress::Acknowledged {
            provider_session_id: provider_session_id.clone(),
        })?;
        let mut observed = None;
        for attempt in 0..5 {
            observed = self
                .session_info(&provider_session_id, directory)
                .await?
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if observed.as_deref() == Some(provider_session_id.as_str()) {
                break;
            }
            if attempt < 4 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        if observed.as_deref() != Some(provider_session_id.as_str()) {
            return Err(HarnessError::Protocol(
                "forked opencode session was not readable after acknowledgement".into(),
            ));
        }
        let fork_messages = self
            .session_messages(&provider_session_id, directory)
            .await?;
        let checkpoint_map = map_opencode_fork_history(&expected_source, &fork_messages)?;
        Ok(ProviderForkResult {
            provider_session_id,
            checkpoint_map,
        })
    }
}

pub(super) fn opencode_message_id(message: &Value) -> Option<&str> {
    message
        .get("info")
        .unwrap_or(message)
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

pub(super) fn opencode_fork_plan(
    messages: &[Value],
    boundary: &ProviderForkBoundary,
) -> Result<(Vec<Value>, Option<String>), HarnessError> {
    for message in messages {
        opencode_message_id(message).ok_or_else(|| {
            HarnessError::Protocol(
                "opencode source history contains a message without an id".into(),
            )
        })?;
    }
    match boundary {
        ProviderForkBoundary::Tip => Ok((messages.to_vec(), None)),
        ProviderForkBoundary::BeforeCheckpoint { checkpoint_id } => {
            let position = messages
                .iter()
                .position(|message| opencode_message_id(message) == Some(checkpoint_id.as_str()))
                .ok_or_else(|| {
                    HarnessError::Protocol(format!(
                        "opencode checkpoint {checkpoint_id} is not in source session"
                    ))
                })?;
            Ok((messages[..position].to_vec(), Some(checkpoint_id.clone())))
        }
        ProviderForkBoundary::ThroughCheckpoint { checkpoint_id } => {
            let position = messages
                .iter()
                .position(|message| opencode_message_id(message) == Some(checkpoint_id.as_str()))
                .ok_or_else(|| {
                    HarnessError::Protocol(format!(
                        "opencode checkpoint {checkpoint_id} is not in source session"
                    ))
                })?;
            let cutoff = match messages.get(position + 1) {
                Some(next) => Some(
                    opencode_message_id(next)
                        .ok_or_else(|| {
                            HarnessError::Protocol(
                                "opencode history after the checkpoint has no id; refusing to widen the fork to tip"
                                    .into(),
                            )
                        })?
                        .to_owned(),
                ),
                None => None,
            };
            Ok((messages[..=position].to_vec(), cutoff))
        }
    }
}

pub(super) fn take_string(
    object: &mut serde_json::Map<String, Value>,
    fields: &[&str],
) -> Option<String> {
    fields.iter().find_map(|field| {
        object
            .remove(*field)
            .and_then(|value| value.as_str().map(str::to_owned))
    })
}

pub(super) fn opencode_history_signatures(
    source: &Value,
    fork: &Value,
    checkpoint_map: &std::collections::BTreeMap<String, String>,
) -> Result<(Value, Value), HarnessError> {
    let mut source = source.clone();
    let mut fork = fork.clone();
    let source_info = source
        .get_mut("info")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| HarnessError::Protocol("opencode source message has no info".into()))?;
    let fork_info = fork
        .get_mut("info")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| HarnessError::Protocol("opencode fork message has no info".into()))?;
    let source_id = take_string(source_info, &["id"])
        .ok_or_else(|| HarnessError::Protocol("opencode source message has no id".into()))?;
    let fork_id = take_string(fork_info, &["id"])
        .ok_or_else(|| HarnessError::Protocol("opencode fork message has no id".into()))?;
    take_string(source_info, &["sessionID", "sessionId"]);
    take_string(fork_info, &["sessionID", "sessionId"]);

    let source_parent = take_string(source_info, &["parentID", "parentId"]);
    let fork_parent = take_string(fork_info, &["parentID", "parentId"]);
    match source_parent {
        Some(source_parent) => {
            let expected = checkpoint_map.get(&source_parent).ok_or_else(|| {
                HarnessError::Protocol(format!(
                    "opencode source parent {source_parent} was not copied before its child"
                ))
            })?;
            if fork_parent.as_deref() != Some(expected.as_str()) {
                return Err(HarnessError::Protocol(format!(
                    "opencode fork parent mismatch for source message {source_id}"
                )));
            }
        }
        None if fork_parent.is_some() => {
            return Err(HarnessError::Protocol(format!(
                "opencode fork added a parent to source message {source_id}"
            )));
        }
        None => {}
    }

    let source_parts = source
        .get_mut("parts")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| HarnessError::Protocol("opencode source message has no parts".into()))?;
    let fork_parts = fork
        .get_mut("parts")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| HarnessError::Protocol("opencode fork message has no parts".into()))?;
    if source_parts.len() != fork_parts.len() {
        return Err(HarnessError::Protocol(format!(
            "opencode fork part count mismatch for source message {source_id}"
        )));
    }
    for (source_part, fork_part) in source_parts.iter_mut().zip(fork_parts.iter_mut()) {
        let source_part = source_part.as_object_mut().ok_or_else(|| {
            HarnessError::Protocol("opencode source history contains a non-object part".into())
        })?;
        let fork_part = fork_part.as_object_mut().ok_or_else(|| {
            HarnessError::Protocol("opencode fork history contains a non-object part".into())
        })?;
        take_string(source_part, &["id"]);
        take_string(fork_part, &["id"]);
        take_string(source_part, &["sessionID", "sessionId"]);
        take_string(fork_part, &["sessionID", "sessionId"]);
        let source_message = take_string(source_part, &["messageID", "messageId"]);
        let fork_message = take_string(fork_part, &["messageID", "messageId"]);
        if source_message.as_deref().is_some_and(|id| id != source_id)
            || fork_message.as_deref().is_some_and(|id| id != fork_id)
        {
            return Err(HarnessError::Protocol(format!(
                "opencode part points at the wrong message for source message {source_id}"
            )));
        }
        if source_part.get("type").and_then(Value::as_str) == Some("compaction") {
            let source_tail = take_string(source_part, &["tail_start_id", "tailStartId"]);
            let fork_tail = take_string(fork_part, &["tail_start_id", "tailStartId"]);
            match source_tail {
                Some(source_tail) => {
                    let expected = checkpoint_map.get(&source_tail).ok_or_else(|| {
                        HarnessError::Protocol(format!(
                            "opencode compaction tail {source_tail} was not copied before its reference"
                        ))
                    })?;
                    if fork_tail.as_deref() != Some(expected.as_str()) {
                        return Err(HarnessError::Protocol(format!(
                            "opencode compaction tail mismatch for source message {source_id}"
                        )));
                    }
                }
                None if fork_tail.is_some() => {
                    return Err(HarnessError::Protocol(format!(
                        "opencode fork added a compaction tail to source message {source_id}"
                    )));
                }
                None => {}
            }
        }
    }
    Ok((source, fork))
}

pub(super) fn map_opencode_fork_history(
    source: &[Value],
    fork: &[Value],
) -> Result<std::collections::BTreeMap<String, String>, HarnessError> {
    if source.len() != fork.len() {
        return Err(HarnessError::Protocol(format!(
            "opencode fork history length mismatch: expected {}, observed {}",
            source.len(),
            fork.len()
        )));
    }
    let mut checkpoint_map = std::collections::BTreeMap::new();
    for (source, fork) in source.iter().zip(fork) {
        let source_id = opencode_message_id(source).ok_or_else(|| {
            HarnessError::Protocol(
                "opencode source history contains a message without an id".into(),
            )
        })?;
        let fork_id = opencode_message_id(fork).ok_or_else(|| {
            HarnessError::Protocol("opencode fork history contains a message without an id".into())
        })?;
        let (source_signature, fork_signature) =
            opencode_history_signatures(source, fork, &checkpoint_map)?;
        if source_signature != fork_signature {
            return Err(HarnessError::Protocol(format!(
                "opencode fork history diverged at source message {source_id}"
            )));
        }
        checkpoint_map.insert(source_id.to_owned(), fork_id.to_owned());
    }
    Ok(checkpoint_map)
}
