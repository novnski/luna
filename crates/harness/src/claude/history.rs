//! CC-10: Claude Code native history (CLI 2.1.286).
//!
//! - **Turn identity.** Every stdin user line LUNA writes carries a client
//!   `uuid`, the Agent SDK's `SDKUserMessage.uuid`. The CLI persists the prompt
//!   under that uuid and echoes the uuids a turn consumed as the result frame's
//!   `user_message_uuid(s)`. A completed turn's first consumed LUNA uuid is its
//!   `ProviderCheckpoint`.
//! - **Readback.** The native transcript
//!   `<config>/projects/<cwd slug>/<session>.jsonl` is a `uuid`/`parentUuid`
//!   tree. The active chain runs from the leaf the CLI's own loader picks (an
//!   explicit `last-prompt` leaf with no later message, otherwise the newest
//!   message) back to the root.
//! - **Fork.** The CLI's `--fork-session` persists nothing until a model turn
//!   runs (probed init-only against 2.1.286). Forks therefore follow the Agent
//!   SDK's `forkSession` contract: copy the active chain up to an anchor into
//!   a new session file with fresh uuids, `forkedFrom` provenance and a title,
//!   without file history. Before acknowledging, LUNA has the real CLI load the
//!   fork read-only (`--resume --fork-session --resume-session-at <tip>`, which
//!   writes nothing).
//! - **Same-session rewind and file checkpoints** use the CLI's own
//!   `rewind_conversation` and `rewind_files` control requests on a resumed,
//!   control-only process. No user message is written, so no model turn runs.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use luna_proto::{
    AgentEvent, DoneStatus, ProviderForkBoundary, ProviderForkRequest, ProviderForkResult,
};

use crate::native_history::{
    NativeFileRewindRequest, NativeFileRewindResult, NativeHistoryScope, NativeHistoryTurns,
    NativeRewindOutcome, NativeRewindProgress, NativeRewindProgressReporter, NativeRewindRequest,
};
use crate::process::{Command, Stdio};
use crate::{HarnessError, ProviderForkProgress, ProviderForkProgressReporter, shutdown_child};

/// The Agent SDK's `enableFileCheckpointing` switch. Without it a
/// non-interactive CLI records no file backups and answers `rewind_files`
/// with "File rewinding is not enabled." (probed against 2.1.286).
pub(crate) const FILE_CHECKPOINT_ENV: &str = "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING";
const CONTROL_TIMEOUT: Duration = Duration::from_secs(20);
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);
const KILL_GRACE: Duration = Duration::from_secs(2);
/// The CLI hashes longer cwd slugs; those directories are found by scanning.
const MAX_PROJECT_SLUG: usize = 200;
const CHAIN_TYPES: [&str; 5] = ["user", "assistant", "system", "attachment", "progress"];
const INIT_REQUEST_ID: &str = "luna-history-init";
/// Refusals the CLI returns before any durable change (`rewind_conversation`).
const REWIND_REFUSALS: [&str; 9] = [
    "commands_queued",
    "prompt_pending",
    "turn_running",
    "target_not_found",
    "stale_target",
    "unseen_later_turn",
    "poll_tool_result_target",
    "target_splits_tool_call",
    "delivered_poll_events_in_range",
];

fn protocol(message: impl Into<String>) -> HarnessError {
    HarnessError::Protocol(message.into())
}

// ── turn identity ───────────────────────────────────────────────────────────

/// Client uuids for one run's stdin prompts, and the checkpoint each
/// completed turn earns.
#[derive(Debug, Default)]
pub(crate) struct TurnIdentities {
    /// Written by LUNA and not yet reported consumed by a result frame.
    sent: Vec<String>,
    checkpointed: HashSet<String>,
    pending: Option<String>,
}

impl TurnIdentities {
    /// Stamp a stdin user line with a fresh client uuid.
    pub(crate) fn stamp(&mut self, line: String) -> String {
        let Ok(Value::Object(mut frame)) = serde_json::from_str::<Value>(&line) else {
            return line;
        };
        let id = uuid::Uuid::new_v4().to_string();
        frame.insert("uuid".into(), Value::String(id.clone()));
        self.sent.push(id);
        Value::Object(frame).to_string()
    }

