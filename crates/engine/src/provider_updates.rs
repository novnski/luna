//! Device-local CLI maintenance. Checks are bounded and cached; updates run
//! only after an explicit UI action and are verified against the same binary
//! path used to launch tasks.
//!
//! OpenCode is generation-aware (OC-17): the binary is the OpenCode 2 the
//! harness resolver selects (1.x is never resolved, PD-15); `~/.opencode/bin`
//! upgrades with `opencode upgrade`, Homebrew installs with their own Cellar
//! formula (`anomalyco/tap/opencode-v2`, `opencode-beta`, or the receipt's
//! tap), npm/bun/pnpm installs with `@opencode/cli`; a "latest" from another
//! major version is never offered.

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
    match (harness).kind() {
        luna_proto::BuiltinHarnessId::Codex => luna_harness::codex::resolve_codex_executable(),
        luna_proto::BuiltinHarnessId::ClaudeCode => {
            luna_harness::claude::resolve_claude_executable()
        }
        luna_proto::BuiltinHarnessId::Opencode => {
            luna_harness::opencode::resolve_opencode_executable()
        }
        _ => None,
    }
}

/// The npm package of the generation LUNA runs. OpenCode 2 ships as
/// `@opencode/cli` (npm `latest` and the update API's `latest/cli/npm`
/// agree); `opencode-ai@latest` is still OpenCode 1.x and is never offered
/// (OC-17, PD-15).
fn package_for(harness: HarnessId) -> Option<&'static str> {
    match (harness).kind() {
        luna_proto::BuiltinHarnessId::Codex => Some("@openai/codex"),
        luna_proto::BuiltinHarnessId::ClaudeCode => Some("@anthropic-ai/claude-code"),
        luna_proto::BuiltinHarnessId::Opencode => Some("@opencode/cli"),
        _ => None,
    }
}

/// Homebrew formulas that install OpenCode. `opencode-v2` and
/// `opencode-beta` are OpenCode 2 channels of the `anomalyco/tap`; plain
/// `opencode` is whatever its tap ships (the tap's is 1.x, which the
/// OpenCode resolver never selects).
const OPENCODE_FORMULAS: [&str; 3] = ["opencode-v2", "opencode-beta", "opencode"];

/// The fully qualified formula of a Cellar install, from its install
/// receipt's tap (`homebrew/core` needs no qualification). Without a
/// readable receipt, OpenCode's own channels fall back to `anomalyco/tap`.
fn brew_formula(cellar: &Path, name: &str, real: &Path) -> String {
    let receipt_tap = real
        .strip_prefix(cellar.join(name))
        .ok()
        .and_then(|rest| rest.components().next())
        .map(|version| {
            cellar
                .join(name)
                .join(version.as_os_str())
                .join("INSTALL_RECEIPT.json")
        })
        .and_then(|receipt| std::fs::read(receipt).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|receipt| {
            receipt
                .pointer("/source/tap")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    match receipt_tap.as_deref() {
        Some("homebrew/core") => name.to_owned(),
        Some(tap) if !tap.is_empty() => format!("{tap}/{name}"),
        _ => format!("anomalyco/tap/{name}"),
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
    let brew_name = match (harness).kind() {
        luna_proto::BuiltinHarnessId::Codex => "codex",
        luna_proto::BuiltinHarnessId::ClaudeCode => "claude-code",
        luna_proto::BuiltinHarnessId::Opencode => "opencode",
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
        let cellar = prefix.join("Cellar");
        if harness == HarnessId::Opencode {
            // The Cellar name decides the channel: `opencode-v2` and
            // `opencode-beta` are OpenCode 2 (`Cellar/opencode-v2` does not
            // start with `Cellar/opencode/`, so each name is matched whole).
            if let Some(name) = OPENCODE_FORMULAS
                .iter()
                .find(|name| real.starts_with(cellar.join(name)))
            {
                let formula = brew_formula(&cellar, name, &real);
                return Some(("brew", vec!["upgrade".into(), formula]));
            }
            continue;
        }
        if real.starts_with(cellar.join(brew_name)) {
            return Some(("brew", vec!["upgrade".into(), brew_name.into()]));
        }
    }
    let real_normalized = real.to_string_lossy().replace('\\', "/");
    let marker = format!("/lib/node_modules/{package}/");
    if let Some((prefix, _)) = real_normalized.split_once(&marker) {
        let prefix = PathBuf::from(prefix);
        let binary = match (harness).kind() {
            luna_proto::BuiltinHarnessId::Codex => "codex",
            luna_proto::BuiltinHarnessId::ClaudeCode => "claude",
            luna_proto::BuiltinHarnessId::Opencode => "opencode",
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
        let name = match (harness).kind() {
            luna_proto::BuiltinHarnessId::Codex => "codex",
            luna_proto::BuiltinHarnessId::ClaudeCode => "claude",
            luna_proto::BuiltinHarnessId::Opencode => "opencode",
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

/// The OC-02 compatibility class of an OpenCode version this process has
/// booted (known-good, untested, or incompatible with the reason).
fn opencode_compat_detail(version: &str) -> Option<String> {
    use luna_harness::opencode::CompatStatus;
    match luna_harness::opencode::opencode_compat_report(version)? {
        Ok(report) if report.status == CompatStatus::KnownGood => {
            Some(format!("OpenCode {version}: API contract verified by LUNA"))
        }
        Ok(_) => Some(format!(
            "OpenCode {version}: untested by LUNA; its API contract checks passed"
        )),
        Err(reason) => Some(reason),
    }
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
            .is_some_and(|(installed, latest)| {
                // OpenCode: never offer another generation than the one LUNA
                // runs (a 1.x "latest" for a 2.x binary, OC-17).
                latest > installed && (harness != HarnessId::Opencode || latest.0 == installed.0)
            });
        let status = ProviderUpdateStatus {
            harness,
            installed_version: installed.clone(),
            latest_version: latest,
            update_available: available,
            update_command: action
                .as_ref()
                .map(|(exe, args)| format!("{exe} {}", args.join(" "))),
            detail: if path.is_none() {
                Some(if harness == HarnessId::Opencode {
                    "OpenCode 2 not installed (OpenCode 1.x is not supported)".into()
                } else {
                    "CLI not installed".into()
                })
            } else if action.is_none() {
                Some("Update this CLI using its original installer".into())
            } else if harness == HarnessId::Opencode {
                installed.as_deref().and_then(opencode_compat_detail)
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
