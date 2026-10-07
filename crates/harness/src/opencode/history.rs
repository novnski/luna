//! Native history (OC-12): the cursor-paged v2 message list, turn
//! identities, the fork with its exclusive `before` boundary and readback,
//! and same-session rewind. Verified against 2.0.22.
//!
//! **Turn identity.** v2 history is a flat, ordered list of items
//! (`{id: "msg_…", type, …}`); an `idle` item closes every execution. A
//! native turn is the region after an `idle` item (or the session start)
//! that contains a `user` item, and its identity is that region's FIRST user
//! item, exactly like Claude's turn-start uuid. The run loop persists it as
//! the `ProviderCheckpoint` (read back at turn end, anchored on the turn's
//! last assistant step), `native_history_turns` lists it, and every history
//! mutation cuts at whole turn regions:
//! - fork `BeforeCheckpoint{turn}` sends `{before: <first item of the turn's
//!   region>}` (`before` is exclusive: `projector.ts`, `seq < boundary`);
//! - fork `ThroughCheckpoint{turn}` sends `{before: <first item after the
//!   turn's closing idle>}`, or `{}` when nothing follows;
//! - rewind stages `revert.stage{messageID: <first item of the region>,
//!   files: false}` and commits it (the commit deletes that item and every
//!   later one; files are always kept).
//!
//! A region with no user item (a background completion's wake) is never a
//! turn. A staged but uncommitted revert ends the active history at its
//! boundary, because the next prompt commits it.

use super::*;
use crate::NativeRewindProgress;

/// Items per message page (the route's maximum).
const PAGE_LIMIT: usize = 200;
/// Full readbacks stop here (400k items) instead of paging forever.
const MAX_PAGES: usize = 2_000;
/// The turn-end checkpoint readback pages newest first; a turn longer than
/// this many pages gets no checkpoint rather than a long stall.
const CHECKPOINT_PAGES: usize = 25;

/// One native turn: its identity and its region `[start, end)` in the
/// active history. `end` is one past the closing `idle` item, or the
/// history length when the turn never closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeTurn {
    pub(super) id: String,
    pub(super) start: usize,
    pub(super) end: usize,
}

/// The session's active history: every item before a staged revert.
pub(super) struct ActiveHistory {
    pub(super) messages: Vec<Value>,
    /// The boundary of a staged, uncommitted revert.
    pub(super) staged_revert: Option<String>,
}

/// Messages read newest first or oldest first; cursors carry the order.
#[derive(Clone, Copy)]
enum Order {
    Asc,
    Desc,
}

/// Percent-encode a cursor for the query string (cursors are base64url
/// today, but they are opaque).
fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(super) fn opencode_message_id(message: &Value) -> Option<&str> {
    message
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

fn message_type(message: &Value) -> &str {
    message.get("type").and_then(Value::as_str).unwrap_or("")
}

fn require_ids(messages: &[Value], what: &str) -> Result<(), HarnessError> {
    if messages.iter().any(|m| opencode_message_id(m).is_none()) {
        return Err(HarnessError::Protocol(format!(
            "opencode {what} history contains a message without an id"
        )));
    }
    Ok(())
}

/// The native turns of an active history, oldest first.
pub(super) fn opencode_turns(messages: &[Value]) -> Vec<NativeTurn> {
    let mut turns = Vec::new();
    let mut region_start = 0;
    let mut open: Option<(String, usize)> = None;
    for (index, message) in messages.iter().enumerate() {
        match message_type(message) {
            "idle" => {
                if let Some((id, start)) = open.take() {
                    turns.push(NativeTurn {
                        id,
                        start,
                        end: index + 1,
                    });
                }
                region_start = index + 1;
            }
            "user" if open.is_none() => {
                if let Some(id) = opencode_message_id(message) {
                    open = Some((id.to_owned(), region_start));
                }
            }
            _ => {}
        }
    }
    if let Some((id, start)) = open {
        turns.push(NativeTurn {
            id,
            start,
            end: messages.len(),
        });
    }
    turns
}

/// A completed compaction hides earlier turns from the live context.
pub(super) fn opencode_compacted(messages: &[Value]) -> bool {
    messages.iter().any(|message| {
        message_type(message) == "compaction"
            && message.get("status").and_then(Value::as_str) == Some("completed")
    })
}

/// Items a fork copies: OpenCode copies only settled history (an assistant
/// step without `time.completed`, a running shell or a running compaction
/// stays on the source).
fn fork_copies(message: &Value) -> bool {
    match message_type(message) {
        "assistant" => message
            .pointer("/time/completed")
            .is_some_and(|v| !v.is_null()),
        "shell" | "compaction" => message.get("status").and_then(Value::as_str) != Some("running"),
        _ => true,
    }
}

fn find_turn<'a>(
    turns: &'a [NativeTurn],
    checkpoint: &str,
) -> Result<&'a NativeTurn, HarnessError> {
    turns
        .iter()
        .find(|turn| turn.id == checkpoint)
        .ok_or_else(|| {
            HarnessError::Protocol(format!(
                "opencode checkpoint {checkpoint} is not a turn start in the source session"
            ))
        })
}