    /// Read a raw stdout line. Only `result` frames matter: their
    /// `user_message_uuids` (or the single `user_message_uuid` of an older
    /// producer) name the prompts the turn consumed, in consumption order.
    pub(crate) fn observe(&mut self, raw: &str) {
        if !raw.contains("\"result\"") {
            return;
        }
        let Ok(frame) = serde_json::from_str::<Value>(raw) else {
            return;
        };
        if frame.get("type").and_then(Value::as_str) != Some("result") {
            return;
        }
        let consumed: Vec<String> = match frame.get("user_message_uuids").and_then(Value::as_array)
        {
            Some(ids) => ids
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            None => frame
                .get("user_message_uuid")
                .and_then(Value::as_str)
                .map(|id| vec![id.to_owned()])
                .unwrap_or_default(),
        };
        let all_ours = !consumed.is_empty() && consumed.iter().all(|id| self.sent.contains(id));
        let succeeded = frame.get("subtype").and_then(Value::as_str) == Some("success")
            && frame.get("is_error").and_then(Value::as_bool) != Some(true);
        self.sent.retain(|id| !consumed.contains(id));
        self.pending = consumed
            .into_iter()
            .next()
            .filter(|first| all_ours && succeeded && !self.checkpointed.contains(first));
    }

    /// The checkpoint to forward immediately before a completed `Done`.
    pub(crate) fn checkpoint_before(&mut self, event: &AgentEvent) -> Option<AgentEvent> {
        match event {
            AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            } => {
                let id = self.pending.take()?;
                self.checkpointed.insert(id.clone());
                Some(AgentEvent::ProviderCheckpoint { checkpoint_id: id })
            }
            AgentEvent::Done { .. } => {
                self.pending = None;
                None
            }
            _ => None,
        }
    }
}

// ── native transcript ───────────────────────────────────────────────────────

fn entry_type(entry: &Value) -> Option<&str> {
    entry.get("type").and_then(Value::as_str)
}

fn entry_uuid(entry: &Value) -> Option<&str> {
    entry.get("uuid").and_then(Value::as_str)
}

fn is_chain_message(entry: &Value) -> bool {
    entry_uuid(entry).is_some()
        && entry_type(entry).is_some_and(|kind| CHAIN_TYPES.contains(&kind))
        && entry.get("isSidechain").and_then(Value::as_bool) != Some(true)
}

/// A human prompt that begins a turn: tool results, meta and compaction
/// summary rows are user-role entries too, but never turn starts.
fn is_human_turn_start(entry: &Value) -> bool {
    if entry_type(entry) != Some("user")
        || entry.get("isMeta").and_then(Value::as_bool) == Some(true)
        || entry.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
        || entry.get("toolUseResult").is_some()
    {
        return false;
    }
    match entry.pointer("/message/content") {
        Some(Value::String(_)) => true,
        Some(Value::Array(blocks)) => blocks
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) != Some("tool_result")),
        _ => false,
    }
}

fn project_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn config_dir(provider_home: Option<&str>) -> Result<PathBuf, HarnessError> {
    provider_home
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| crate::executable::home_dir().map(|home| home.join(".claude")))
        .ok_or_else(|| protocol("Claude's configuration directory is unavailable"))
}

/// `CLAUDE_CONFIG_DIR` for a history child only when the engine's home is not
/// the CLI's own default. Setting it explicitly to the default would change
/// the CLI's credential-store key, so the inherited environment is preferred.
fn config_override(provider_home: Option<&str>) -> Option<PathBuf> {
    let home = PathBuf::from(provider_home.filter(|home| !home.is_empty())?);
    let Ok(default) = config_dir(None) else {
        return Some(home);
    };
    let same = home == default
        || std::fs::canonicalize(&home)
            .ok()
            .zip(std::fs::canonicalize(&default).ok())
            .is_some_and(|(left, right)| left == right);
    (!same).then_some(home)
}

fn require_uuid(value: &str, what: &str) -> Result<(), HarnessError> {
    uuid::Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| protocol(format!("{what} is not a Claude UUID")))
}

