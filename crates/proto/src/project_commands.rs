//! User-owned, device-local project actions and durable terminal receipts,
//! plus the AW-11 repository layer (`.luna/project.toml`) beneath them.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAction {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub setup_on_worktree_create: bool,
    #[serde(default = "yes")]
    pub wait_for_setup: bool,
    #[serde(default)]
    pub preview_url: Option<String>,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectCommandStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Canceled,
}
impl ProjectCommandStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Running => "Running",
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
            Self::Canceled => "Canceled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCommandRun {
    pub id: String,
    pub project_id: String,
    pub device_id: String,
    pub chat_id: String,
    pub cwd: String,
    pub checkout_generation: String,
    pub action: ProjectAction,
    pub action_digest: String,
    pub setup: bool,
    pub attempt: u32,
    pub status: ProjectCommandStatus,
    pub terminal_id: Option<String>,
    pub previous_terminal_ids: Vec<String>,
    pub process_group_id: Option<u32>,
    pub exit_code: Option<i32>,
    pub detail: Option<String>,
    pub updated_at_ms: i64,
    /// Where the captured content came from. Receipts written before AW-11
    /// were all personal.
    #[serde(default)]
    pub source: ProjectCommandSource,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCommandsSnapshot {
    pub project_id: String,
    pub cwd: String,
    /// The runnable, effective list in precedence order: this device's
    /// commands, then accepted repository commands no personal command
    /// overrides. Unaccepted repository commands are never in it.
    pub actions: Vec<ProjectAction>,
    pub runs: Vec<ProjectCommandRun>,
    /// The checkout's repository layer with this device's trust decisions.
    /// Absent from engines that predate AW-11.
    #[serde(default)]
    pub repository: Option<ProjectRepositoryConfig>,
}

// AW-11 portable project configuration. A repository may check in
// `.luna/project.toml`; it is untrusted input layered UNDER this device's
// commands. Reading it never runs anything, and a repository command becomes
// runnable only after this device accepts its exact content (the digest
// below). Any change to that content makes it unaccepted again.

/// The checked-in file, relative to the task checkout or project folder.
pub const PROJECT_CONFIG_FILE: &str = ".luna/project.toml";
/// The only `version` this build reads; any other value is refused.
pub const PROJECT_CONFIG_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectCommandSource {
    /// Saved on this device (Settings → Projects): authored here.
    #[default]
    Personal,
    /// Declared in the checkout's `.luna/project.toml`.
    Repository,
}

/// This device's decision about one repository command's current content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectCommandTrust {
    /// This exact content was accepted on this device.
    Accepted,
    /// Never accepted on this device.
    Unaccepted,
    /// Accepted once, but the content changed since; it needs a new review.
    Changed,
}
impl ProjectCommandTrust {
    pub fn label(self) -> &'static str {
        match self {
            Self::Accepted => "Accepted",
            Self::Unaccepted => "Not accepted",
            Self::Changed => "Changed since accepted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectConfigState {
    /// The checkout has no `.luna/project.toml`.
    Absent,
    /// Read and validated; `commands` holds its declarations.
    Loaded,
    /// Present but ignored as a whole; `detail` says why. Personal commands
    /// keep working.
    Refused,
    /// The project folder itself could not be read.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRepositoryCommand {
    /// Canonical content: repository commands never set up worktrees and
    /// always wait for a running setup.
    pub action: ProjectAction,
    /// sha256 of the canonical action; acceptance pins exactly this value.
    pub digest: String,
    pub trust: ProjectCommandTrust,
    /// A command on this device with the same ID takes precedence.
    pub overridden: bool,
}
impl ProjectRepositoryCommand {
    /// Whether Run may execute this repository content on this device.
    pub fn runnable(&self) -> bool {
        self.trust == ProjectCommandTrust::Accepted && !self.overridden
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRepositoryConfig {
    /// The checkout or project folder the file was read from.
    pub checkout: String,
    /// Always [`PROJECT_CONFIG_FILE`].
    pub file: String,
    pub state: ProjectConfigState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub commands: Vec<ProjectRepositoryCommand>,
}
