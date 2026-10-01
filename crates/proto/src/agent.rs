//! Agent-side wire types: harness identity, run requests, streaming events, tool calls.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessId {
    ClaudeCode,
    Codex,
    /// Legacy wire value, retained only until old LUNA task rows are retired.
    Cursor,
    /// Legacy wire value, retained only until old LUNA task rows are retired.
    Devin,
    /// Legacy wire value, retained only until old LUNA task rows are retired.
    Grok,
    /// Legacy wire value, retained only until old LUNA task rows are retired.
    Hermes,
    /// Legacy wire value, retained only until old LUNA task rows are retired.
    Pi,
    /// SST's opencode agent, driven natively over its own HTTP/SSE server
    /// protocol (`opencode serve` — the same wire the opencode desktop app
    /// speaks).
    Opencode,
    /// Legacy wire value, retained only until old LUNA task rows are retired.
    Antigravity,
    /// Test harness; never shown in production pickers.
    Mock,
}

impl HarnessId {
    /// Old ids remain decodable until their LUNA task rows have been retired.
    /// They have no runnable integration in the current engine.
    pub fn is_retired(self) -> bool {
        !matches!(
            self,
            Self::ClaudeCode | Self::Codex | Self::Opencode | Self::Mock
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Ultra,
    /// xhigh + harness-specific setting.
    Ultracode,
    /// Prompt-prefix driven (Claude).
    Ultrathink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxLevel {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

/// User-facing execution permission ladder. Ordering is intentionally the
/// privilege ordering so policy ceilings can clamp with `min`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    #[default]
    Ask,
    Auto,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionScope {
    Workspace,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalReviewer {
    User,
    Automatic,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionEscalation {
    Ask,
    Deny,
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionNetworkPolicy {
    #[default]
    ProviderDefault,
    Restricted,
    Unrestricted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionCeilingSource {
    ParentThread,
    Machine,
    Environment,
    Provider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionDegradationReason {
    ParentCeiling,
    MachineCeiling,
    EnvironmentCeiling,
    ProviderUnsupported,
    LegacyAmbiguous,
}

/// Engine-resolved execution policy. Adapters consume this value; the legacy
/// `sandbox`/`autoApprove` fields remain only for backwards wire compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionPolicy {
    pub requested_mode: PermissionMode,
    pub effective_mode: PermissionMode,
    pub scope: PermissionScope,
    pub reviewer: ApprovalReviewer,
    pub escalation: PermissionEscalation,
    #[serde(default)]
    pub network_policy: PermissionNetworkPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_roots: Vec<String>,
    pub machine_name: String,
    pub provider_mapping: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling_source: Option<PermissionCeilingSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degradation_reason: Option<PermissionDegradationReason>,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        Self {
            requested_mode: PermissionMode::Ask,
            effective_mode: PermissionMode::Ask,
            scope: PermissionScope::Workspace,
            reviewer: ApprovalReviewer::User,
            escalation: PermissionEscalation::Ask,
            network_policy: PermissionNetworkPolicy::ProviderDefault,
            workspace_roots: Vec::new(),
            machine_name: String::new(),
            provider_mapping: "legacy-safe-ask".into(),
            ceiling_source: None,
            degradation_reason: Some(PermissionDegradationReason::LegacyAmbiguous),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPermissionCapabilities {
    pub supported_modes: Vec<PermissionMode>,
    pub supports_workspace_sandbox: bool,
    pub supports_automatic_reviewer: bool,
    pub supports_live_mode_change: bool,
    pub supports_task_allowance: bool,
    pub approval_kinds: Vec<ApprovalActionKind>,
    pub network_control: String,
    pub known_limitations: Vec<String>,
}

impl ProviderPermissionCapabilities {
    pub fn supports(&self, mode: PermissionMode) -> bool {
        self.supported_modes.contains(&mode)
    }
}

impl Default for ProviderPermissionCapabilities {
    fn default() -> Self {
        Self {
            supported_modes: Vec::new(),
            supports_workspace_sandbox: false,
            supports_automatic_reviewer: false,
            supports_live_mode_change: false,
            supports_task_allowance: false,
            approval_kinds: Vec::new(),
            network_control: "unverified".into(),
            known_limitations: vec!["Permission capabilities were not declared.".into()],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalActionKind {
    Command,
    FileChange,
    Network,
    ExternalPath,
    Tool,
    Other,
    /// Engine-owned marker for a LUNA cross-task approval. Existing approval
    /// kinds remain provider approvals when older records are deserialized.
    LunaCrossTask,
}

/// `ApprovalRequest::expires_at_ms` for a provider request that carries no
/// expiry of its own (Codex app-server v2 approvals and elicitations). The
/// request waits until the user answers, the provider resolves it, the turn
/// ends or the session closes. Out of chrono's range, so it never renders as
/// a clock time.
pub const APPROVAL_NO_EXPIRY: i64 = i64::MAX;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub provider_request_id: String,
    pub provider: HarnessId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub machine_name: String,
    pub action_kind: ApprovalActionKind,
    pub summary: String,
    pub requested_scope: PermissionScope,
    pub expires_at_ms: i64,
    pub supports_task_allowance: bool,
    /// Optional provider context and exact advertised decision choices. Older
    /// provider records retain the legacy approval path when this is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<ApprovalDetails>,
}

/// Display-only metadata. Grants/rules are held by the adapter, never accepted
/// from a client-supplied copy of this structure.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDetails {
    pub provider_thread_id: Option<String>,
    pub item_id: Option<String>,
    pub approval_id: Option<String>,
    pub kind: Option<String>,
    pub environment_id: Option<String>,
    pub cwd: Option<String>,
    pub command: Option<String>,
    pub reason: Option<String>,
    pub command_actions: Option<serde_json::Value>,
    pub file_changes: Option<serde_json::Value>,
    pub grant_root: Option<String>,
    pub network: Option<ApprovalNetworkContext>,
    pub permissions: Option<serde_json::Value>,
    pub proposed_exec_policy_amendment: Option<serde_json::Value>,
    #[serde(default)]
    pub proposed_network_policy_amendments: Vec<serde_json::Value>,
    #[serde(default)]
    pub choices: Vec<ApprovalChoice>,
    /// Native sub-agent (child provider thread) that raised the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
    /// MCP/app tool-call approval context, as the provider rendered it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ApprovalToolContext>,
}

/// Display-only MCP tool-call approval context (Codex `_meta` keys).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalToolContext {
    pub server: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `(display name, rendered value)` in provider order.
    #[serde(default)]
    pub arguments: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalNetworkContext {
    pub host: String,
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalChoice {
    pub label: String,
    /// Native lifetime, e.g. request, turn, session, or persistent policy.
    pub scope: String,
    pub description: String,
}

impl ApprovalDetails {
    /// Shared rendering for the composer and side-chat panels. JSON context
    /// remains exact so filesystem deny entries and host rules stay visible.
    pub fn display_rows(&self) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        if let Some(subagent) = &self.subagent {
            rows.push(("Requested by sub-agent".into(), subagent.clone()));
        }
        if let Some(tool) = &self.tool {
            if let Some(name) = &tool.tool {
                rows.push(("Tool".into(), name.clone()));
            }
            rows.push((
                "MCP server".into(),
                match &tool.connector {
                    Some(connector) => format!("{connector} ({})", tool.server),
                    None => tool.server.clone(),
                },
            ));
            if let Some(description) = &tool.description {
                rows.push(("Tool description".into(), description.clone()));
            }
            for (name, value) in &tool.arguments {
                rows.push((format!("Argument · {name}"), value.clone()));
            }
        }
        for (label, value) in [
            ("Action", self.kind.as_ref()),
            ("Command / input", self.command.as_ref()),
            ("Working directory", self.cwd.as_ref()),
            ("Environment", self.environment_id.as_ref()),
            ("Reason", self.reason.as_ref()),
            ("Provider thread", self.provider_thread_id.as_ref()),
            ("Item", self.item_id.as_ref()),
            ("Approval callback", self.approval_id.as_ref()),
            ("Proposed write root", self.grant_root.as_ref()),
        ] {
            if let Some(value) = value {
                rows.push((label.into(), value.clone()));
            }
        }
        if let Some(network) = &self.network {
            rows.push((
                "Network host".into(),
                format!("{} ({})", network.host, network.protocol),
            ));
        }
        for (label, value) in [
            ("Parsed command actions", self.command_actions.as_ref()),
            ("File changes", self.file_changes.as_ref()),
            (
                "Proposed command rule",
                self.proposed_exec_policy_amendment.as_ref(),
            ),
        ] {
            if let Some(value) = value {
                rows.push((
                    label.into(),
                    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
                ));
            }
        }
        if let Some(permissions) = &self.permissions {
            if let Some(categories) = permissions.as_object() {
                for (category, value) in categories {
                    let label = match category.as_str() {
                        "network" => "Network permissions",
                        "fileSystem" => "Filesystem permissions",
                        _ => category,
                    };
                    if !value.is_null() {
                        rows.push((
                            label.into(),
                            serde_json::to_string_pretty(value)
                                .unwrap_or_else(|_| value.to_string()),
                        ));
                    }
                }
            } else {
                rows.push(("Requested permissions".into(), permissions.to_string()));
            }
        }
        for (index, value) in self.proposed_network_policy_amendments.iter().enumerate() {
            rows.push((
                format!("Proposed network rule {}", index + 1),
                value.to_string(),
            ));
        }
        rows
    }
}

impl ApprovalRequest {
    /// The provider's deadline, or `None` when the request waits for an answer.
    pub fn expires(&self) -> Option<i64> {
        (self.expires_at_ms != APPROVAL_NO_EXPIRY).then_some(self.expires_at_ms)
    }

    /// Persisted marker for recovery only. This client-visible value cannot
    /// authorize a live approval; SessionsEngine keeps that kind separately.
    pub fn is_luna_cross_task(&self) -> bool {
        self.action_kind == ApprovalActionKind::LunaCrossTask
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision {
    AllowOnce,
    AllowTask,
    Deny,
    Cancel,
    /// Index in the authoritative pending request's advertised choices. The
    /// client cannot submit permission objects or policy amendment payloads.
    ProviderChoice {
        index: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SteeringMode {
    /// Steer delivered at the next step boundary within the live turn.
    StepBoundary,
    /// Steer delivered only between turns.
    TurnBoundary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub label: String,
    /// Short tagline rendered under the name in the model picker (11px muted),
    /// mirroring the Electron app's `ModelInfo.description`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub reasoning_levels: Vec<ReasoningLevel>,
    #[serde(default)]
    pub options: Vec<ModelOption>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    pub id: String,
    pub label: String,
    pub choices: Vec<ModelOptionChoice>,
    pub default_choice: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOptionChoice {
    pub id: String,
    pub label: String,
}

/// Native selections keep their provider identity independent of composer labels.
/// Images use the existing staged attachment transport, never sender-local paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ComposerInput {
    Mention { name: String, path: String },
    Skill { name: String, path: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    /// A native Codex review, dispatched through review/start rather than an ordinary turn.
    /// Separate reviews first create an ordinary LUNA chat and a fresh native thread;
    /// the adapter always runs inline in that chat's own provider binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<NativeReviewRequest>,
    /// "Send as goal" (Codex only): start a native goal instead of an ordinary
    /// turn. The adapter loads the thread, runs Codex's clear+set sequence and
    /// adopts the turn Codex starts itself. Never replayed by recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<crate::GoalStartRequest>,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_items: Vec<ComposerInput>,
    /// The harness picked at send time. Rides the command plane so
    /// claim-on-first-command (chat row still in flight on the registry
    /// channel) dispatches — and records — the picked harness instead of the
    /// engine default. Additive + serde-defaulted for wire compat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessId>,
    /// Chat-selected provider authentication metadata. The engine resolves it
    /// to a local execution home; clients never send a filesystem path or token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_account_id: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<ReasoningLevel>,
    /// Harness-specific option selections (option id -> choice id), JSON round-tripped.
    #[serde(default)]
    pub model_options: serde_json::Map<String, serde_json::Value>,
    pub cwd: String,
    pub sandbox: SandboxLevel,
    #[serde(default)]
    pub auto_approve: bool,
    /// Canonical requested mode. Missing legacy records decode to Ask; legacy
    /// sandbox/autoApprove combinations are never interpreted as Full.
    #[serde(default)]
    pub permission_mode: PermissionMode,
    /// Child work can request less privilege but never exceed this mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_permission_ceiling: Option<PermissionMode>,
    /// Optional environment-specific ceiling carried over RPC/remote paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_permission_ceiling: Option<PermissionMode>,
    /// Filled authoritatively by the engine immediately before dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_policy: Option<PermissionPolicy>,
    /// Harness-native session id to resume, if any.
    pub resume: Option<String>,
    /// Absolute paths of image attachments already staged on the run device
    /// (composer uploads: UploadChunk/UploadCommit → durable path). The same
    /// paths also ride the prompt text as `Attached images (local files …)`
    /// refs (luna's `withAttachments` transport — that's what persists in the
    /// doc); this field additionally lets a harness send native image input
    /// (Codex localImage) or inline image content blocks. Additive + serde-defaulted for wire compat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    /// Host-side isolated-worktree creation (see [`WorktreeSpec`]): when set,
    /// the HOST materializes the worktree at command-drain time and runs there
    /// instead of `cwd`. Additive + serde-defaulted for wire compat — an old
    /// host ignores it and runs in `cwd` (the repo's main checkout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeSpec>,
}

/// Stable native review target contract shared by durable commands and the picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ReviewTarget {
    UncommittedChanges,
    BaseBranch { branch: String },
    Commit { sha: String, title: Option<String> },
    Custom { instructions: String },
}

impl ReviewTarget {
    pub fn validate(&self) -> Result<(), String> {
        let (value, label) = match self {
            Self::UncommittedChanges => return Ok(()),
            Self::BaseBranch { branch } => (branch, "base branch"),
            Self::Commit { sha, .. } => (sha, "commit"),
            Self::Custom { instructions } => (instructions, "review instructions"),
        };
        if value.trim().is_empty() {
            return Err(format!("Enter a {label} before starting the review"));
        }
        if value.contains('\0') {
            return Err(format!("The {label} contains an invalid null character"));
        }
        Ok(())
    }

    pub fn label(&self) -> String {
        match self {
            Self::UncommittedChanges => "Review working changes".into(),
            Self::BaseBranch { branch } => format!("Review changes against {branch}"),
            Self::Commit { sha, title } => format!(
                "Review commit {}{}",
                sha,
                title
                    .as_ref()
                    .map(|title| format!(": {title}"))
                    .unwrap_or_default()
            ),
            Self::Custom { instructions } => {
                format!("Review with custom instructions:\n\n{instructions}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeReviewRequest {
    pub target: ReviewTarget,
}

/// Isolated-worktree directive riding [`RunRequest`]. The worktree is created
/// by the HOST while draining the queued Run — not by the sender over a
/// blocking CreateWorktree RPC — so the send path stays durable: a lost relay
/// frame can't wedge the composer on "Sending…" while the session runs anyway
/// (2026-08-18 user report).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSpec {
    /// The repo whose worktree to create (the space's folder on the host).
    pub repo_path: String,
    /// Base ref the fresh `luna/<name>` branch is created off.
    pub base: String,
}

/// The session-scoped singleton id for the live plan/todo chip. ACP plan
/// updates carry no wire id; adapters emit every update under this one id so
/// the fold refreshes the same chip in place. Consumers that de-duplicate
/// tool ids across segment boundaries (the engine's stale-echo filter) must
/// EXEMPT this id — it legitimately reappears in every segment for the whole
/// life of a run.
pub const LIVE_PLAN_TOOL_ID: &str = "acp-plan";

/// A decoded tool invocation, reduced to the fields each kind renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ToolCall {
    Exec {
        command: String,
    },
    ReadFile {
        path: String,
    },
    WriteFile {
        path: String,
        /// Full content; STRIPPED by the render-parts policy before entering the doc.
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
    EditFile {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        old_string: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        new_string: Option<String>,
    },
    ApplyPatch {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Search {
        pattern: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Glob {
        pattern: String,
    },
    WebFetch {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    WebSearch {
        query: String,
    },
    ViewImage {
        path: String,
    },
    Todo {
        #[serde(default)]
        items: Vec<TodoItem>,
    },
    Mcp {
        server: String,
        tool: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
    /// A provider-owned computer-use call observed on this Codex turn. The
    /// status describes provider events, never success in the target app.
    ComputerUse {
        action: String,
        app: Option<String>,
        task_id: Option<String>,
        thread_id: String,
        turn_id: String,
        status: ComputerUseStatus,
        screenshot_observed: bool,
    },
    Unknown {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComputerUseStatus {
    Running,
    ProviderCompleted,
    ProviderFailed,
    Unknown,
}

impl ToolCall {
    /// A subagent SPAWN call — the `Agent[: <description>]` naming convention
    /// every driver decodes its spawn tool into (claude/codex `Task`, cursor
    /// `task`, grok `spawn_subagent`, opencode `task`). This is the single
    /// genus gate for subagent binding: tagged subagent traffic may only ever
    /// stamp a ref/status onto a spawn call, so a driver keying bug can never
    /// turn an ordinary Run/Read chip into a spawn chip (2026-08-20: claude's
    /// background-shell `task_notification` did exactly that — the chip
    /// linked to a never-created doc and opened an empty panel).
    pub fn is_subagent_spawn(&self) -> bool {
        let name = match self {
            ToolCall::Unknown { name, .. } => name,
            ToolCall::Mcp { tool, .. } => tool,
            _ => return false,
        };
        name == "Agent" || name.starts_with("Agent: ")
    }

    /// The model a subagent SPAWN was given, when the spawn named one.
    ///
    /// Read off the spawn's own input rather than the session's picked model:
    /// a spawn may override it per child (claude's `Agent` takes `model`, grok
    /// `spawn_subagent` a `model_id`), and two chips spawned in one turn can
    /// legitimately name different models. `None` means the spawn didn't say —
    /// the child inherits the parent's model, which the chip already implies,
    /// so nothing is rendered rather than guessing a name.
    ///
    /// Only ever answers for [`is_subagent_spawn`](Self::is_subagent_spawn)
    /// calls: an ordinary tool with a stray `model` argument is not a spawn.
    pub fn subagent_model(&self) -> Option<&str> {
        if !self.is_subagent_spawn() {
            return None;
        }
        let input = match self {
            ToolCall::Unknown { input, .. } | ToolCall::Mcp { input, .. } => input.as_ref()?,
            _ => return None,
        };
        SUBAGENT_MODEL_KEYS
            .iter()
            .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|model| !model.is_empty())
    }
}

/// Spawn-input keys that carry a child model, in precedence order. Drivers
/// disagree on the spelling, so the lookup is by key set, not by harness —
/// a new adapter naming it any of these needs no code change here.
pub const SUBAGENT_MODEL_KEYS: [&str; 4] = ["model", "modelId", "model_id", "subagent_model"];

/// The spawn-input keys [`sanitize_tool_call`](crate::) must preserve so the
/// chip can name the child's model. Deliberately tiny: everything else on a
/// spawn's input (the whole prompt, most of all) stays host-local.
pub const SUBAGENT_INPUT_KEEP: [&str; 5] = [
    "model",
    "modelId",
    "model_id",
    "subagent_model",
    "subagent_type",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    pub text: String,
    pub done: bool,
}

/// A slash command advertised by the agent (ACP `availableCommands`): typed as
/// `/name` at the start of the composer, sent to the agent as prompt text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Placeholder hint for the command's argument, when it takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hint: Option<String>,
}

/// A file modification carried inline on a tool result (ACP
/// `ToolCallContent::Diff`). `old_text: None` means a new file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiff {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    pub new_text: String,
    /// Provider-supplied patch when complete before/after text is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<String>,
    #[serde(default)]
    pub multi_select: bool,
    /// Present for typed provider or engine-owned approval requests. Keeping
    /// this on the input part preserves the established inline UI and recovery
    /// marker; live authorization is held separately by the engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalRequest>,
    /// Generic MCP form/URL input; not a permission grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<crate::McpInput>,
    /// Provider answer policy for a content question (Codex
    /// `item/tool/requestUserInput`). `None` keeps the legacy behaviour:
    /// options plus an always-available typed answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<UserInputDetails>,
}

/// Codex `ToolRequestUserInputQuestion` answer policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputDetails {
    /// Parallel to `UserInputQuestion::options`; empty strings when absent.
    #[serde(default)]
    pub option_descriptions: Vec<String>,
    /// `isOther`: a typed answer is accepted in addition to the options.
    #[serde(default)]
    pub allow_other: bool,
    /// `isSecret`: mask the typed answer while it is entered.
    #[serde(default)]
    pub secret: bool,
}

impl UserInputQuestion {
    /// Whether a typed answer is offered. Questions without options always
    /// take text; provider policy decides when options exist.
    pub fn accepts_typed_answer(&self) -> bool {
        self.options.is_empty() || self.input.as_ref().is_none_or(|input| input.allow_other)
    }

    pub fn option_description(&self, index: usize) -> Option<&str> {
        self.input
            .as_ref()?
            .option_descriptions
            .get(index)
            .map(String::as_str)
            .filter(|text| !text.is_empty())
    }

    pub fn is_secret(&self) -> bool {
        self.input.as_ref().is_some_and(|input| input.secret)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputAnswer {
    pub question_id: String,
    pub labels: Vec<String>,
}

/// A Codex async question is assistant-message metadata, not a pending
/// `request_user_input` RPC. Its answer is a later user message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsyncUserInputQuestion {
    pub title: String,
    #[serde(default)]
    pub options: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DoneStatus {
    Completed,
    Interrupted,
    Errored,
}

/// Provider-observed state of a reusable child session. This is distinct from
/// a child turn's `Done`, which reports an assignment outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChildRuntimeStatus {
    Active,
    Idle,
}

/// Live tool activity. Raw text stays on the owning host; documents retain a
/// small display tail. Patch text is a serialized `ToolDiff`, not a terminal delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolActivityKind {
    Stdout,
    Stdin,
    Patch,
    /// Provider progress status for a running tool (latest message wins).
    Progress,
}

/// Transient provider status shown beside the working indicator. Never a
/// transcript part and never a turn failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderNoticeKind {
    /// The provider is retrying (e.g. Codex `error {willRetry:true}`).
    Retrying,
    /// An informational provider warning (e.g. Codex `warning`).
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderNotice {
    pub kind: ProviderNoticeKind,
    pub message: String,
    pub at_ms: i64,
}

/// The normalized streaming event every harness emits.
///
/// Mirrors luna's `AgentEvent` tagged enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    #[serde(rename_all = "camelCase")]
    SessionStarted {
        harness: HarnessId,
        model: String,
        #[serde(default)]
        tools: Vec<String>,
        cwd: String,
        /// Harness-native session id (used for resume).
        session_id: String,
        assistant_message_id: String,
    },
    /// Native review lifecycle. Completion text is authoritative and folded by ID,
    /// so started/completed replay never duplicates findings in the transcript.
    #[serde(rename_all = "camelCase")]
    NativeReview {
        id: String,
        completed: bool,
        text: String,
    },
    TextDelta {
        text: String,
    },
    /// A generated raster asset. The engine materializes this path before publication.
    #[serde(rename_all = "camelCase")]
    GeneratedImage {
        id: String,
        path: String,
        name: String,
        mime_type: String,
    },
    ReasoningDelta {
        text: String,
    },
    /// Backend-internal steering boundary marker.
    #[serde(rename_all = "camelCase")]
    AssistantMessageCompleted {
        assistant_message_id: String,
    },
    ToolCall {
        id: String,
        call: ToolCall,
    },
    #[serde(rename_all = "camelCase")]
    ToolActivity {
        id: String,
        kind: ToolActivityKind,
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolResult {
        id: String,
        is_error: bool,
        /// Complete output received by the adapter. The host journals it
        /// before limiting live-event or synced-document previews. Provider
        /// limits still apply; this does not recover bytes never received.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// Inline file diff for edit-shaped tools (ACP `Diff` content).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<ToolDiff>,
    },
    /// Latest context occupancy, independent of cumulative billing usage.
    /// Missing fields preserve the previous measurement; zero tokens is valid.
    #[serde(rename_all = "camelCase")]
    ContextUsage {
        tokens: Option<u64>,
        window: Option<u64>,
    },
    /// Provider-reported context compaction for the active root turn. This is
    /// control-plane state, not transcript content or a turn-completion signal.
    /// The operation id is derived from the provider thread/turn/item tuple so
    /// stale completions cannot settle a newer rollover.
    #[serde(rename_all = "camelCase")]
    ContextCompaction {
        operation_id: String,
        status: crate::ContextOperationStatus,
        thread_id: String,
        turn_id: String,
    },
    /// Provider-owned checkpoint for the current completed root turn. This is
    /// control-plane metadata used by LUNA's provider-backed fork/edit path;
    /// it is never rendered as transcript content. Adapters emit it only when
    /// the provider exposes a stable native history boundary.
    #[serde(rename_all = "camelCase")]
    ProviderCheckpoint {
        checkpoint_id: String,
    },
    /// Kept as a harness passthrough (rate-limit probes); never persisted to docs.
    #[serde(rename_all = "camelCase")]
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    /// Native child-session status, only meaningful inside `Subagent`.
    #[serde(rename_all = "camelCase")]
    ChildStatus {
        status: ChildRuntimeStatus,
    },
    /// The agent advertised (or changed) its slash-command set — ACP
    /// `available_commands_update`. The engine caches the latest list per
    /// harness for the composer's `/` popup; never persisted to docs.
    #[serde(rename_all = "camelCase")]
    AvailableCommands {
        commands: Vec<SlashCommand>,
    },
    /// Authoritative native turn boundary, including autonomous goal turns.
    #[serde(rename_all = "camelCase")]
    ProviderTurnStarted {
        thread_id: String,
        turn_id: String,
    },
    /// Provider goal snapshots never imply a transcript/turn completion.
    #[serde(rename_all = "camelCase")]
    GoalChanged {
        thread_id: String,
        goal: Option<crate::ThreadGoal>,
        /// Codex's `turnId`: set when the change happened inside a provider
        /// turn (accounting, or the model's `update_goal`), null for client
        /// controls and resume snapshots.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    /// Internal supported-provider notification that asks the engine to
    /// refresh task metadata. It is consumed before journal/doc publication.
    #[serde(rename_all = "camelCase")]
    ProviderTaskChanged {
        change: String,
    },
    /// Host-only native provider evidence: decoded lifecycle frames an
    /// adapter does not project yet (tasks, hooks, compaction, resets),
    /// per-result identity records, and bounded, redacted samples of unknown
    /// or invalid frames. Journaled for inspection only: never a transcript
    /// part, turn boundary, resume/liveness signal, or crash-recovery
    /// activity.
    #[serde(rename_all = "camelCase")]
    ProviderEvidence {
        provider: HarnessId,
        /// `result`, `system/<subtype>`, `conversation_reset`,
        /// `native:<kind>`, `unknown:<kind>` or `invalid:<kind>`.
        kind: String,
        /// The native frame uuid, when the provider supplied one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native_id: Option<String>,
        /// Bounded, redacted payload.
        #[serde(default)]
        payload: serde_json::Value,
    },
    Error {
        message: String,
    },
    /// Transient provider status (retry, warning). The engine projects the
    /// latest notice onto task metadata and clears it when the turn settles.
    #[serde(rename_all = "camelCase")]
    ProviderNotice {
        notice: ProviderNotice,
    },
    #[serde(rename_all = "camelCase")]
    InputRequested {
        request_id: String,
        questions: Vec<UserInputQuestion>,
    },
    #[serde(rename_all = "camelCase")]
    InputResolved {
        request_id: String,
    },
    #[serde(rename_all = "camelCase")]
    AsyncQuestions {
        /// Stable provider agent-message item id for replay and command dedup.
        message_id: String,
        questions: Vec<AsyncUserInputQuestion>,
    },
    /// A confirmed new assignment. When tagged as Subagent, this reopens the
    /// same child transcript even if the provider does not echo the user text.
    #[serde(rename_all = "camelCase")]
    Steered {
        /// Exact native client user-message correlation when available. Native
        /// recording proves incorporation into history, not model sampling.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_message_id: Option<String>,
        assistant_message_id: Option<String>,
        /// With an exact user_message_id, None denotes historical readback:
        /// settle delivery without reopening a turn or rotating live output.
        next_assistant_message_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Done {
        status: DoneStatus,
        result: Option<String>,
        error: Option<String>,
        session_id: Option<String>,
    },
    /// A USER-role message injected into a running session — today only seen
    /// wrapped in [`AgentEvent::Subagent`]: the PARENT agent steering its
    /// subagent mid-run (claude: a tagged user frame's text blocks). The
    /// engine writes it to the subagent doc as its own user entry, closing
    /// the streaming assistant segment above it — the subagent transcript
    /// then reads like any steered chat. Never emitted untagged (the parent
    /// chat's user messages come from doc commands, not the wire).
    #[serde(rename_all = "camelCase")]
    UserMessage {
        text: String,
    },
    /// An event belonging to a SUBAGENT's nested transcript, attributed to
    /// the spawning tool call (`parent_tool_use_id` = the parent-feed
    /// `ToolCall::id` that launched it). Never folded into the parent chat
    /// doc — the engine routes these to the subagent's own doc; the parent
    /// keeps only the spawn chip. Additive: old consumers that don't match
    /// this variant drop the nested traffic, which is the pre-subagent-viz
    /// behavior.
    #[serde(rename_all = "camelCase")]
    Subagent {
        parent_tool_use_id: String,
        event: Box<AgentEvent>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_event_round_trips() {
        let ev = AgentEvent::ToolCall {
            id: "t1".into(),
            call: ToolCall::Exec {
                command: "cargo test".into(),
            },
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), ev);
    }

    /// Drivers spell the key differently; the chip must not care which one
    /// spawned the child. Non-spawns never answer, whatever they carry.
    #[test]
    fn subagent_model_reads_every_spelling_and_only_off_a_spawn() {
        let spawn = |input: serde_json::Value| ToolCall::Unknown {
            name: "Agent: scan".into(),
            input: Some(input),
        };
        for key in SUBAGENT_MODEL_KEYS {
            let call = spawn(serde_json::json!({ key: "haiku" }));
            assert_eq!(call.subagent_model(), Some("haiku"), "key {key}");
        }
        // An MCP-shaped spawn (cursor routes its `task` through MCP) too.
        assert_eq!(
            ToolCall::Mcp {
                server: "s".into(),
                tool: "Agent: scan".into(),
                input: Some(serde_json::json!({ "model": "sonnet" })),
            }
            .subagent_model(),
            Some("sonnet")
        );
        // Not a spawn: the name gate wins over the key.
        assert_eq!(
            ToolCall::Unknown {
                name: "Bash".into(),
                input: Some(serde_json::json!({ "model": "haiku" })),
            }
            .subagent_model(),
            None
        );
        // A spawn that named nothing usable inherits — nothing to render.
        assert_eq!(
            spawn(serde_json::json!({ "model": " " })).subagent_model(),
            None
        );
        assert_eq!(
            spawn(serde_json::json!({ "prompt": "x" })).subagent_model(),
            None
        );
        assert_eq!(
            ToolCall::Unknown {
                name: "Agent".into(),
                input: None
            }
            .subagent_model(),
            None
        );
        // Non-string values are not names.
        assert_eq!(
            spawn(serde_json::json!({ "model": 5 })).subagent_model(),
            None
        );
    }

    #[test]
    fn run_request_attachments_default_and_round_trip() {
        // Old-wire JSON without the field parses (additive compat)…
        let old = r#"{"prompt":"p","model":null,"reasoning":null,"cwd":".","sandbox":"workspace-write","resume":null}"#;
        let req: RunRequest = serde_json::from_str(old).unwrap();
        assert!(req.provider_account_id.is_none());
        assert!(req.attachments.is_empty());
        assert_eq!(req.permission_mode, PermissionMode::Ask);
        assert!(req.permission_policy.is_none());
        assert!(!req.auto_approve);
        // …and an empty list serializes away (old readers never see it).
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("attachments").is_none());
        // Populated lists round-trip.
        let req = RunRequest {
            attachments: vec!["/tmp/a.png".into()],
            ..req
        };
        let round: RunRequest =
            serde_json::from_value(serde_json::to_value(&req).unwrap()).unwrap();
        assert_eq!(round.attachments, vec!["/tmp/a.png".to_string()]);
    }

    #[test]
    fn run_request_worktree_default_and_round_trip() {
        // Old-wire JSON without the field parses (additive compat)…
        let old = r#"{"prompt":"p","model":null,"reasoning":null,"cwd":".","sandbox":"workspace-write","resume":null}"#;
        let req: RunRequest = serde_json::from_str(old).unwrap();
        assert!(req.worktree.is_none());
        // …and `None` serializes away (old readers never see it).
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("worktree").is_none());
        // A populated spec round-trips camelCased.
        let req = RunRequest {
            worktree: Some(WorktreeSpec {
                repo_path: "/repos/comet".into(),
                base: "main".into(),
            }),
            ..req
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["worktree"]["repoPath"], "/repos/comet");
        let round: RunRequest = serde_json::from_value(json).unwrap();
        assert_eq!(round.worktree, req.worktree);
    }

    #[test]
    fn harness_id_uses_kebab_case() {
        assert_eq!(
            serde_json::to_string(&HarnessId::ClaudeCode).unwrap(),
            "\"claude-code\""
        );
    }

    #[test]
    fn older_approval_actions_remain_provider_and_luna_marker_round_trips() {
        let old = serde_json::json!({
            "providerRequestId": "provider-1", "provider": "codex",
            "machineName": "synthetic", "actionKind": "tool",
            "summary": "synthetic provider tool", "requestedScope": "workspace",
            "expiresAtMs": 1234, "supportsTaskAllowance": false
        });
        let provider: ApprovalRequest = serde_json::from_value(old.clone()).unwrap();
        assert!(!provider.is_luna_cross_task());
        assert_eq!(serde_json::to_value(&provider).unwrap(), old);
        let luna = ApprovalRequest {
            action_kind: ApprovalActionKind::LunaCrossTask,
            ..provider
        };
        let encoded = serde_json::to_value(&luna).unwrap();
        assert_eq!(encoded["actionKind"], "lunaCrossTask");
        assert!(
            serde_json::from_value::<ApprovalRequest>(encoded)
                .unwrap()
                .is_luna_cross_task()
        );
    }
}

/// Host-owned context snapshot, replicated with the chat document.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub tokens: Option<u64>,
    pub window: Option<u64>,
}

impl ContextUsage {
    pub fn fraction(self) -> Option<f64> {
        Some(self.tokens? as f64 / self.window.filter(|n| *n > 0)? as f64)
    }
}

#[cfg(test)]
mod generated_image_tests {
    use super::*;

    #[test]
    fn generated_image_event_round_trip() {
        let event = AgentEvent::GeneratedImage {
            id: "item:image".into(),
            path: "/uploads/generated.png".into(),
            name: "generated.png".into(),
            mime_type: "image/png".into(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "generatedImage");
        assert_eq!(value["mimeType"], "image/png");
        assert_eq!(serde_json::from_value::<AgentEvent>(value).unwrap(), event);
    }
}
