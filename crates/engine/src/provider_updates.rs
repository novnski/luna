//! Device-local CLI maintenance. Checks are bounded and cached; updates run
//! only after an explicit UI action and are verified against the same binary
//! path used to launch tasks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::future::join_all;
use luna_harness::process::Command;
use luna_proto::{HarnessId, ProviderUpdateStatus, version_triple};

const CHECK_TTL: Duration = Duration::from_secs(2 * 60 * 60);
const VERSION_TIMEOUT: Duration = Duration::from_secs(3);
const REGISTRY_TIMEOUT: Duration = Duration::from_secs(4);
const BREW_TIMEOUT: Duration = Duration::from_secs(6);
const UPDATE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const PROVIDERS: [HarnessId; 3] = [HarnessId::Codex, HarnessId::ClaudeCode, HarnessId::Opencode];

static CHECK_CACHE: OnceLock<Mutex<HashMap<HarnessId, (Instant, ProviderUpdateStatus)>>> =
    OnceLock::new();
static UPDATE_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<HarnessId, (Instant, ProviderUpdateStatus)>> {
    CHECK_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn path_for(harness: HarnessId) -> Option<PathBuf> {
    match harness {
        HarnessId::Codex => luna_harness::codex::resolve_codex_executable(),
        HarnessId::ClaudeCode => luna_harness::claude::resolve_claude_executable(),
        HarnessId::Opencode => luna_harness::opencode::resolve_opencode_executable(),
        _ => None,
    }
}

fn package_for(harness: HarnessId) -> Option<&'static str> {
    match harness {
        HarnessId::Codex => Some("@openai/codex"),
        HarnessId::ClaudeCode => Some("@anthropic-ai/claude-code"),
        HarnessId::Opencode => Some("opencode-ai"),
        _ => None,
    }
}

fn parse_version(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|part| part.trim_start_matches('v'))
        .find_map(|part| {
            let version = part.trim_matches(|ch: char| !ch.is_ascii_digit() && ch != '.');
            version_triple(version).map(|_| version.to_owned())
        })
}

async fn installed_version(path: &Path) -> Option<String> {
    let mut command = Command::new(path);
    luna_harness::compose_child_path(&mut command, path);
    command.arg("--version").kill_on_drop(true);
    let output = tokio::time::timeout(VERSION_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_version(&String::from_utf8_lossy(&output.stdout)))
        .flatten()
}

