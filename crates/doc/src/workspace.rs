//! Workspace doc schema over `loro` — the per-org entity index that replaced luna's
//! residual entity sync (ARCHITECTURE.md §2.2). Lived in its own DO room (same
//! SessionRoom class, doc id `ws/{orgId}`).
//!
//! Legacy: the row-table registry ([`crate::registry`]) replaced this doc. The
//! engine only reads an old local snapshot once ([`WorkspaceDoc::read_all`]) to
//! seed the registry; the `upsert_*` writers remain to build such snapshots.
//!
//! Container layout — maps keyed by id, NOT lists: entity rows are LWW upserts, and a
//! map-of-maps means concurrent writers to *different* rows never conflict while writes
//! to the *same* row settle field-by-field LWW (exactly right for renames/archives):
//! - `devices`: LoroMap keyed by deviceId → row map {id, name, platform, lastSeenAt}
//! - `spaces`: LoroMap keyed by spaceId → row map {id, deviceId, path, name?,
//!   gitDetected, gitCheckedAt?, checkoutId?, createdAt}
//! - `chats`: LoroMap keyed by chatId → row map {id, deviceId, title?, archived, cwd?,
//!   branch?, checkoutId?, config?(json), lastMessagePreview?, lastMessageAt?, createdAt,
//!   harnessSessionId?, harnessSessionCwd?, spaceId?, lastSeenAt?}
//! - `sessions`: LoroMap keyed by chatId → row map {chatId, deviceId, status, startedAt?,
//!   updatedAt}
//! - `meta`: LoroMap {schemaVersion} — in-band detection for future destructive changes
//!
//! Writer discipline (ARCHITECTURE §2.2): each device writes its own device row, its
//! own session rows, and rows for chats it hosts; title/archived renames are LWW map
//! sets from any device — matching luna's Mutate surface. Presence rides the room's
//! `EphemeralStore` under keys `presence/{deviceId}` (an online timestamp), replacing
//! luna's 15s heartbeat writes so liveness never grows the oplog.
//!
//! Timestamps are stored as epoch millis (the session-doc convention) and surface as
//! `chrono::DateTime<Utc>` through the `luna_proto` entity types.

use chrono::{DateTime, Utc};
use loro::{ExportMode, LoroDoc, LoroMap, LoroValue, ToJson};
use serde::{Deserialize, Serialize};

use luna_proto::{Chat, ChatConfig, Device, Session, SessionStatus, Space};

use crate::schema::DocError;

/// Ephemeral presence key for a device (`presence/{deviceId}` → online timestamp).
pub fn presence_key(device_id: &str) -> String {
    format!("presence/{device_id}")
}

/// Everything in the workspace doc, materialized (`read_all`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceState {
    pub devices: Vec<Device>,
    pub spaces: Vec<Space>,
    pub chats: Vec<Chat>,
    pub sessions: Vec<Session>,
}

/// Result of a `delete_space` cascade — the chat ids removed alongside the
/// space so the engine can drop local run state / doc-host handles.
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedSpace {
    pub existed: bool,
    pub chat_ids: Vec<String>,
}

/// A workspace doc handle: typed access over a LoroDoc with the schema above.
pub struct WorkspaceDoc {
    doc: LoroDoc,
}

impl Default for WorkspaceDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceDoc {
    /// Fresh, empty workspace doc.
    pub fn new() -> Self {
        Self {
            doc: LoroDoc::new(),
        }
    }

    /// Wrap an existing doc (e.g. imported from a snapshot).
    pub fn from_doc(doc: LoroDoc) -> Self {
        Self { doc }
    }

    /// Export a snapshot (persistence) — `ExportMode::Snapshot`.
    pub fn export_snapshot(&self) -> Result<Vec<u8>, DocError> {
        self.doc
            .export(ExportMode::Snapshot)
            .map_err(|e| DocError::Schema(e.to_string()))
    }

    // ── devices ─────────────────────────────────────────────────────────────