#[derive(Debug, Clone, Copy)]
enum ForkCut {
    /// Copy the active chain through this inclusive position.
    Through(usize),
    /// Nothing precedes the prompt at this position.
    BeforeFirst(usize),
}

#[derive(Debug, Clone)]
pub(crate) struct Transcript {
    entries: Vec<Value>,
    /// Indexes into `entries`, from the root to the active leaf.
    chain: Vec<usize>,
    compacted: bool,
}

impl Transcript {
    pub(crate) fn locate(
        config_dir: &Path,
        cwd: &str,
        session_id: &str,
    ) -> Result<PathBuf, HarnessError> {
        require_uuid(session_id, "the Claude session id")?;
        let file = format!("{session_id}.jsonl");
        let projects = config_dir.join("projects");
        let slug = project_slug(cwd);
        if !cwd.is_empty() && slug.len() <= MAX_PROJECT_SLUG {
            let direct = projects.join(&slug).join(&file);
            if direct.is_file() {
                return Ok(direct);
            }
        }
        let mut found = Vec::new();
        if let Ok(directories) = std::fs::read_dir(&projects) {
            for directory in directories.flatten() {
                let candidate = directory.path().join(&file);
                if candidate.is_file() {
                    found.push(candidate);
                }
            }
        }
        match found.len() {
            0 => Err(protocol(format!(
                "Claude session {session_id} has no native transcript under {}",
                projects.display()
            ))),
            1 => Ok(found.remove(0)),
            _ => Err(protocol(format!(
                "Claude session {session_id} has more than one native transcript"
            ))),
        }
    }

    pub(crate) fn load(path: &Path) -> Result<Self, HarnessError> {
        let text = std::fs::read_to_string(path)?;
        let entries: Vec<Value> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let mut first_index: HashMap<String, usize> = HashMap::new();
        let mut last_message: Option<String> = None;
        let mut explicit_leaf: Option<Option<String>> = None;
        for (index, entry) in entries.iter().enumerate() {
            if is_chain_message(entry) {
                let uuid = entry_uuid(entry).unwrap_or_default().to_owned();
                first_index.entry(uuid.clone()).or_insert(index);
                last_message = Some(uuid);
                explicit_leaf = None;
            } else if entry_type(entry) == Some("last-prompt")
                && entry.get("explicit").and_then(Value::as_bool) == Some(true)
            {
                explicit_leaf = Some(
                    entry
                        .get("leafUuid")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                );
            }
        }
        let mut cursor = explicit_leaf.unwrap_or(last_message);
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut compacted = false;
        while let Some(uuid) = cursor {
            if !seen.insert(uuid.clone()) {
                return Err(protocol("Claude transcript has a parentUuid cycle"));
            }
            let index = *first_index.get(&uuid).ok_or_else(|| {
                protocol(format!("Claude transcript is missing chain message {uuid}"))
            })?;
            let entry = &entries[index];
            if entry.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
                || entry
                    .get("logicalParentUuid")
                    .and_then(Value::as_str)
                    .is_some()
                || entry.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
            {
                compacted = true;
            }
            chain.push(index);
            cursor = entry
                .get("parentUuid")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        chain.reverse();
        Ok(Self {
            entries,
            chain,
            compacted,
        })
    }

    fn chain_entry(&self, position: usize) -> &Value {
        &self.entries[self.chain[position]]
    }

    fn chain_position(&self, uuid: &str) -> Option<usize> {
        self.chain
            .iter()
            .position(|&index| entry_uuid(&self.entries[index]) == Some(uuid))
    }

    pub(crate) fn turns(&self) -> NativeHistoryTurns {
        NativeHistoryTurns {
            turn_starts: self
                .chain
                .iter()
                .map(|&index| &self.entries[index])
                .filter(|entry| is_human_turn_start(entry))
                .filter_map(entry_uuid)
                .map(str::to_owned)
                .collect(),
            compacted: self.compacted,
        }
    }

    /// Where a fork ends on the active chain.
    fn fork_end(&self, boundary: &ProviderForkBoundary) -> Result<ForkCut, HarnessError> {
        if self.chain.is_empty() {
            return Err(protocol("the Claude session has no conversation to fork"));
        }
        let turn = |id: &str| -> Result<usize, HarnessError> {
            let position = self.chain_position(id).ok_or_else(|| {
                protocol("the selected Claude turn is not in the session's active history")
            })?;
            if !is_human_turn_start(self.chain_entry(position)) {
                return Err(protocol(
                    "the selected Claude checkpoint is not a prompt turn start",
                ));
            }
            Ok(position)
        };
        match boundary {
            ProviderForkBoundary::Tip => Ok(ForkCut::Through(self.chain.len() - 1)),
            ProviderForkBoundary::BeforeCheckpoint { checkpoint_id } => {
                let position = turn(checkpoint_id)?;
                Ok(match position.checked_sub(1) {
                    Some(end) => ForkCut::Through(end),
                    None => ForkCut::BeforeFirst(position),
                })
            }
            ProviderForkBoundary::ThroughCheckpoint { checkpoint_id } => {
                let position = turn(checkpoint_id)?;
                Ok(ForkCut::Through(
                    (position + 1..self.chain.len())
                        .find(|&next| is_human_turn_start(self.chain_entry(next)))
                        .map_or(self.chain.len() - 1, |next| next - 1),
                ))
            }
        }
    }

    /// Rows copied into the fork. A fork before the first turn copies only
    /// that prompt, as a dead branch behind an explicit empty leaf.
    fn copied(&self, cut: ForkCut) -> Vec<&Value> {
        match cut {
            ForkCut::Through(end) => self.chain[..=end]
                .iter()
                .map(|&index| &self.entries[index])
                .filter(|entry| entry_type(entry) != Some("progress"))
                .collect(),
            ForkCut::BeforeFirst(position) => vec![self.chain_entry(position)],
        }
    }

    fn title(&self) -> Option<String> {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| match entry_type(entry) {
                Some("custom-title") => entry.get("customTitle").and_then(Value::as_str),
                Some("ai-title") => entry.get("aiTitle").and_then(Value::as_str),
                _ => None,
            })
            .map(str::to_owned)
    }

