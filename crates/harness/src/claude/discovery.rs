//! Scoped Claude Code discovery (G1, E23).
//!
//! The `initialize` control request reports the commands and output style the
//! process loaded. It is only truthful for a run when it uses that run's binary,
//! provider home, cwd and policy, so discovery spawns from the same
//! [`ClaudeLaunchProfile`] and caches per scope. The key covers the executable
//! identity, policy argv, cwd, provider home, account instance and a fingerprint
//! of the files that feed commands/styles/settings. A project or account switch
//! therefore re-probes, and so does adding or editing a command or settings file.
//! A TTL backstops sources the fingerprint cannot see. Only successes are cached.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use luna_proto::SlashCommand;

use super::profile::ClaudeLaunchProfile;
use crate::process::{Command, Stdio};
use crate::{CommandScope, HarnessError, shutdown_child};

const CACHE_TTL: Duration = Duration::from_secs(600);
const CACHE_ENTRIES: usize = 32;
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_ID: &str = "luna-command-probe";
/// Files and folders under a `.claude` root that feed commands, styles, skills,
/// agents or settings. `.claude/worktrees` and other bulk folders are never walked.
const CONFIG_ENTRIES: &[&str] = &[
    "settings.json",
    "settings.local.json",
    "commands",
    "output-styles",
    "skills",
    "agents",
    "plugins/installed_plugins.json",
    "plugins/known_marketplaces.json",
];
const MANAGED_SETTINGS: &[&str] = &[
    "/Library/Application Support/ClaudeCode/managed-settings.json",
    "/etc/claude-code/managed-settings.json",
];
const FINGERPRINT_BUDGET: usize = 2048;

/// Successful discovery results per scope key.
#[derive(Default)]
pub(super) struct DiscoveryCache {
    entries: Mutex<HashMap<u64, (Instant, Vec<SlashCommand>)>>,
}

impl DiscoveryCache {
    pub(super) fn get(&self, key: u64) -> Option<Vec<SlashCommand>> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .get(&key)
            .filter(|(at, _)| at.elapsed() < CACHE_TTL)
            .map(|(_, commands)| commands.clone())
    }

    pub(super) fn put(&self, key: u64, commands: Vec<SlashCommand>) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
        if entries.len() >= CACHE_ENTRIES
            && let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(key, _)| *key)
        {
            entries.remove(&oldest);
        }
        entries.insert(key, (Instant::now(), commands));
    }
}

/// Cache key for one discovery scope; see the module docs for what it covers.
pub(super) fn scope_key(exe: &Path, profile: &ClaudeLaunchProfile, scope: &CommandScope) -> u64 {
    let mut hasher = DefaultHasher::new();
    stat(&std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf())).hash(&mut hasher);
    profile.policy_args().hash(&mut hasher);
    profile.cwd.hash(&mut hasher);
    let home = profile.effective_config_home();
    home.hash(&mut hasher);
    scope.auth_instance.hash(&mut hasher);
    config_fingerprint(home.as_deref(), profile.cwd.as_deref()).hash(&mut hasher);
    hasher.finish()
}

type Stamp = (PathBuf, Option<(u64, SystemTime)>);

fn stat(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path)
        .ok()
        .and_then(|meta| Some((meta.len(), meta.modified().ok()?)));
    (path.to_path_buf(), meta)
}

/// Bounded stat walk of the configuration inputs a Claude process reads: the
/// provider home, every `.claude`/`.mcp.json` from the cwd up to the root, and
/// managed settings.
fn config_fingerprint(home: Option<&Path>, cwd: Option<&Path>) -> Vec<Stamp> {
    let mut out = Vec::new();
    let mut budget = FINGERPRINT_BUDGET;
    if let Some(home) = home {
        walk_config_root(home, &mut out, &mut budget);
    }
    if let Some(cwd) = cwd {
        for dir in cwd.ancestors() {
            let root = dir.join(".claude");
            if root.is_dir() {
                walk_config_root(&root, &mut out, &mut budget);
            }
            let mcp = dir.join(".mcp.json");
            if mcp.exists() {
                out.push(stat(&mcp));
            }
        }
    }
    for managed in MANAGED_SETTINGS {
        out.push(stat(Path::new(managed)));
    }
    out
}

fn walk_config_root(root: &Path, out: &mut Vec<Stamp>, budget: &mut usize) {
    for entry in CONFIG_ENTRIES {
        walk(&root.join(entry), 3, out, budget);
    }
}