    /// Upsert a full device row (writer discipline: callers pass their OWN device).
    pub fn upsert_device(&self, device: &Device) -> Result<(), DocError> {
        let row = self.row("devices", &device.id)?;
        row.insert("id", device.id.as_str())?;
        row.insert("name", device.name.as_str())?;
        row.insert("platform", device.platform.as_str())?;
        set_opt_ms(&row, "lastSeenAt", device.last_seen_at)?;
        set_opt_ms(&row, "createdAt", device.created_at)?;
        set_opt_str(&row, "version", device.version.as_deref())?;
        row.insert(
            "capabilities",
            crate::schema::loro_value_from_json(&serde_json::json!(&device.capabilities)),
        )?;
        self.doc.commit();
        Ok(())
    }

    pub fn read_devices(&self) -> Result<Vec<Device>, DocError> {
        let mut devices: Vec<Device> = self
            .read_rows::<RawDevice>("devices")?
            .into_iter()
            .map(Device::from)
            .collect();
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(devices)
    }

    // ── spaces ──────────────────────────────────────────────────────────────

    /// Upsert a full space row (creation from any device; owner-only fields are
    /// enforced one layer up, in the engine).
    pub fn upsert_space(&self, space: &Space) -> Result<(), DocError> {
        let row = self.row("spaces", &space.id)?;
        row.insert("id", space.id.as_str())?;
        row.insert("deviceId", space.device_id.as_str())?;
        row.insert("path", space.path.as_str())?;
        set_opt_str(&row, "name", space.name.as_deref())?;
        row.insert("gitDetected", space.git_detected)?;
        set_opt_ms(&row, "gitCheckedAt", space.git_checked_at)?;
        set_opt_str(&row, "checkoutId", space.checkout_id.as_deref())?;
        row.insert(
            "executionDefaults",
            LoroValue::from(serde_json::to_value(&space.execution_defaults)?),
        )?;
        row.insert("createdAt", space.created_at.timestamp_millis())?;
        set_opt_ms(&row, "removedAt", space.removed_at)?;
        self.doc.commit();
        Ok(())
    }

    pub fn read_spaces(&self) -> Result<Vec<Space>, DocError> {
        let mut spaces: Vec<Space> = self
            .read_rows::<RawSpace>("spaces")?
            .into_iter()
            .map(Space::from)
            .collect();
        spaces.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(spaces)
    }

    // ── chats ───────────────────────────────────────────────────────────────

    /// Upsert a full chat row (host device, or CreateChat targeting a device).
    pub fn upsert_chat(&self, chat: &Chat) -> Result<(), DocError> {
        let row = self.row("chats", &chat.id)?;
        match &chat.child_thread {
            Some(child) => {
                row.insert("childThread", LoroValue::from(serde_json::to_value(child)?))?
            }
            None => row.delete("childThread")?,
        }
        row.insert("id", chat.id.as_str())?;
        row.insert("deviceId", chat.device_id.as_str())?;
        set_opt_str(&row, "title", chat.title.as_deref())?;
        row.insert("archived", chat.archived)?;
        match &chat.settlement_override {
            Some(settlement) => row.insert(
                "settlementOverride",
                LoroValue::from(serde_json::to_value(settlement)?),
            )?,
            None => row.delete("settlementOverride")?,
        }
        match chat.pinned_order.as_deref() {
            Some(order) => row.insert("pinnedOrder", order)?,
            None => row.delete("pinnedOrder")?,
        }
        set_opt_str(&row, "cwd", chat.cwd.as_deref())?;
        set_opt_str(&row, "branch", chat.branch.as_deref())?;
        set_opt_str(&row, "checkoutId", chat.checkout_id.as_deref())?;
        match &chat.source_context {
            Some(context) => row.insert(
                "sourceContext",
                LoroValue::from(serde_json::to_value(context)?),
            )?,
            None => row.delete("sourceContext")?,
        }
        match &chat.config {
            Some(config) => row.insert("config", LoroValue::from(serde_json::to_value(config)?))?,
            None => row.delete("config")?,
        }
        set_opt_str(
            &row,
            "lastMessagePreview",
            chat.last_message_preview.as_deref(),
        )?;
        set_opt_ms(&row, "lastMessageAt", chat.last_message_at)?;
        row.insert("createdAt", chat.created_at.timestamp_millis())?;
        // Preserved on full-row upserts (set_chat_activity/set_chat_host read →
        // modify → upsert; dropping these here would silently amnesia the chat).
        set_opt_str(&row, "harnessSessionId", chat.harness_session_id.as_deref())?;
        set_opt_str(
            &row,
            "harnessSessionCwd",
            chat.harness_session_cwd.as_deref(),
        )?;
        set_opt_str(&row, "spaceId", chat.space_id.as_deref())?;
        set_opt_ms(&row, "lastSeenAt", chat.last_seen_at)?;
        row.insert("manuallyUnread", chat.manually_unread)?;
        self.doc.commit();
        Ok(())
    }