/// The items a fork at `boundary` copies, and its exclusive `before` id
/// (`None` = the full history). `staged_revert` is the active history's
/// end when a revert is staged on the source.
pub(super) fn opencode_fork_plan(
    messages: &[Value],
    staged_revert: Option<&str>,
    boundary: &ProviderForkBoundary,
) -> Result<(Vec<Value>, Option<String>), HarnessError> {
    require_ids(messages, "source")?;
    let turns = opencode_turns(messages);
    let cut = match boundary {
        ProviderForkBoundary::Tip => messages.len(),
        ProviderForkBoundary::BeforeCheckpoint { checkpoint_id } => {
            find_turn(&turns, checkpoint_id)?.start
        }
        ProviderForkBoundary::ThroughCheckpoint { checkpoint_id } => {
            find_turn(&turns, checkpoint_id)?.end
        }
    };
    if cut == 0 {
        // OpenCode refuses an empty fork (`empty_session`); refuse first.
        return Err(HarnessError::Protocol(
            "opencode has no history before this point to fork".into(),
        ));
    }
    let before = messages
        .get(cut)
        .and_then(opencode_message_id)
        .map(str::to_owned)
        .or_else(|| staged_revert.map(str::to_owned));
    let copied = messages[..cut]
        .iter()
        .filter(|message| fork_copies(message))
        .cloned()
        .collect();
    Ok((copied, before))
}

/// A copied item's content: everything except its regenerated id.
fn signature(message: &Value) -> Value {
    let mut message = message.clone();
    if let Some(object) = message.as_object_mut() {
        object.remove("id");
    }
    message
}

/// Map every expected source item onto the fork's copy, position by
/// position; the fork must hold exactly those items with identical content.
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
        if signature(source) != signature(fork) {
            return Err(HarnessError::Protocol(format!(
                "opencode fork history diverged at source message {source_id}"
            )));
        }
        checkpoint_map.insert(source_id.to_owned(), fork_id.to_owned());
    }
    Ok(checkpoint_map)
}

/// The turn-start identity of the turn whose region holds `anchor`, from a
/// newest-first slice that reaches back at least to the idle item before
/// it (or the session start when `complete`). `None` when the anchor isn't
/// there yet, or its region has no user item (a wake).
fn turn_start_from_desc(desc: &[Value], anchor: &str, complete: bool) -> Option<Option<String>> {
    let position = desc
        .iter()
        .position(|message| opencode_message_id(message) == Some(anchor))?;
    let region = match desc[position..]
        .iter()
        .position(|message| message_type(message) == "idle")
    {
        Some(offset) => &desc[position..position + offset],
        None if complete => &desc[position..],
        None => return None,
    };
    Some(
        region
            .iter()
            .filter(|message| message_type(message) == "user")
            .filter_map(opencode_message_id)
            .last()
            .map(str::to_owned),
    )
}

/// The `_tag` of a 2.x error envelope, as a refusal reason.
fn envelope_tag(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body.trim())
        .ok()?
        .get("_tag")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn refused(reason: &str, message: impl Into<String>) -> NativeRewindOutcome {
    NativeRewindOutcome::Refused {
        reason: reason.into(),
        message: message.into(),
    }
}

/// What resuming a session that no longer exists says. LUNA never replaces
/// it silently: the chat keeps its history and Clear context starts fresh.
pub(super) fn deleted_session_message(session_id: &str) -> String {
    format!(
        "This chat's OpenCode session {session_id} no longer exists (it was deleted outside \
         LUNA). LUNA kept this chat's history; use Clear context… to continue in a new \
         OpenCode session."
    )
}