fn walk(path: &Path, depth: usize, out: &mut Vec<Stamp>, budget: &mut usize) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    out.push(stat(path));
    if depth == 0 || !path.is_dir() {
        return;
    }
    let Ok(read) = std::fs::read_dir(path) else {
        return;
    };
    let mut children: Vec<PathBuf> = read.flatten().map(|entry| entry.path()).collect();
    children.sort();
    for child in children {
        walk(&child, depth - 1, out, budget);
    }
}

/// Spawn `exe` in stream-json mode, apply `configure` (profile flags, cwd,
/// home), send only the `initialize` control request and return its
/// control_response. No user message is ever written, so no turn or API call
/// happens; the child is torn down once the response lands.
pub(super) async fn initialize_probe(
    exe: &Path,
    configure: impl FnOnce(&mut Command),
    kill_grace: Duration,
) -> Result<Value, HarnessError> {
    probe(exe, configure, kill_grace, false).await
}

pub(super) async fn usage_probe(
    exe: &Path,
    configure: impl FnOnce(&mut Command),
    kill_grace: Duration,
) -> Result<Value, HarnessError> {
    probe(exe, configure, kill_grace, true).await
}

async fn probe(
    exe: &Path,
    configure: impl FnOnce(&mut Command),
    kill_grace: Duration,
    include_usage: bool,
) -> Result<Value, HarnessError> {
    let mut cmd = Command::new(exe);
    crate::compose_child_path(&mut cmd, exe);
    cmd.args([
        "--print",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        // Mandatory with --print + stream-json output; without it the CLI
        // exits immediately with a usage error.
        "--verbose",
    ]);
    if include_usage {
        cmd.arg("--no-session-persistence");
    }
    // A probe without a workspace runs in a neutral directory, never the host
    // process's own, whose project settings and commands are not the user's.
    // A scoped probe's launch profile sets the scope's cwd over this.
    cmd.current_dir(std::env::temp_dir());
    configure(&mut cmd);
    crate::mcp_isolation::claude_strict_empty(&mut cmd);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            HarnessError::NotInstalled(exe.display().to_string())
        } else {
            HarnessError::Io(e)
        }
    })?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        shutdown_child(&mut child, kill_grace).await;
        return Err(HarnessError::Protocol("claude child has no stdio".into()));
    };
    let discovery = async {
        let request = serde_json::json!({
            "type": "control_request",
            "request_id": PROBE_ID,
            "request": { "subtype": "initialize" },
        });
        stdin
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(HarnessError::Io)?;
        stdin.flush().await.map_err(HarnessError::Io)?;
        let mut lines = BufReader::new(stdout).lines();
        let mut initialized = None;
        while let Some(line) = lines.next_line().await.map_err(HarnessError::Io)? {
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if frame.get("type").and_then(Value::as_str) != Some("control_response") {
                continue;
            }
            let response = frame.get("response").cloned().unwrap_or(Value::Null);
            let expected = if initialized.is_some() {
                "luna-usage-probe"
            } else {
                PROBE_ID
            };
            if response.get("request_id").and_then(Value::as_str) != Some(expected) {
                continue;
            }
            if response.get("subtype").and_then(Value::as_str) == Some("error") {
                let msg = response
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("initialize control request failed");
                return Err(HarnessError::Protocol(msg.into()));
            }
            if include_usage && initialized.is_none() {
                let body = response.get("response").cloned().unwrap_or(Value::Null);
                initialized = Some(body.get("account").cloned().unwrap_or(Value::Null));
                let request = serde_json::json!({"type":"control_request", "request_id":"luna-usage-probe", "request":{"subtype":"get_usage"}});
                stdin
                    .write_all(format!("{request}\n").as_bytes())
                    .await
                    .map_err(HarnessError::Io)?;
                stdin.flush().await.map_err(HarnessError::Io)?;
                continue;
            }
            return Ok(match initialized {
                Some(account) => {
                    serde_json::json!({"account":account, "usage":response.get("response")})
                }
                None => response,
            });
        }
        Err(HarnessError::Protocol(
            "claude exited before answering the initialize control request".into(),
        ))
    };
    let timeout = if include_usage {
        Duration::from_secs(20)
    } else {
        PROBE_TIMEOUT
    };
    let result = tokio::time::timeout(timeout, discovery).await;
    shutdown_child(&mut child, kill_grace).await;
    match result {
        Ok(inner) => inner,
        Err(_) => Err(HarnessError::Protocol(
            "Claude initialization timed out".into(),
        )),
    }
}
