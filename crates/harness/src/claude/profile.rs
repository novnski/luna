//! Effective Claude Code configuration shared by sessions and discovery (G1, E23).
//!
//! Everything that decides which settings files, custom commands, output styles,
//! hooks and built-in tools a `claude` process loads lives here. Session launches
//! and the `initialize` discovery probe both build their argv, cwd and provider home
//! from one [`ClaudeLaunchProfile`], so the command catalog LUNA shows is the one
//! the run actually gets.
//!
//! Ask/Auto semantics, recorded against Claude Code 2.1.286 (see
//! `tests/fixtures/claude/compat-2.1.286.json`):
//! - `--restricted` stays the permission boundary. It confines the file tools to the
//!   working directories, refuses bypassPermissions, and leaves settings/git/tool-config
//!   writes to a person or the permission handler. It also ignores user, project and
//!   local settings files (and so custom commands, output styles and their hooks).
//!   `--setting-sources` cannot re-enable them, but `--settings` and managed settings
//!   still apply.
//! - `--restricted` removes Bash and the other code-running tools, plus WebFetch,
//!   unless `--tools` NAMES them. The `default` preset does not count as naming them.
//!   Ask/Auto therefore pass an explicit grant: the restricted 2.1.286 tool set plus
//!   Bash and WebFetch. Those escalate through the stdio permission channel (Ask:
//!   the user; Auto: the native reviewer, then the user).
//!   Workflow and CronCreate stay ungranted until their own lanes certify them.
//! - Full runs unrestricted (bypassPermissions) and loads every settings source.

use std::path::{Path, PathBuf};

use luna_proto::{PermissionMode, RunRequest};
use serde_json::{Map, Value};

use crate::process::Command;

/// Claude Code release the Ask/Auto tool grant was recorded against.
pub const RECORDED_CLI_VERSION: &str = "2.1.286";

/// The built-in tools `--restricted` keeps on 2.1.286 (observed in `system/init`),
/// followed by the command/web tools it removes unless `--tools` names them.
pub const ASK_AUTO_TOOL_GRANT: &[&str] = &[
    "Task",
    "AskUserQuestion",
    "CronDelete",
    "CronList",
    "Edit",
    "EnterPlanMode",
    "EnterWorktree",
    "ExitPlanMode",
    "ExitWorktree",
    "Glob",
    "Grep",
    "ListAgents",
    "NotebookEdit",
    "Read",
    "ReportFindings",
    "ScheduleWakeup",
    "SendMessage",
    "Skill",
    "TaskStop",
    "WebSearch",
    "Write",
    "Bash",
    "WebFetch",
];

/// Which built-in tools a launch asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolSelection {
    /// The mode's normal set: the explicit grant under `--restricted`, the CLI's
    /// own default set otherwise.
    ModeDefault,
    /// `--tools ""` (title runs).
    Disabled,
}

/// The configuration identity of one Claude process: permission policy, tools,
/// working directory and provider home. Run-only flags (model, effort, resume,
/// prompts) and run-only settings (fast mode, thinking) are layered on top and
/// never change which commands, styles or tools load.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClaudeLaunchProfile {
    pub mode: PermissionMode,
    pub tools: ToolSelection,
    /// `None` inherits the engine's working directory (unscoped discovery).
    pub cwd: Option<PathBuf>,
    /// Explicit provider home (`CLAUDE_CONFIG_DIR`); `None` inherits the engine's.
    pub config_home: Option<PathBuf>,
}

impl ClaudeLaunchProfile {
    pub fn new(mode: PermissionMode, cwd: Option<PathBuf>, config_home: Option<PathBuf>) -> Self {
        Self {
            mode,
            tools: ToolSelection::ModeDefault,
            cwd: cwd.filter(|cwd| !cwd.as_os_str().is_empty()),
            config_home,
        }
    }

    /// The profile a session run launches with: the engine-resolved effective
    /// mode (Ask when no policy was stamped) and the request's cwd.
    pub fn for_request(request: &RunRequest, config_home: Option<PathBuf>) -> Self {
        let mode = request
            .permission_policy
            .as_ref()
            .map(|policy| policy.effective_mode)
            .unwrap_or(PermissionMode::Ask);
        Self::new(mode, Some(PathBuf::from(&request.cwd)), config_home)
    }

    pub fn restricted(&self) -> bool {
        !matches!(self.mode, PermissionMode::Full)
    }

    /// Policy flags. Discovery and runs pass exactly these.
    pub fn policy_args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec!["--permission-prompt-tool".into(), "stdio".into()];
        args.extend(mode_args(self.mode).iter().map(|arg| (*arg).to_owned()));
        match self.tools {
            ToolSelection::Disabled => args.extend(["--tools".into(), String::new()]),
            ToolSelection::ModeDefault if self.restricted() => {
                args.extend(["--tools".into(), ASK_AUTO_TOOL_GRANT.join(",")]);
            }
            ToolSelection::ModeDefault => {}
        }
        args
    }

    /// The provider home the child reads: the explicit override, else the
    /// inherited `CLAUDE_CONFIG_DIR`, else `~/.claude`.
    pub fn effective_config_home(&self) -> Option<PathBuf> {
        self.config_home
            .clone()
            .or_else(|| {
                std::env::var_os("CLAUDE_CONFIG_DIR")
                    .filter(|dir| !dir.is_empty())
                    .map(PathBuf::from)
            })
            .or_else(|| crate::executable::home_dir().map(|home| home.join(".claude")))
    }

    /// Apply the policy flags, ONE merged `--settings` layer, the cwd and the
    /// provider home to `cmd`.
    pub fn apply(&self, cmd: &mut Command, settings: &Map<String, Value>) {
        cmd.args(self.policy_args());
        if !settings.is_empty() {
            cmd.arg("--settings");
            cmd.arg(Value::Object(settings.clone()).to_string());
        }
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        if let Some(home) = &self.config_home {
            cmd.env("CLAUDE_CONFIG_DIR", home);
        }
    }
}

/// Native permission flags for a LUNA mode.
pub fn mode_args(mode: PermissionMode) -> &'static [&'static str] {
    match mode {
        PermissionMode::Ask => &["--restricted", "--permission-mode", "acceptEdits"],
        PermissionMode::Auto => &["--restricted", "--permission-mode", "auto"],
        PermissionMode::Full => &[
            "--permission-mode",
            "bypassPermissions",
            "--dangerously-skip-permissions",
        ],
    }
}

/// Settings a discovery probe adds on top of the run's: `initialize` alone runs
/// SessionStart hooks (observed on 2.1.286), so discovery never runs hooks.
/// Hooks do not change the commands, styles or tools initialize reports.
pub fn discovery_settings() -> Map<String, Value> {
    let mut settings = Map::new();
    settings.insert("disableAllHooks".into(), Value::Bool(true));
    settings
}

/// Canonical form used for scope keys and the cwd handed to the child.
pub fn canonical_dir(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