async fn latest_brew_version(path: &Path, args: &[String]) -> Option<String> {
    let cask = args.iter().any(|arg| arg == "--cask");
    let formula = args.last()?;
    let mut command = Command::new("brew");
    luna_harness::compose_child_path(&mut command, path);
    command.arg("info").arg("--json=v2");
    if cask {
        command.arg("--cask");
    }
    command
        .arg(formula)
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .kill_on_drop(true);
    let output = tokio::time::timeout(BREW_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() || output.stdout.len() > 256 * 1024 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let version = if cask {
        value
            .get("casks")?
            .as_array()?
            .first()?
            .get("version")?
            .as_str()?
    } else {
        value
            .get("formulae")?
            .as_array()?
            .first()?
            .get("versions")?
            .get("stable")?
            .as_str()?
    };
    version_triple(version).map(|_| version.to_owned())
}

async fn latest_version(harness: HarnessId, path: &Path) -> Option<String> {
    if let Some(("brew", args)) = update_command(harness, path) {
        return latest_brew_version(path, &args).await;
    }
    let package = package_for(harness)?;
    let url = format!(
        "https://registry.npmjs.org/{}/latest",
        package.replace('@', "%40").replace('/', "%2F")
    );
    let client = reqwest::Client::builder()
        .timeout(REGISTRY_TIMEOUT)
        .build()
        .ok()?;
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() || response.content_length().is_some_and(|len| len > 8192) {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    if bytes.len() > 8192 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .filter(|version| version_triple(version).is_some())
        .map(str::to_owned)
}

/// Exact command for a recognized installation. Unknown paths are manual.
fn update_command(harness: HarnessId, path: &Path) -> Option<(&'static str, Vec<String>)> {
    let package = package_for(harness)?;
    let real = path.canonicalize().ok()?;
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    if let Some(home) = home.as_deref() {
        if harness == HarnessId::ClaudeCode
            && path == home.join(".local/bin/claude")
            && real.starts_with(home.join(".local/share/claude/versions"))
        {
            return Some(("claude", vec!["update".into()]));
        }
        if harness == HarnessId::Opencode
            && path == home.join(".opencode/bin/opencode")
            && real.starts_with(home.join(".opencode/bin"))
        {
            return Some(("opencode", vec!["upgrade".into()]));
        }
    }
    let brew_name = match harness {
        HarnessId::Codex => "codex",
        HarnessId::ClaudeCode => "claude-code",
        HarnessId::Opencode => "opencode",
        _ => return None,
    };
    let mut brew_prefixes = vec![PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")];
    if let Some(prefix) = std::env::var_os("HOMEBREW_PREFIX") {
        brew_prefixes.push(PathBuf::from(prefix));
    }
    for prefix in brew_prefixes {
        if real.starts_with(prefix.join("Caskroom").join(brew_name)) {
            return Some((
                "brew",
                vec!["upgrade".into(), "--cask".into(), brew_name.into()],
            ));
        }
        if real.starts_with(prefix.join("Cellar").join(brew_name)) {
            let formula = if harness == HarnessId::Opencode {
                "anomalyco/tap/opencode"
            } else {
                brew_name
            };
            return Some(("brew", vec!["upgrade".into(), formula.into()]));
        }
    }
    let real_normalized = real.to_string_lossy().replace('\\', "/");
    let marker = format!("/lib/node_modules/{package}/");
    if let Some((prefix, _)) = real_normalized.split_once(&marker) {
        let prefix = PathBuf::from(prefix);
        let binary = match harness {
            HarnessId::Codex => "codex",
            HarnessId::ClaudeCode => "claude",
            HarnessId::Opencode => "opencode",
            _ => return None,
        };
        if path.starts_with(prefix.join("bin"))
            && path.file_stem().and_then(|stem| stem.to_str()) == Some(binary)
        {
            return Some((
                "npm",
                vec![
                    "install".into(),
                    "--global".into(),
                    "--prefix".into(),
                    prefix.to_string_lossy().into_owned(),
                    format!("{package}@latest"),
                ],
            ));
        }
    }
    if let Some(home) = home.as_deref() {
        let name = match harness {
            HarnessId::Codex => "codex",
            HarnessId::ClaudeCode => "claude",
            HarnessId::Opencode => "opencode",
            _ => return None,
        };
        if path == home.join(".bun/bin").join(name) {
            return Some((
                "bun",
                vec!["i".into(), "-g".into(), format!("{package}@latest")],
            ));
        }
        if path == home.join(".local/share/pnpm").join(name) {
            return Some((
                "pnpm",
                vec!["add".into(), "-g".into(), format!("{package}@latest")],
            ));
        }
    }
    None
}

pub(crate) async fn check(force: bool) -> Vec<ProviderUpdateStatus> {
    join_all(PROVIDERS.into_iter().map(|harness| async move {
        if !force {
            let cached = cache()
                .lock()
                .ok()
                .and_then(|cache| cache.get(&harness).cloned());
            if let Some((checked, status)) =
                cached.filter(|(checked, _)| checked.elapsed() < CHECK_TTL)
            {
                let _ = checked;
                return status;
            }
        }
        let path = path_for(harness);
        let installed = if let Some(path) = path.as_deref() {
            installed_version(path).await
        } else {
            None
        };
        let latest = if installed.is_some() {
            if let Some(path) = path.as_deref() {
                latest_version(harness, path).await
            } else {
                None
            }
        } else {
            None
        };
        let action = path
            .as_deref()
            .and_then(|path| update_command(harness, path));
        let available = installed
            .as_deref()
            .and_then(version_triple)
            .zip(latest.as_deref().and_then(version_triple))
            .is_some_and(|(installed, latest)| latest > installed);
        let status = ProviderUpdateStatus {
            harness,
            installed_version: installed,
            latest_version: latest,
            update_available: available,
            update_command: action
                .as_ref()
                .map(|(exe, args)| format!("{exe} {}", args.join(" "))),
            detail: if path.is_none() {
                Some("CLI not installed".into())
            } else if action.is_none() {
                Some("Update this CLI using its original installer".into())
            } else {
                None
            },
        };
        if let Ok(mut cache) = cache().lock() {
            cache.insert(harness, (Instant::now(), status.clone()));
        }
        status
    }))
    .await
}

pub(crate) async fn update(harness: HarnessId) -> Result<String, String> {
    let lock = UPDATE_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = lock
        .try_lock()
        .map_err(|_| "Another CLI update is already running".to_string())?;
    let path = path_for(harness).ok_or("Provider CLI is not installed")?;
    let before = installed_version(&path)
        .await
        .ok_or("Could not read the installed CLI version")?;
    let (executable, args) = update_command(harness, &path)
        .ok_or("This installation has no supported automatic update command")?;
    let mut command = if executable == "claude" || executable == "opencode" {
        Command::new(&path)
    } else {
        Command::new(executable)
    };
    luna_harness::compose_child_path(&mut command, &path);
    command.args(&args).kill_on_drop(true);
    let output = tokio::time::timeout(UPDATE_TIMEOUT, command.output())
        .await
        .map_err(|_| "CLI update timed out".to_string())?
        .map_err(|error| format!("Could not start CLI update: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "CLI update failed: {}",
            stderr
                .chars()
                .rev()
                .take(500)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        ));
    }
    let current_path = path_for(harness).ok_or("Updated CLI is not installed")?;
    let after = installed_version(&current_path)
        .await
        .ok_or("Could not verify CLI version after update")?;
    if version_triple(&after) <= version_triple(&before) {
        return Err(format!(
            "Update finished, but the active CLI still reports {after}"
        ));
    }
    if let Ok(mut cache) = cache().lock() {
        cache.remove(&harness);
    }
    Ok(after)
}