    pub fn read_chats(&self) -> Result<Vec<Chat>, DocError> {
        let mut chats: Vec<Chat> = self
            .read_rows::<RawChat>("chats")?
            .into_iter()
            .map(Chat::from)
            .collect();
        chats.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(chats)
    }

    // ── sessions ────────────────────────────────────────────────────────────

    /// Upsert a session-status row (writer discipline: each device writes only its
    /// own runs' rows). Staleness is checked client-side against `updatedAt`.
    pub fn upsert_session(&self, session: &Session) -> Result<(), DocError> {
        let row = self.row("sessions", &session.chat_id)?;
        row.insert("chatId", session.chat_id.as_str())?;
        row.insert("deviceId", session.device_id.as_str())?;
        row.insert("status", status_str(session.status))?;
        set_opt_str(
            &row,
            "lastCompletedTurn",
            session.last_completed_turn.as_deref(),
        )?;
        set_opt_ms(&row, "startedAt", session.started_at)?;
        row.insert("updatedAt", session.updated_at.timestamp_millis())?;
        self.doc.commit();
        Ok(())
    }

    pub fn read_sessions(&self) -> Result<Vec<Session>, DocError> {
        let mut sessions: Vec<Session> = self
            .read_rows::<RawSession>("sessions")?
            .into_iter()
            .map(Session::from)
            .collect();
        sessions.sort_by(|a, b| a.chat_id.cmp(&b.chat_id));
        Ok(sessions)
    }

    // ── whole-doc read ──────────────────────────────────────────────────────

    pub fn read_all(&self) -> Result<WorkspaceState, DocError> {
        Ok(WorkspaceState {
            devices: self.read_devices()?,
            spaces: self.read_spaces()?,
            chats: self.read_chats()?,
            sessions: self.read_sessions()?,
        })
    }

    // ── row plumbing ────────────────────────────────────────────────────────

    /// The row map for `key`, creating it when absent.
    fn row(&self, container: &str, key: &str) -> Result<LoroMap, DocError> {
        let parent = self.doc.get_map(container);
        match parent.get(key) {
            Some(loro::ValueOrContainer::Container(loro::Container::Map(map))) => Ok(map),
            _ => Ok(parent.insert_container(key, LoroMap::new())?),
        }
    }

    /// All rows of a container as typed values (malformed rows are skipped with a
    /// warning rather than failing the whole read — a bad peer must not blind us).
    fn read_rows<T: serde::de::DeserializeOwned>(
        &self,
        container: &str,
    ) -> Result<Vec<T>, DocError> {
        let value = self.doc.get_map(container).get_deep_value().to_json_value();
        let serde_json::Value::Object(rows) = value else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(rows.len());
        for (key, row) in rows {
            match serde_json::from_value::<T>(row) {
                Ok(parsed) => out.push(parsed),
                Err(err) => {
                    tracing::warn!(container, row = %key, error = %err, "skipping malformed workspace row");
                }
            }
        }
        Ok(out)
    }
}

fn set_opt_str(row: &LoroMap, key: &str, value: Option<&str>) -> Result<(), DocError> {
    match value {
        Some(v) => row.insert(key, v)?,
        None => row.delete(key)?,
    }
    Ok(())
}

fn set_opt_ms(row: &LoroMap, key: &str, value: Option<DateTime<Utc>>) -> Result<(), DocError> {
    match value {
        Some(at) => row.insert(key, at.timestamp_millis())?,
        None => row.delete(key)?,
    }
    Ok(())
}

fn status_str(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Idle => "idle",
        SessionStatus::Working => "working",
        SessionStatus::AwaitingApproval => "awaitingApproval",
        SessionStatus::AwaitingInput => "awaitingInput",
        SessionStatus::Errored => "errored",
    }
}

fn dt(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or(DateTime::UNIX_EPOCH)
}

// ── doc-resident row shapes (epoch-millis timestamps) ───────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawDevice {
    id: String,
    name: String,
    platform: String,
    #[serde(default)]
    last_seen_at: Option<i64>,
    #[serde(default)]
    created_at: Option<i64>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    capabilities: Vec<String>,
}