    /// The Agent SDK `forkSession` copy: fresh uuids, linear parent links,
    /// `forkedFrom` provenance, no file history, then a title row. A fork
    /// before the first turn ends with the CLI's own rewind marker for an
    /// empty conversation (`last-prompt` with an explicit null leaf), which
    /// 2.1.286 resumes (probed) while a metadata-only file is not resumable.
    fn fork_entries(
        &self,
        cut: ForkCut,
        source_session: &str,
        fork_session: &str,
        title: Option<&str>,
    ) -> Result<(Vec<Value>, Vec<(String, String)>), HarnessError> {
        let kept = self.copied(cut);
        if kept.is_empty() {
            return Err(protocol("the Claude session has no messages to fork"));
        }
        let mapping: Vec<(String, String)> = kept
            .iter()
            .map(|entry| {
                (
                    entry_uuid(entry).unwrap_or_default().to_owned(),
                    uuid::Uuid::new_v4().to_string(),
                )
            })
            .collect();
        let lookup: HashMap<&str, &str> = mapping
            .iter()
            .map(|(old, new)| (old.as_str(), new.as_str()))
            .collect();
        let now = iso_now();
        let mut out = Vec::with_capacity(kept.len() + 3);
        for (index, entry) in kept.iter().enumerate() {
            let Value::Object(source) = entry else {
                return Err(protocol("Claude transcript row is not an object"));
            };
            let mut copy = source.clone();
            for key in [
                "teamName",
                "agentName",
                "sessionKind",
                "slug",
                "sourceToolAssistantUUID",
            ] {
                copy.remove(key);
            }
            copy.insert("uuid".into(), json!(mapping[index].1));
            copy.insert(
                "parentUuid".into(),
                index
                    .checked_sub(1)
                    .map_or(Value::Null, |previous| json!(mapping[previous].1)),
            );
            if let Some(logical) = copy.get("logicalParentUuid").and_then(Value::as_str) {
                let mapped = lookup.get(logical).map_or(Value::Null, |id| json!(id));
                copy.insert("logicalParentUuid".into(), mapped);
            }
            copy.insert("sessionId".into(), json!(fork_session));
            copy.insert("isSidechain".into(), json!(false));
            if index + 1 == kept.len() {
                copy.insert("timestamp".into(), json!(now));
            }
            if entry_type(entry) == Some("system")
                && entry.get("subtype").and_then(Value::as_str) == Some("model_refusal_fallback")
            {
                copy.insert("neutralizedByFork".into(), json!(true));
            }
            if let Some(Value::Object(attachment)) = copy.get_mut("attachment") {
                if let Some(mapped) = attachment
                    .get("source_uuid")
                    .and_then(Value::as_str)
                    .and_then(|source| lookup.get(source))
                {
                    attachment.insert("source_uuid".into(), json!(mapped));
                }
                if attachment.get("type").and_then(Value::as_str) == Some("deferred_tools_record")
                    && let Some(Value::Array(names)) = attachment.get_mut("nameOnlyAnnouncements")
                {
                    let remapped = names
                        .iter()
                        .filter_map(Value::as_str)
                        .filter_map(|name| lookup.get(name).map(|id| json!(id)))
                        .collect();
                    *names = remapped;
                }
            }
            copy.insert(
                "forkedFrom".into(),
                json!({ "sessionId": source_session, "messageUuid": mapping[index].0 }),
            );
            out.push(Value::Object(copy));
        }
        if let ForkCut::BeforeFirst(_) = cut {
            out.push(json!({
                "type": "last-prompt", "leafUuid": null, "explicit": true, "rewound": true,
                "sessionId": fork_session,
            }));
        }
        let replacements: Vec<Value> = self
            .entries
            .iter()
            .filter(|entry| entry_type(entry) == Some("content-replacement"))
            .filter_map(|entry| entry.get("replacements").and_then(Value::as_array))
            .flatten()
            .cloned()
            .collect();
        if !replacements.is_empty() {
            out.push(json!({
                "type": "content-replacement", "sessionId": fork_session,
                "replacements": replacements, "uuid": uuid::Uuid::new_v4().to_string(),
                "timestamp": now,
            }));
        }
        if let Some(atis) = self
            .entries
            .iter()
            .rev()
            .find(|entry| entry_type(entry) == Some("atis-latch"))
            .and_then(|entry| entry.get("atis"))
        {
            out.push(json!({ "type": "atis-latch", "sessionId": fork_session, "atis": atis }));
        }
        if let Some(relocated) = self
            .entries
            .iter()
            .rev()
            .find(|entry| entry_type(entry) == Some("relocated"))
            .and_then(|entry| entry.get("relocatedCwd"))
        {
            out.push(json!({
                "type": "relocated", "sessionId": fork_session, "relocatedCwd": relocated,
            }));
        }
        let title = title
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "{} (fork)",
                    self.title().unwrap_or_else(|| "Forked session".into())
                )
            });
        out.push(json!({
            "type": "custom-title", "sessionId": fork_session, "customTitle": title,
            "uuid": uuid::Uuid::new_v4().to_string(), "timestamp": now,
        }));
        Ok((out, mapping))
    }
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`, the transcript's timestamp shape.
fn iso_now() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = elapsed.as_secs() as i64;
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60,
        elapsed.subsec_millis()
    )
}

/// Write a new session file without ever replacing an existing one.
fn write_new_session(
    directory: &Path,
    session_id: &str,
    entries: &[Value],
) -> Result<PathBuf, HarnessError> {
    let target = directory.join(format!("{session_id}.jsonl"));
    let staging = directory.join(format!(".{session_id}.jsonl.luna-fork"));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)?;
        for entry in entries {
            writeln!(file, "{entry}")?;
        }
        file.sync_all()?;
    }
    let linked = std::fs::hard_link(&staging, &target);
    let _ = std::fs::remove_file(&staging);
    linked?;
    Ok(target)
}

// ── control-only CLI process ────────────────────────────────────────────────

#[derive(Debug, Default)]
struct ControlRun {
    initialized: bool,
    /// The `response` object of each request, in order.
    responses: Vec<Value>,
    exit: Option<i32>,
    stderr_tail: String,
}

/// Resume (or load) a session without writing a user message: initialize,
/// send `requests` one at a time, then close stdin. `on_written` fires after
/// each request line is flushed.
async fn run_control(
    executable: &Path,
    scope: &NativeHistoryScope,
    args: &[String],
    file_checkpoints: bool,
    requests: &[Value],
    on_written: Option<&(dyn Fn() + Send + Sync)>,
) -> Result<ControlRun, HarnessError> {
    let mut command = Command::new(executable);
    crate::compose_child_path(&mut command, executable);
    command.args([
        "--print",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        // A control-only process loads no hooks, plugins or MCP servers:
        // history maintenance must not trigger SessionStart side effects.
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        "{\"mcpServers\":{}}",
    ]);
    command.args(args);
    if !scope.cwd.is_empty() {
        command.current_dir(&scope.cwd);
    }
    if let Some(directory) = config_override(scope.provider_home.as_deref()) {
        command.env("CLAUDE_CONFIG_DIR", directory);
    }
    if file_checkpoints {
        command.env(FILE_CHECKPOINT_ENV, "1");
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            HarnessError::NotInstalled(executable.display().to_string())
        } else {
            HarnessError::Io(error)
        }
    })?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        shutdown_child(&mut child, KILL_GRACE).await;
        return Err(protocol("claude child has no stdio"));
    };
    let stderr_task = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            text
        })
    });
    let mut run = ControlRun::default();
    let conversation = async {
        let mut stdin = Some(stdin);
        let mut lines = BufReader::new(stdout).lines();
        let initialize = json!({
            "type": "control_request", "request_id": INIT_REQUEST_ID,
            "request": { "subtype": "initialize" },
        });
        if let Some(writer) = stdin.as_mut() {
            writer
                .write_all(format!("{initialize}\n").as_bytes())
                .await?;
            writer.flush().await?;
        }
        let mut awaiting = INIT_REQUEST_ID.to_string();
        while let Some(line) = lines.next_line().await? {
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if frame.get("type").and_then(Value::as_str) != Some("control_response") {
                continue;
            }
            let response = frame.get("response").cloned().unwrap_or(Value::Null);
            if response.get("request_id").and_then(Value::as_str) != Some(awaiting.as_str()) {
                continue;
            }
            if awaiting == INIT_REQUEST_ID {
                if response.get("subtype").and_then(Value::as_str) == Some("error") {
                    stdin = None;
                    continue;
                }
                run.initialized = true;
            } else {
                run.responses.push(response);
            }
            let next = run.responses.len();
            match (requests.get(next), stdin.as_mut()) {
                (Some(request), Some(writer)) => {
                    awaiting = format!("luna-history-{next}");
                    let line = json!({
                        "type": "control_request", "request_id": awaiting, "request": request,
                    });
                    writer.write_all(format!("{line}\n").as_bytes()).await?;
                    writer.flush().await?;
                    if let Some(written) = on_written {
                        written();
                    }
                }
                _ => {
                    // Closing stdin ends the control-only process.
                    stdin = None;
                    awaiting.clear();
                }
            }
        }
        Ok::<(), std::io::Error>(())
    };
    let outcome = tokio::time::timeout(CONTROL_TIMEOUT, conversation).await;
    match tokio::time::timeout(EXIT_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => run.exit = status.code(),
        _ => shutdown_child(&mut child, KILL_GRACE).await,
    }
    if let Some(task) = stderr_task
        && let Ok(Ok(text)) = tokio::time::timeout(Duration::from_secs(2), task).await
    {
        let tail: String = text.chars().rev().take(600).collect();
        run.stderr_tail = tail.chars().rev().collect::<String>().trim().to_owned();
    }
    match outcome {
        Ok(Ok(())) => Ok(run),
        Ok(Err(error)) => Err(HarnessError::Io(error)),
        Err(_) => Err(protocol("Claude history process timed out")),
    }
}

fn control_payload(response: &Value) -> Result<&Value, String> {
    if response.get("subtype").and_then(Value::as_str) == Some("error") {
        return Err(response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Claude rejected the control request")
            .to_owned());
    }
    Ok(response.get("response").unwrap_or(&Value::Null))
}

fn failed_start(run: &ControlRun, what: &str) -> HarnessError {
    let detail = if run.stderr_tail.is_empty() {
        format!("exit {:?}", run.exit)
    } else {
        run.stderr_tail.clone()
    };
    protocol(format!("Claude could not {what}: {detail}"))
}

// ── operations ──────────────────────────────────────────────────────────────

pub(crate) fn native_turns(scope: &NativeHistoryScope) -> Result<NativeHistoryTurns, HarnessError> {
    let directory = config_dir(scope.provider_home.as_deref())?;
    let path = Transcript::locate(&directory, &scope.cwd, &scope.session_id)?;
    Ok(Transcript::load(&path)?.turns())
}

pub(crate) async fn fork_session(
    executable: &Path,
    request: ProviderForkRequest,
    progress: &ProviderForkProgressReporter,
) -> Result<ProviderForkResult, HarnessError> {
    let directory = config_dir(request.provider_home.as_deref())?;
    let source_path = Transcript::locate(&directory, &request.cwd, &request.source_session_id)?;
    let source = Transcript::load(&source_path)?;
    if source.compacted {
        return Err(protocol(
            "Claude compacted this session; its earlier turns cannot be forked exactly",
        ));
    }
    let end = source.fork_end(&request.boundary)?;
    let fork_id = uuid::Uuid::new_v4().to_string();
    let (entries, mapping) = source.fork_entries(
        end,
        &request.source_session_id,
        &fork_id,
        request.target_name.as_deref(),
    )?;
    // Rows that stay on the fork's active chain (none before the first turn).
    let kept = match end {
        ForkCut::Through(_) => source.copied(end),
        ForkCut::BeforeFirst(_) => Vec::new(),
    };
    let source_turns: HashSet<&str> = kept
        .iter()
        .filter(|entry| is_human_turn_start(entry))
        .filter_map(|entry| entry_uuid(entry))
        .collect();
    let checkpoint_map = mapping
        .iter()
        .filter(|(old, _)| source_turns.contains(old.as_str()))
        .map(|(old, new)| (old.clone(), new.clone()))
        .collect();
    let project = source_path
        .parent()
        .ok_or_else(|| protocol("Claude transcript has no project directory"))?;
    progress(ProviderForkProgress::MutationStarted)?;
    let fork_path = write_new_session(project, &fork_id, &entries)?;
    let verified = async {
        let fork = Transcript::load(&fork_path)?;
        let matches =
            fork.chain.len() == kept.len()
                && fork.chain.iter().zip(&kept).zip(&mapping).all(
                    |((&index, original), (_, new))| {
                        let copy = &fork.entries[index];
                        entry_uuid(copy) == Some(new.as_str())
                            && entry_type(copy) == entry_type(original)
                            && copy.get("message") == original.get("message")
                    },
                );
        if !matches {
            return Err(protocol(
                "the Claude fork transcript did not read back as the source prefix",
            ));
        }
        // `--fork-session` with no user message persists nothing; the
        // anchor check proves the CLI's own loader sees the copied tip.
        let mut args = vec![
            format!("--resume={fork_id}"),
            "--fork-session".into(),
            format!("--session-id={}", uuid::Uuid::new_v4()),
        ];
        if let (ForkCut::Through(_), Some((_, tip))) = (end, mapping.last()) {
            args.push(format!("--resume-session-at={tip}"));
        }
        let run = run_control(
            executable,
            &NativeHistoryScope {
                session_id: fork_id.clone(),
                cwd: request.cwd.clone(),
                provider_home: request.provider_home.clone(),
            },
            &args,
            false,
            &[],
            None,
        )
        .await?;
        if !run.initialized || run.exit != Some(0) {
            return Err(failed_start(&run, "load the forked session"));
        }
        Ok(())
    }
    .await;
    if let Err(error) = verified {
        // The unverified file is LUNA's own new session and nothing is bound
        // to it yet; remove it rather than leave a half-checked fork behind.
        let _ = std::fs::remove_file(&fork_path);
        return Err(error);
    }
    progress(ProviderForkProgress::Acknowledged {
        provider_session_id: fork_id.clone(),
    })?;
    Ok(ProviderForkResult {
        provider_session_id: fork_id,
        checkpoint_map,
    })
}

pub(crate) async fn rewind_conversation(
    executable: &Path,
    request: NativeRewindRequest,
    progress: &NativeRewindProgressReporter,
) -> Result<NativeRewindOutcome, HarnessError> {
    require_uuid(&request.target_checkpoint, "the rewind target")?;
    require_uuid(&request.last_seen_checkpoint, "the newest reviewed turn")?;
    let written = std::sync::atomic::AtomicBool::new(false);
    let on_written = || {
        written.store(true, std::sync::atomic::Ordering::Release);
        progress(NativeRewindProgress::RequestWritten);
    };
    let run = run_control(
        executable,
        &request.scope,
        &[format!("--resume={}", request.scope.session_id)],
        false,
        &[json!({
            "subtype": "rewind_conversation",
            "target_message_uuid": request.target_checkpoint,
            "last_seen_user_message_uuid": request.last_seen_checkpoint,
        })],
        Some(&on_written as &(dyn Fn() + Send + Sync)),
    )
    .await?;
    if !run.initialized {
        return Err(failed_start(&run, "resume the session for rewind"));
    }
    let Some(response) = run.responses.first() else {
        return Err(protocol(
            if written.load(std::sync::atomic::Ordering::Acquire) {
                "Claude ended before answering the rewind; its outcome is unknown"
            } else {
                "Claude ended before the rewind was sent"
            },
        ));
    };
    let payload = match control_payload(response) {
        Ok(payload) => payload,
        Err(message) => {
            return Ok(NativeRewindOutcome::Refused {
                reason: "error".into(),
                message,
            });
        }
    };
    if payload.get("rewound").and_then(Value::as_bool) == Some(true) {
        return Ok(NativeRewindOutcome::Rewound);
    }
    let reason = payload
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let message = payload
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("Claude did not rewind")
        .to_owned();
    if REWIND_REFUSALS.contains(&reason.as_str()) {
        Ok(NativeRewindOutcome::Refused { reason, message })
    } else {
        // `state_changed`, `persist_failed` and unknown reasons may have
        // touched the transcript: reconcile by readback instead.
        Err(protocol(format!(
            "Claude rewind outcome is uncertain ({reason}: {message})"
        )))
    }
}

pub(crate) async fn rewind_files(
    executable: &Path,
    request: NativeFileRewindRequest,
) -> Result<NativeFileRewindResult, HarnessError> {
    require_uuid(&request.checkpoint, "the file checkpoint")?;
    let run = run_control(
        executable,
        &request.scope,
        &[format!("--resume={}", request.scope.session_id)],
        true,
        &[json!({
            "subtype": "rewind_files",
            "user_message_id": request.checkpoint,
            "dry_run": request.dry_run,
        })],
        None,
    )
    .await?;
    if !run.initialized {
        return Err(failed_start(
            &run,
            "resume the session for file checkpoints",
        ));
    }
    let Some(response) = run.responses.first() else {
        return Err(protocol(if request.dry_run {
            "Claude ended before previewing file checkpoints"
        } else {
            "Claude ended before confirming the file restore; preview again to see the current state"
        }));
    };
    let payload = match control_payload(response) {
        Ok(payload) => payload,
        Err(message) => {
            return Ok(NativeFileRewindResult {
                can_rewind: false,
                error: Some(message),
                ..Default::default()
            });
        }
    };
    Ok(NativeFileRewindResult {
        can_rewind: payload.get("canRewind").and_then(Value::as_bool) == Some(true),
        error: payload
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned),
        files_changed: payload
            .get("filesChanged")
            .and_then(Value::as_array)
            .map(|files| {
                files
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        insertions: payload
            .get("insertions")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        deletions: payload
            .get("deletions")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        skipped_links: payload.get("skippedLinks").and_then(Value::as_u64),
    })
}