impl Server {
    /// One page of messages: the items and the next cursor.
    async fn message_page(
        &self,
        session_id: &str,
        directory: Option<&str>,
        start: Order,
        cursor: Option<&str>,
    ) -> Result<(Vec<Value>, Option<String>), HarnessError> {
        // A cursor carries its order; the route refuses both together.
        let query = match cursor {
            Some(cursor) => format!("cursor={}&limit={PAGE_LIMIT}", encode_query(cursor)),
            None => format!(
                "order={}&limit={PAGE_LIMIT}",
                match start {
                    Order::Asc => "asc",
                    Order::Desc => "desc",
                }
            ),
        };
        let path = format!("/api/session/{session_id}/message?{query}");
        let page = self.get_json(&path, directory).await?;
        let items = page
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| {
                HarnessError::Protocol("opencode session messages response was not an array".into())
            })?;
        let next = page
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .filter(|next| !next.is_empty())
            .map(str::to_owned);
        Ok((items, next))
    }

    /// Every message of a session, oldest first (cursor-paged; the route's
    /// default is the newest 50, newest first).
    pub(super) async fn session_messages(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<Vec<Value>, HarnessError> {
        let mut messages = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let (items, next) = self
                .message_page(session_id, directory, Order::Asc, cursor.as_deref())
                .await?;
            if items.is_empty() {
                return Ok(messages);
            }
            messages.extend(items);
            match next {
                Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
                Some(_) => {
                    return Err(HarnessError::Protocol(
                        "opencode message paging repeated its cursor".into(),
                    ));
                }
                None => return Ok(messages),
            }
        }
        Err(HarnessError::Protocol(format!(
            "opencode session {session_id} has more than {} messages",
            MAX_PAGES * PAGE_LIMIT
        )))
    }

    /// The active history: all messages, cut at a staged revert.
    pub(super) async fn active_history(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<ActiveHistory, HarnessError> {
        let info = self.session_info(session_id, directory).await?;
        let staged_revert = info
            .pointer("/revert/messageID")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut messages = self.session_messages(session_id, directory).await?;
        if let Some(boundary) = staged_revert.as_deref() {
            let cut = messages
                .iter()
                .position(|message| opencode_message_id(message) == Some(boundary))
                .ok_or_else(|| {
                    HarnessError::Protocol(format!(
                        "opencode staged revert boundary {boundary} is not in the session history"
                    ))
                })?;
            messages.truncate(cut);
        }
        Ok(ActiveHistory {
            messages,
            staged_revert,
        })
    }

    /// The session for a resume, or the actionable "deleted outside LUNA"
    /// error when OpenCode no longer has it.
    pub(super) async fn resumable_session(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<Value, HarnessError> {
        let path = format!("/api/session/{session_id}");
        let (status, text) = self
            .exchange(
                reqwest::Method::GET,
                &path,
                directory,
                None,
                Some(CALL_TIMEOUT),
            )
            .await?;
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(HarnessError::Protocol(deleted_session_message(session_id)));
        }
        if !status.is_success() {
            return Err(HarnessError::Protocol(http_error_message(
                "GET", &path, status, &text,
            )));
        }
        let value = serde_json::from_str(&text)
            .map_err(|e| HarnessError::Protocol(format!("opencode GET {path}: {e}")))?;
        Ok(unwrap_data(value))
    }

    /// The finished turn's checkpoint: the first user item of the region
    /// holding `anchor` (the turn's last assistant step), read newest first.
    pub(super) async fn turn_checkpoint(
        &self,
        session_id: &str,
        directory: Option<&str>,
        anchor: &str,
    ) -> Result<Option<String>, HarnessError> {
        let mut desc = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..CHECKPOINT_PAGES {
            let (items, next) = self
                .message_page(session_id, directory, Order::Desc, cursor.as_deref())
                .await?;
            let complete = items.is_empty() || next.is_none();
            desc.extend(items);
            if let Some(found) = turn_start_from_desc(&desc, anchor, complete) {
                return Ok(found);
            }
            if complete || next == cursor {
                return Ok(None);
            }
            cursor = next;
        }
        Ok(None)
    }

    /// `native_history_turns`: the active history's turn identities.
    pub(super) async fn native_turns(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<NativeHistoryTurns, HarnessError> {
        let history = self.active_history(session_id, directory).await?;
        Ok(NativeHistoryTurns {
            turn_starts: opencode_turns(&history.messages)
                .into_iter()
                .map(|turn| turn.id)
                .collect(),
            compacted: opencode_compacted(&history.messages),
        })
    }

    async fn delete_session(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<(), HarnessError> {
        let path = format!("/api/session/{session_id}");
        let (status, text) = self
            .exchange(
                reqwest::Method::DELETE,
                &path,
                directory,
                None,
                Some(CALL_TIMEOUT),
            )
            .await?;
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(HarnessError::Protocol(http_error_message(
                "DELETE", &path, status, &text,
            )))
        }
    }

    pub(super) async fn fork_session_staged(
        &self,
        source_session_id: &str,
        directory: Option<&str>,
        boundary: &ProviderForkBoundary,
        progress: &ProviderForkProgressReporter,
    ) -> Result<ProviderForkResult, HarnessError> {
        let source = self.active_history(source_session_id, directory).await?;
        let (expected, before) =
            opencode_fork_plan(&source.messages, source.staged_revert.as_deref(), boundary)?;
        let path = format!("/api/session/{source_session_id}/fork");
        let body = match &before {
            Some(before) => json!({ "before": before }),
            None => json!({}),
        };
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
        let verified = async {
            // The fork reports the boundary it applied; it must be ours.
            let applied = value.pointer("/fork/boundary");
            let applied_ok = match (&before, applied) {
                (Some(before), Some(applied)) => {
                    applied.get("type").and_then(Value::as_str) == Some("before")
                        && applied.get("messageID").and_then(Value::as_str) == Some(before)
                }
                (None, Some(applied)) => {
                    applied.get("type").and_then(Value::as_str) == Some("through")
                }
                (_, None) => true,
            };
            if !applied_ok {
                return Err(HarnessError::Protocol(format!(
                    "opencode fork applied a different boundary: {}",
                    applied.cloned().unwrap_or(Value::Null)
                )));
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
            map_opencode_fork_history(&expected, &fork_messages)
        }
        .await;
        match verified {
            Ok(checkpoint_map) => Ok(ProviderForkResult {
                provider_session_id,
                checkpoint_map,
            }),
            // Never leave an unverified fork behind: delete the session this
            // call created before reporting the failure.
            Err(error) => match self.delete_session(&provider_session_id, directory).await {
                Ok(()) => Err(HarnessError::Protocol(format!(
                    "{error}; the unverified fork {provider_session_id} was deleted"
                ))),
                Err(cleanup) => Err(HarnessError::Protocol(format!(
                    "{error}; deleting the unverified fork {provider_session_id} also failed: {cleanup}"
                ))),
            },
        }
    }

    /// One revert-route call; `Ok((status, body))` for any HTTP answer.
    async fn revert_call(
        &self,
        method: reqwest::Method,
        session_id: &str,
        suffix: &str,
        directory: Option<&str>,
        body: Option<&Value>,
    ) -> Result<(reqwest::StatusCode, String), HarnessError> {
        let path = format!("/api/session/{session_id}/revert{suffix}");
        self.exchange(method, &path, directory, body, Some(CALL_TIMEOUT))
            .await
    }

    /// Clear a staged revert (`DELETE …/revert`); OpenCode then wakes the
    /// session, which an idle session ends at once (the turn list ignores
    /// the empty `idle` item it may leave).
    async fn clear_revert(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<(), HarnessError> {
        let (status, text) = self
            .revert_call(reqwest::Method::DELETE, session_id, "", directory, None)
            .await?;
        if status.is_success() {
            Ok(())
        } else {
            Err(HarnessError::Protocol(http_error_message(
                "DELETE",
                &format!("/api/session/{session_id}/revert"),
                status,
                &text,
            )))
        }
    }

    /// `rewind_native_conversation`: remove the target turn and every later
    /// one from this same session, keeping files. Refusals happen before
    /// any mutation; an answer that leaves the outcome unknown is an error
    /// (the engine reconciles by readback, where a staged revert reads as
    /// applied because the next prompt commits it).
    pub(super) async fn rewind_conversation(
        &self,
        request: &NativeRewindRequest,
        progress: &NativeRewindProgressReporter,
    ) -> Result<NativeRewindOutcome, HarnessError> {
        let session_id = request.scope.session_id.as_str();
        let directory = Some(request.scope.cwd.as_str()).filter(|cwd| !cwd.is_empty());
        if self
            .session_running(session_id, directory)
            .await
            .unwrap_or(true)
        {
            return Ok(refused(
                "turn_running",
                "OpenCode is running a turn in this session",
            ));
        }
        let info = self.session_info(session_id, directory).await?;
        if info.get("revert").is_some_and(|revert| !revert.is_null()) {
            return Ok(refused(
                "revert_staged",
                "This OpenCode session has a revert staged by another client. Apply or restore it in OpenCode first.",
            ));
        }
        let messages = self.session_messages(session_id, directory).await?;
        require_ids(&messages, "session")?;
        let turns = opencode_turns(&messages);
        let Some(position) = turns
            .iter()
            .position(|turn| turn.id == request.target_checkpoint)
        else {
            return Ok(refused(
                "target_not_found",
                "The selected turn is not in this OpenCode session",
            ));
        };
        if turns.last().map(|turn| turn.id.as_str()) != Some(request.last_seen_checkpoint.as_str())
        {
            return Ok(refused(
                "unseen_later_turn",
                "OpenCode holds a later turn that LUNA has not shown",
            ));
        }
        // Activity outside every turn after the target (a background wake)
        // would be removed without LUNA ever having listed it.
        let target = &turns[position];
        let covered = |index: usize| turns.iter().any(|t| (t.start..t.end).contains(&index));
        let unseen = messages
            .iter()
            .enumerate()
            .skip(target.start)
            .any(|(index, message)| {
                matches!(
                    message_type(message),
                    "assistant" | "synthetic" | "shell" | "skill" | "compaction" | "user"
                ) && !covered(index)
            });
        if unseen {
            return Ok(refused(
                "unseen_later_turn",
                "OpenCode holds activity after the selected turn that is not a LUNA turn",
            ));
        }
        let boundary = opencode_message_id(&messages[target.start])
            .unwrap_or_default()
            .to_owned();

        progress(NativeRewindProgress::RequestWritten);
        let (status, text) = self
            .revert_call(
                reqwest::Method::POST,
                session_id,
                "/stage",
                directory,
                Some(&json!({ "messageID": boundary, "files": false })),
            )
            .await?;
        if status.is_client_error() {
            return Ok(refused(
                &envelope_tag(&text).unwrap_or_else(|| "stage_refused".into()),
                describe_error_envelope(&text).unwrap_or_else(|| truncate_body(&text)),
            ));
        }
        if !status.is_success() {
            return Err(HarnessError::Protocol(format!(
                "OpenCode's revert staging outcome is unknown: {}",
                http_error_message("POST", "revert/stage", status, &text)
            )));
        }
        let staged = serde_json::from_str::<Value>(&text)
            .map(unwrap_data)
            .unwrap_or(Value::Null);
        if staged
            .get("files")
            .and_then(Value::as_array)
            .is_some_and(|files| !files.is_empty())
        {
            let cleared = self.clear_revert(session_id, directory).await;
            return Err(HarnessError::Protocol(format!(
                "OpenCode reported file changes for a files-kept rewind ({}); {}",
                staged["files"],
                match cleared {
                    Ok(()) => "the staged revert was cleared".to_owned(),
                    Err(error) => format!("clearing the staged revert failed: {error}"),
                }
            )));
        }
        let commit = self
            .revert_call(
                reqwest::Method::POST,
                session_id,
                "/commit",
                directory,
                None,
            )
            .await;
        if let Ok((status, _)) = &commit
            && status.is_success()
        {
            return Ok(NativeRewindOutcome::Rewound);
        }
        let failure = match &commit {
            Ok((status, text)) => http_error_message("POST", "revert/commit", *status, text),
            Err(error) => error.to_string(),
        };
        // Settle what the failed commit left: still staged (clear it), gone
        // and applied by someone else's prompt, or cleared by another client.
        let info = self
            .session_info(session_id, directory)
            .await
            .map_err(|error| {
                HarnessError::Protocol(format!(
                    "OpenCode's revert commit failed ({failure}) and its state is unknown: {error}"
                ))
            })?;
        let still_staged =
            info.pointer("/revert/messageID").and_then(Value::as_str) == Some(boundary.as_str());
        if still_staged {
            return match self.clear_revert(session_id, directory).await {
                Ok(()) => Ok(refused(
                    "commit_failed",
                    format!(
                        "OpenCode did not apply the rewind ({failure}); the staged revert was cleared"
                    ),
                )),
                Err(error) => Err(HarnessError::Protocol(format!(
                    "OpenCode's revert commit failed ({failure}) and clearing it failed: {error}"
                ))),
            };
        }
        let after = self.session_messages(session_id, directory).await?;
        if after
            .iter()
            .any(|message| opencode_message_id(message) == Some(boundary.as_str()))
        {
            Ok(refused(
                "revert_cleared",
                format!("OpenCode did not apply the rewind ({failure}); nothing was removed"),
            ))
        } else {
            Ok(NativeRewindOutcome::Rewound)
        }
    }
}