impl From<RawDevice> for Device {
    fn from(raw: RawDevice) -> Self {
        Device {
            id: raw.id,
            name: raw.name,
            platform: raw.platform,
            last_seen_at: raw.last_seen_at.map(dt),
            created_at: raw.created_at.map(dt),
            version: raw.version,
            capabilities: raw.capabilities,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawSpace {
    #[serde(default)]
    execution_defaults: luna_proto::ProjectExecutionDefaults,
    id: String,
    device_id: String,
    path: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    git_detected: bool,
    #[serde(default)]
    git_checked_at: Option<i64>,
    #[serde(default)]
    checkout_id: Option<String>,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    removed_at: Option<i64>,
}

impl From<RawSpace> for Space {
    fn from(raw: RawSpace) -> Self {
        Space {
            execution_defaults: raw.execution_defaults,
            id: raw.id,
            device_id: raw.device_id,
            path: raw.path,
            name: raw.name,
            git_detected: raw.git_detected,
            git_checked_at: raw.git_checked_at.map(dt),
            checkout_id: raw.checkout_id,
            created_at: dt(raw.created_at),
            removed_at: raw.removed_at.map(dt),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawChat {
    #[serde(default)]
    child_thread: Option<luna_proto::ChildThread>,
    id: String,
    device_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    settlement_override: Option<luna_proto::ChatSettlementOverride>,
    #[serde(default, deserialize_with = "luna_proto::deserialize_pin_order")]
    pinned_order: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    checkout_id: Option<String>,
    #[serde(default)]
    source_context: Option<luna_proto::ConversationSourceContext>,
    /// LENIENT: a config this build can't decode (a harness/reasoning/sandbox
    /// id from a NEWER peer — field incident: pre-v0.2.10 laptops dropped
    /// every `"opencode"` chat row wholesale, so new sessions silently never
    /// appeared in the sidebar) degrades to `None` instead of failing the
    /// row. The chat stays visible and selectable with generic defaults;
    /// up-to-date devices still see the real config.
    #[serde(default, deserialize_with = "lenient_chat_config")]
    config: Option<ChatConfig>,
    #[serde(default)]
    last_message_preview: Option<String>,
    #[serde(default)]
    last_message_at: Option<i64>,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    harness_session_id: Option<String>,
    #[serde(default)]
    harness_session_cwd: Option<String>,
    #[serde(default)]
    space_id: Option<String>,
    #[serde(default)]
    last_seen_at: Option<i64>,
    #[serde(default)]
    manually_unread: bool,
    #[serde(default)]
    room_gen: Option<u32>,
}

/// Decode a chat row's `config` leniently: unknown enum values (a newer
/// peer's harness id, reasoning level or sandbox mode) cost the CONFIG, not
/// the row. Mirrors the transcript salvage rule: a missing field must cost
/// at most what the field carried.
fn lenient_chat_config<'de, D>(deserializer: D) -> Result<Option<ChatConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| match serde_json::from_value::<ChatConfig>(value) {
        Ok(config) => Some(config),
        Err(err) => {
            tracing::warn!(error = %err, "chat config from a newer peer; showing the row without it");
            None
        }
    }))
}

impl From<RawChat> for Chat {
    fn from(raw: RawChat) -> Self {
        Chat {
            child_thread: raw.child_thread,
            id: raw.id,
            device_id: raw.device_id,
            title: raw.title,
            archived: raw.archived,
            settlement_override: raw.settlement_override,
            pinned_order: raw.pinned_order,
            cwd: raw.cwd,
            branch: raw.branch,
            checkout_id: raw.checkout_id,
            source_context: raw.source_context,
            config: raw.config,
            last_message_preview: raw.last_message_preview,
            last_message_at: raw.last_message_at.map(dt),
            created_at: dt(raw.created_at),
            harness_session_id: raw.harness_session_id,
            harness_session_cwd: raw.harness_session_cwd,
            space_id: raw.space_id,
            last_seen_at: raw.last_seen_at.map(dt),
            manually_unread: raw.manually_unread,
            room_gen: raw.room_gen,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawSession {
    #[serde(default)]
    last_completed_turn: Option<String>,
    chat_id: String,
    device_id: String,
    status: SessionStatus,
    #[serde(default)]
    started_at: Option<i64>,
    #[serde(default)]
    updated_at: i64,
    #[serde(default)]
    recovery: Option<luna_proto::TaskRecovery>,
}

impl From<RawSession> for Session {
    fn from(raw: RawSession) -> Self {
        Session {
            last_completed_turn: raw.last_completed_turn,
            chat_id: raw.chat_id,
            device_id: raw.device_id,
            status: raw.status,
            started_at: raw.started_at.map(dt),
            updated_at: dt(raw.updated_at),
            recovery: raw.recovery,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luna_proto::{HarnessId, SandboxLevel};

    fn ts(ms: i64) -> DateTime<Utc> {
        dt(ms)
    }

    fn device(id: &str, name: &str) -> Device {
        Device {
            id: id.into(),
            name: name.into(),
            platform: "linux".into(),
            last_seen_at: Some(ts(1_000)),
            created_at: Some(ts(500)),
            version: Some("0.1.0".into()),
            capabilities: Vec::new(),
        }
    }

    fn chat(id: &str, device_id: &str) -> Chat {
        Chat {
            child_thread: None,
            id: id.into(),
            device_id: device_id.into(),
            title: Some("First chat".into()),
            archived: false,
            settlement_override: None,
            pinned_order: None,
            cwd: Some("/tmp/repo".into()),
            branch: Some("main".into()),
            checkout_id: None,
            source_context: None,
            config: Some(ChatConfig {
                harness: HarnessId::Mock,
                provider_account_id: None,
                model: Some("mock-1".into()),
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
                permission_mode: luna_proto::PermissionMode::Ask,
                effective_permission_mode: luna_proto::PermissionMode::Ask,
                permission_ceiling_source: None,
                permission_degradation_reason: None,
                permission_audit: Vec::new(),
            }),
            last_message_preview: None,
            last_message_at: None,
            created_at: ts(2_000),
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            last_seen_at: None,
            manually_unread: false,
            room_gen: None,
        }
    }

    fn session(chat_id: &str, device_id: &str, status: SessionStatus) -> Session {
        Session {
            last_completed_turn: None,
            chat_id: chat_id.into(),
            device_id: device_id.into(),
            status,
            started_at: Some(ts(3_000)),
            updated_at: ts(3_500),
            recovery: None,
        }
    }

    #[test]
    fn completion_marker_survives_workspace_sync_and_legacy_rows() {
        let ws = WorkspaceDoc::new();
        let mut row = session("chat-1", "dev-a", SessionStatus::Idle);
        row.last_completed_turn = Some("turn-one".into());
        ws.upsert_session(&row).unwrap();
        assert_eq!(ws.read_sessions().unwrap(), vec![row.clone()]);
        row.status = SessionStatus::Working;
        ws.upsert_session(&row).unwrap();
        assert_eq!(ws.read_sessions().unwrap(), vec![row]);
        ws.row("sessions", "chat-1")
            .unwrap()
            .delete("lastCompletedTurn")
            .unwrap();
        assert_eq!(ws.read_sessions().unwrap()[0].last_completed_turn, None);
    }

    #[test]
    fn rows_round_trip() {
        let ws = WorkspaceDoc::new();
        let mut device = device("dev-a", "laptop");
        device.capabilities = vec![luna_proto::capabilities::MESSAGE_QUEUE_V1.into()];
        ws.upsert_device(&device).unwrap();
        ws.upsert_chat(&chat("chat-1", "dev-a")).unwrap();
        ws.upsert_session(&session("chat-1", "dev-a", SessionStatus::Working))
            .unwrap();

        let state = ws.read_all().unwrap();
        assert_eq!(state.devices, vec![device]);
        assert_eq!(state.chats, vec![chat("chat-1", "dev-a")]);
        assert_eq!(
            state.sessions,
            vec![session("chat-1", "dev-a", SessionStatus::Working)]
        );

        // Upsert refreshes in place — no duplicate rows, cleared options removed.
        let mut updated = chat("chat-1", "dev-a");
        updated.title = None;
        updated.last_message_preview = Some("hello".into());
        updated.last_message_at = Some(ts(9_000));
        ws.upsert_chat(&updated).unwrap();
        let chats = ws.read_chats().unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].title, None);
        assert_eq!(chats[0].last_message_preview.as_deref(), Some("hello"));
    }
}
