//! Effective Claude extension configuration for one launch (CC-12): which
//! settings sources load, the output style, hooks, and custom commands/skills
//! with their scope and precedence; plus typed skill dispatch and the
//! fingerprint that invalidates discovery when those files change.
//!
//! The launch policy is never chosen here: [`launch_sources`] reads the argv
//! the adapter actually builds, so a change to Ask/Auto/Full flags (G1, owned
//! by the baseline package) flows through unchanged. Precedence rules were
//! verified against Claude Code 2.1.286 with initialization-only handshakes in
//! isolated config dirs (evidence: `cc12-extensions/probes/init/results.json`):
//!
//! - `--restricted` (and `--safe-mode`) ignore user/project/local settings and
//!   their commands, skills, styles and hooks — even with `--setting-sources`;
//!   managed settings and `--settings` still apply.
//! - Scalar settings (`outputStyle`): managed > `--settings` > local > project > user.
//! - Same-name commands/skills: user beats project; a skill beats a command of
//!   the same scope. `.claude/commands/ns/x.md` is `ns:x`.
//! - Hooks merge across every loaded source; `disableAllHooks` in a loaded
//!   non-managed source turns off every non-managed hook (organization hooks
//!   still run); managed `allowManagedHooksOnly` does the same.
//! - Custom output-style files load only with their source (`user`/`project`).
//!
//! The provider's own initialize answer for the same argv/cwd/config dir
//! ([`apply_native`]) is the authority for what is effective; the file scan
//! explains why (scope, shadowing, ignored sources) and covers what the
//! handshake does not report (hooks).
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use luna_proto::{
    ComposerInput, HarnessId, NativeCommandKind, NativeCommandView, NativeConfiguredValue,
    NativeExtensionsView, NativeHookView, NativeOutputStyleView, NativeSettingsScope,
    NativeSettingsSourceView, NativeStyleOption, PermissionMode, SlashCommand,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::HarnessError;
use crate::process::{Command, Stdio};

/// Built-in output styles reported by Claude Code 2.1.286's initialize.
pub const BUILT_IN_OUTPUT_STYLES: [&str; 5] =
    ["default", "Proactive", "Concise", "Explanatory", "Learning"];
const MAX_SCAN_DEPTH: usize = 4;
const MAX_SCAN_ENTRIES: usize = 2000;
const MAX_FILE_BYTES: u64 = 1 << 20;
const READBACK_TIMEOUT: Duration = Duration::from_secs(10);

/// Flags whose next argv element is a value (skipped while scanning flags).
const VALUE_FLAGS: &[&str] = &[
    "--input-format",
    "--output-format",
    "--thinking-display",
    "--permission-prompt-tool",
    "--model",
    "--effort",
    "--append-system-prompt",
    "--system-prompt",
    "--permission-mode",
    "--tools",
    "--mcp-config",
    "--add-dir",
    "--agents",
    "--plugin-dir",
];

/// Which settings sources a launch loads, read from its own argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSources {
    pub user: bool,
    pub project: bool,
    pub local: bool,
    /// `--settings` value: inline JSON or a file path.
    pub flag: Option<String>,
    /// Why user/project/local are off, when the whole tier is off.
    pub ignored_because: Option<&'static str>,
    pub policy: String,
}

pub fn launch_sources(args: &[String]) -> LaunchSources {
    let mut restricted = false;
    let mut safe_mode = false;
    let mut selected: Option<Vec<String>> = None;
    let mut flag = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (key, inline) = match arg.split_once('=') {
            Some((key, value)) if key.starts_with("--") => (key, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        match key {
            "--restricted" => restricted = true,
            "--safe-mode" => safe_mode = true,
            "--setting-sources" => {
                let value = inline.or_else(|| iter.next().cloned()).unwrap_or_default();
                selected = Some(
                    value
                        .split(',')
                        .map(|source| source.trim().to_ascii_lowercase())
                        .filter(|source| !source.is_empty())
                        .collect(),
                );
            }
            "--settings" => flag = inline.or_else(|| iter.next().cloned()),
            key if inline.is_none() && VALUE_FLAGS.contains(&key) => {
                iter.next();
            }
            _ => {}
        }
    }
    if restricted || safe_mode {
        let (because, policy) = if restricted {
            (
                "ignored: restricted mode",
                "Restricted mode: user, project and local settings files (and their commands, skills, output styles and hooks) are ignored; managed settings and --settings still apply.",
            )
        } else {
            (
                "ignored: safe mode",
                "Safe mode: user, project and local customizations are ignored; managed settings and --settings still apply.",
            )
        };
        return LaunchSources {
            user: false,
            project: false,
            local: false,
            flag,
            ignored_because: Some(because),
            policy: policy.into(),
        };
    }
    match selected {
        Some(sources) => {
            let has = |name: &str| sources.iter().any(|source| source == name);
            let listed = if sources.is_empty() {
                "none".to_owned()
            } else {
                sources.join(", ")
            };
            LaunchSources {
                user: has("user"),
                project: has("project"),
                local: has("local"),
                flag,
                ignored_because: None,
                policy: format!(
                    "Setting sources limited to: {listed}; managed settings and --settings still apply."
                ),
            }
        }
        None => LaunchSources {
            user: true,
            project: true,
            local: true,
            flag,
            ignored_because: None,
            policy: "All setting sources load: user, project and local, under managed policy."
                .into(),
        },
    }
}

/// The filesystem a launch reads its extensions from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionScope {
    /// The launch working directory (project root for `.claude/`).
    pub cwd: Option<PathBuf>,
    /// `$CLAUDE_CONFIG_DIR` or `~/.claude`.
    pub config_dir: PathBuf,
    /// Directory holding `managed-settings.json` and `managed-settings.d/`.
    pub managed_dir: PathBuf,
    /// Used only to shorten display paths.
    pub home: Option<PathBuf>,
}

pub fn default_managed_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/ClaudeCode")
    } else if cfg!(windows) {
        PathBuf::from(r"C:\Program Files\ClaudeCode")
    } else {
        PathBuf::from("/etc/claude-code")
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The config dir a spawned CLI inherits from this process.
pub fn default_config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".claude")))
}

impl ExtensionScope {
    pub fn new(cwd: Option<PathBuf>, config_dir: PathBuf, managed_dir: PathBuf) -> Self {
        Self {
            cwd,
            config_dir,
            managed_dir,
            home: home_dir(),
        }
    }

    fn display(&self, path: &Path) -> String {
        if let Some(cwd) = &self.cwd
            && let Ok(rest) = path.strip_prefix(cwd)
        {
            return rest.display().to_string();
        }
        if let Ok(rest) = path.strip_prefix(&self.config_dir) {
            let base = match &self.home {
                Some(home) if self.config_dir == home.join(".claude") => "~/.claude".to_owned(),
                _ => "$CLAUDE_CONFIG_DIR".to_owned(),
            };
            return format!("{base}/{}", rest.display());
        }
        if path.starts_with(&self.managed_dir) {
            return path.display().to_string();
        }
        match &self.home {
            Some(home) => match path.strip_prefix(home) {
                Ok(rest) => format!("~/{}", rest.display()),
                Err(_) => path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            },
            None => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    fn project_dir(&self) -> Option<PathBuf> {
        self.cwd.as_ref().map(|cwd| cwd.join(".claude"))
    }
}

fn read_bounded(path: &Path) -> Result<Option<String>, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("unreadable: {error}")),
    };
    if !metadata.is_file() {
        return Err("unreadable: not a regular file".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err("unreadable: larger than 1 MiB".into());
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|error| format!("unreadable: {error}"))
}

fn read_settings(path: &Path) -> Result<Option<Value>, String> {
    let Some(text) = read_bounded(path)? else {
        return Ok(None);
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => Ok(Some(value)),
        Ok(_) => Err("unreadable: settings must be a JSON object".into()),
        Err(error) => Err(format!("unreadable: invalid JSON ({error})")),
    }
}

/// Later managed files override scalar keys; hook lists concatenate.
fn merge_settings(into: &mut Value, next: Value) {
    let (Some(into), Value::Object(next)) = (into.as_object_mut(), next) else {
        return;
    };
    for (key, value) in next {
        match (key.as_str(), into.get_mut(&key), value) {
            ("hooks", Some(Value::Object(existing)), Value::Object(hooks)) => {
                for (event, groups) in hooks {
                    match (existing.get_mut(&event), groups) {
                        (Some(Value::Array(list)), Value::Array(more)) => list.extend(more),
                        (_, groups) => {
                            existing.insert(event, groups);
                        }
                    }
                }
            }
            (_, _, value) => {
                into.insert(key, value);
            }
        }
    }
}

struct Source {
    scope: NativeSettingsScope,
    display: String,
    present: bool,
    loaded: bool,
    reason: Option<String>,
    json: Option<Value>,
}

fn load_sources(scope: &ExtensionScope, launch: &LaunchSources) -> Vec<Source> {
    let mut sources = Vec::new();
    // Managed: managed-settings.json, then managed-settings.d/*.json sorted.
    {
        let file = scope.managed_dir.join("managed-settings.json");
        let mut merged: Option<Value> = None;
        let mut errors = Vec::new();
        let mut files = vec![file.clone()];
        if let Ok(entries) = std::fs::read_dir(scope.managed_dir.join("managed-settings.d")) {
            let mut drop_ins: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect();
            drop_ins.sort();
            files.extend(drop_ins);
        }
        for path in &files {
            match read_settings(path) {
                Ok(Some(value)) => match &mut merged {
                    Some(existing) => merge_settings(existing, value),
                    None => merged = Some(value),
                },
                Ok(None) => {}
                Err(error) => errors.push(format!("{}: {error}", scope.display(path))),
            }
        }
        let present = merged.is_some() || !errors.is_empty();
        sources.push(Source {
            scope: NativeSettingsScope::Managed,
            display: if files.len() > 1 {
                format!("{} (+{} drop-in)", scope.display(&file), files.len() - 1)
            } else {
                scope.display(&file)
            },
            present,
            loaded: merged.is_some(),
            reason: if !errors.is_empty() {
                Some(errors.join("; "))
            } else if !present {
                Some("not configured on this machine".into())
            } else {
                None
            },
            json: merged,
        });
    }
    // Flag: inline JSON or a file path.
    if let Some(flag) = &launch.flag {
        let (display, parsed) = if flag.trim_start().starts_with('{') {
            (
                "--settings (inline JSON)".to_owned(),
                serde_json::from_str::<Value>(flag)
                    .map_err(|error| format!("unreadable: invalid JSON ({error})"))
                    .map(Some),
            )
        } else {
            let path = match &scope.cwd {
                Some(cwd) => cwd.join(flag),
                None => PathBuf::from(flag),
            };
            (
                format!("--settings {}", scope.display(&path)),
                read_settings(&path),
            )
        };
        let (json, reason) = match parsed {
            Ok(json) => (json, None),
            Err(error) => (None, Some(error)),
        };
        sources.push(Source {
            scope: NativeSettingsScope::Flag,
            display,
            present: true,
            loaded: json.is_some(),
            reason,
            json,
        });
    }
    let project = scope.project_dir();
    for (tier, enabled, path) in [
        (
            NativeSettingsScope::Local,
            launch.local,
            project.as_ref().map(|dir| dir.join("settings.local.json")),
        ),
        (
            NativeSettingsScope::Project,
            launch.project,
            project.as_ref().map(|dir| dir.join("settings.json")),
        ),
        (
            NativeSettingsScope::User,
            launch.user,
            Some(scope.config_dir.join("settings.json")),
        ),
    ] {
        let Some(path) = path else {
            sources.push(Source {
                scope: tier,
                display: ".claude/".to_owned(),
                present: false,
                loaded: false,
                reason: Some("no project directory for this launch".into()),
                json: None,
            });
            continue;
        };
        let read = read_settings(&path);
        let present = !matches!(read, Ok(None));
        let off_reason = (!enabled).then(|| {
            launch
                .ignored_because
                .unwrap_or("ignored: not in --setting-sources")
                .to_owned()
        });
        let (json, reason) = match read {
            Ok(json) => (json, off_reason),
            Err(error) => (None, Some(off_reason.unwrap_or(error))),
        };
        sources.push(Source {
            scope: tier,
            display: scope.display(&path),
            present,
            // An absent file still "loads" its scope's directories (commands,
            // skills, styles) when the source is enabled.
            loaded: enabled,
            reason: if present || !enabled {
                reason
            } else {
                Some("no settings file".into())
            },
            // Kept for display even when ignored; every consumer checks `loaded`.
            json,
        });
    }
    sources
}

fn source_loaded(sources: &[Source], scope: NativeSettingsScope) -> bool {
    sources
        .iter()
        .any(|source| source.scope == scope && source.loaded)
}

fn source_reason(sources: &[Source], scope: NativeSettingsScope) -> Option<String> {
    sources
        .iter()
        .find(|source| source.scope == scope)
        .and_then(|source| source.reason.clone())
}

/// Minimal YAML front matter reader: `key: value` lines between `---` fences.
fn front_matter(text: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return fields;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
            fields.insert(key.trim().to_ascii_lowercase(), value.to_owned());
        }
    }
    fields
}

/// Bounded walk returning `(relative path, absolute path)` for regular files.
/// Symlinked directories are not followed.
fn walk(root: &Path, accept: &dyn Fn(&Path) -> bool) -> Vec<(PathBuf, PathBuf)> {
    fn visit(
        root: &Path,
        dir: &Path,
        depth: usize,
        accept: &dyn Fn(&Path) -> bool,
        out: &mut Vec<(PathBuf, PathBuf)>,
    ) {
        if depth > MAX_SCAN_DEPTH || out.len() >= MAX_SCAN_ENTRIES {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if out.len() >= MAX_SCAN_ENTRIES {
                return;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                visit(root, &path, depth + 1, accept, out);
            } else if (kind.is_file() || kind.is_symlink()) && path.is_file() && accept(&path) {
                if let Ok(relative) = path.strip_prefix(root) {
                    out.push((relative.to_path_buf(), path.clone()));
                }
            }
        }
    }
    let mut out = Vec::new();
    visit(root, root, 0, accept, &mut out);
    out
}

fn is_markdown(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "md")
}

struct Entry {
    view: NativeCommandView,
    rank: (u8, u8),
}

fn scope_rank(scope: NativeSettingsScope) -> u8 {
    match scope {
        NativeSettingsScope::Managed => 0,
        NativeSettingsScope::User => 1,
        NativeSettingsScope::Project => 2,
        _ => 3,
    }
}

fn kind_label(scope: NativeSettingsScope, kind: NativeCommandKind) -> String {
    format!(
        "{} {}",
        scope.label(),
        match kind {
            NativeCommandKind::Command => "command",
            NativeCommandKind::Skill => "skill",
        }
    )
}

fn scan_commands(scope: &ExtensionScope, sources: &[Source]) -> Vec<NativeCommandView> {
    let mut entries = Vec::new();
    let roots = [
        (NativeSettingsScope::User, Some(scope.config_dir.clone())),
        (NativeSettingsScope::Project, scope.project_dir()),
    ];
    for (tier, root) in roots {
        let Some(root) = root else {
            continue;
        };
        let loaded = source_loaded(sources, tier);
        let reason = (!loaded).then(|| source_reason(sources, tier)).flatten();
        let commands_root = root.join("commands");
        for (relative, path) in walk(&commands_root, &is_markdown) {
            let name = relative
                .with_extension("")
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(":");
            let fields = read_bounded(&path)
                .ok()
                .flatten()
                .map(|text| front_matter(&text))
                .unwrap_or_default();
            entries.push(Entry {
                rank: (scope_rank(tier), 1),
                view: NativeCommandView {
                    name,
                    kind: NativeCommandKind::Command,
                    scope: tier,
                    description: fields.get("description").cloned().unwrap_or_default(),
                    argument_hint: fields
                        .get("argument-hint")
                        .filter(|hint| !hint.is_empty())
                        .cloned(),
                    configured_effective: loaded,
                    native: None,
                    shadowed_by: None,
                    reason: reason.clone(),
                },
            });
        }
        let skills_root = root.join("skills");
        let Ok(dirs) = std::fs::read_dir(&skills_root) else {
            continue;
        };
        let mut dirs: Vec<_> = dirs.filter_map(Result::ok).collect();
        dirs.sort_by_key(|entry| entry.file_name());
        for dir in dirs.into_iter().take(MAX_SCAN_ENTRIES) {
            let file = dir.path().join("SKILL.md");
            let Ok(Some(text)) = read_bounded(&file) else {
                continue;
            };
            let fields = front_matter(&text);
            let name = fields
                .get("name")
                .filter(|name| !name.is_empty())
                .cloned()
                .unwrap_or_else(|| dir.file_name().to_string_lossy().into_owned());
            entries.push(Entry {
                rank: (scope_rank(tier), 0),
                view: NativeCommandView {
                    name,
                    kind: NativeCommandKind::Skill,
                    scope: tier,
                    description: fields.get("description").cloned().unwrap_or_default(),
                    argument_hint: fields
                        .get("argument-hint")
                        .filter(|hint| !hint.is_empty())
                        .cloned(),
                    configured_effective: loaded,
                    native: None,
                    shadowed_by: None,
                    reason: reason.clone(),
                },
            });
        }
    }
    // Resolve name collisions among loaded entries: lowest rank wins.
    let mut winners: HashMap<String, (u8, u8, NativeSettingsScope, NativeCommandKind)> =
        HashMap::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.view.configured_effective)
    {
        let candidate = (
            entry.rank.0,
            entry.rank.1,
            entry.view.scope,
            entry.view.kind,
        );
        winners
            .entry(entry.view.name.clone())
            .and_modify(|current| {
                if (candidate.0, candidate.1) < (current.0, current.1) {
                    *current = candidate;
                }
            })
            .or_insert(candidate);
    }
    let mut out: Vec<NativeCommandView> = entries
        .into_iter()
        .map(|mut entry| {
            if entry.view.configured_effective
                && let Some(winner) = winners.get(&entry.view.name)
                && (winner.0, winner.1) != entry.rank
            {
                entry.view.configured_effective = false;
                entry.view.shadowed_by = Some(kind_label(winner.2, winner.3));
                entry.view.reason = Some(format!(
                    "shadowed by the {} of the same name",
                    kind_label(winner.2, winner.3).to_lowercase()
                ));
            }
            entry.view
        })
        .collect();
    out.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(scope_rank(left.scope).cmp(&scope_rank(right.scope)))
    });
    out
}

fn style_files(dir: &Path, tier: NativeSettingsScope) -> Vec<NativeStyleOption> {
    walk(dir, &is_markdown)
        .into_iter()
        .filter(|(relative, _)| relative.components().count() == 1)
        .map(|(relative, path)| {
            let name = read_bounded(&path)
                .ok()
                .flatten()
                .and_then(|text| front_matter(&text).get("name").cloned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| relative.with_extension("").to_string_lossy().into_owned());
            NativeStyleOption { name, scope: tier }
        })
        .collect()
}

fn output_style(scope: &ExtensionScope, sources: &[Source]) -> NativeOutputStyleView {
    let mut view = NativeOutputStyleView::default();
    let mut applied: Option<NativeSettingsScope> = None;
    for source in sources {
        // Sources are in precedence order: managed, flag, local, project, user.
        // An ignored source still shows what it configures, as not applied.
        let value = source
            .json
            .as_ref()
            .and_then(|json| json.get("outputStyle"))
            .and_then(Value::as_str);
        let Some(value) = value else {
            continue;
        };
        let takes = source.loaded && applied.is_none();
        if takes {
            applied = Some(source.scope);
            view.configured_effective = value.to_owned();
            view.configured_scope = Some(source.scope);
        }
        view.configured.push(NativeConfiguredValue {
            scope: source.scope,
            value: value.to_owned(),
            applied: takes,
            reason: if takes {
                None
            } else if !source.loaded {
                source.reason.clone()
            } else {
                applied.map(|winner| format!("overridden by {}", winner.label().to_lowercase()))
            },
        });
    }
    if applied.is_none() {
        view.configured_effective = "default".into();
    }
    view.available = BUILT_IN_OUTPUT_STYLES
        .iter()
        .map(|name| NativeStyleOption {
            name: (*name).into(),
            scope: NativeSettingsScope::BuiltIn,
        })
        .collect();
    if source_loaded(sources, NativeSettingsScope::User) {
        view.available.extend(style_files(
            &scope.config_dir.join("output-styles"),
            NativeSettingsScope::User,
        ));
    }
    if source_loaded(sources, NativeSettingsScope::Project)
        && let Some(project) = scope.project_dir()
    {
        view.available.extend(style_files(
            &project.join("output-styles"),
            NativeSettingsScope::Project,
        ));
    }
    view
}

/// Display label for a hook handler: its type plus a program or host name.
/// The full command line stays on the host (it may embed secrets).
fn handler_label(handler: &Value) -> String {
    let kind = handler
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("command");
    let detail = match kind {
        "command" => handler
            .get("command")
            .and_then(Value::as_str)
            .and_then(|command| {
                let token = command.split_whitespace().next()?;
                let token = token.trim_matches(|c| c == '"' || c == '\'');
                let name = token.rsplit(['/', '\\']).next().unwrap_or(token);
                (!name.is_empty()).then(|| name.to_owned())
            }),
        "http" => handler
            .get("url")
            .and_then(Value::as_str)
            .and_then(|url| url.split("://").nth(1))
            .and_then(|rest| rest.split(['/', '?', '#']).next())
            .map(|host| {
                host.rsplit_once('@')
                    .map(|(_, host)| host)
                    .unwrap_or(host)
                    .to_owned()
            }),
        _ => None,
    };
    match detail {
        Some(detail) => format!("{kind} · {detail}"),
        None => kind.to_owned(),
    }
}

fn hooks(sources: &[Source]) -> Vec<NativeHookView> {
    let flag_true = |json: Option<&Value>, key: &str| {
        json.and_then(|json| json.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let managed = sources
        .iter()
        .find(|source| source.scope == NativeSettingsScope::Managed)
        .and_then(|source| source.json.as_ref());
    let all_off = flag_true(managed, "disableAllHooks");
    let managed_only = flag_true(managed, "allowManagedHooksOnly");
    let non_managed_off = sources.iter().find(|source| {
        source.scope != NativeSettingsScope::Managed
            && source.loaded
            && flag_true(source.json.as_ref(), "disableAllHooks")
    });
    let mut out = Vec::new();
    for source in sources {
        let Some(hooks) = source
            .json
            .as_ref()
            .and_then(|json| json.get("hooks"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let (effective, reason) = if !source.loaded {
            (false, source.reason.clone())
        } else if all_off {
            (
                false,
                Some("disabled by managed disableAllHooks".to_owned()),
            )
        } else if source.scope != NativeSettingsScope::Managed && managed_only {
            (
                false,
                Some("managed policy allows managed hooks only".to_owned()),
            )
        } else if source.scope != NativeSettingsScope::Managed
            && let Some(off) = non_managed_off
        {
            (
                false,
                Some(format!(
                    "disabled by disableAllHooks in {} settings",
                    off.scope.label().to_lowercase()
                )),
            )
        } else {
            (true, None)
        };
        for (event, groups) in hooks {
            for group in groups.as_array().map(Vec::as_slice).unwrap_or_default() {
                let matcher = group
                    .get("matcher")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|matcher| !matcher.is_empty() && *matcher != "*")
                    .map(str::to_owned);
                for handler in group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    out.push(NativeHookView {
                        event: event.clone(),
                        matcher: matcher.clone(),
                        handler: handler_label(handler),
                        scope: source.scope,
                        effective,
                        reason: reason.clone(),
                    });
                }
            }
        }
    }
    out
}

/// File-based effective view for a launch with `args` in `scope`.
pub fn snapshot(
    scope: &ExtensionScope,
    args: &[String],
    permission_mode: PermissionMode,
) -> NativeExtensionsView {
    let launch = launch_sources(args);
    let sources = load_sources(scope, &launch);
    let view_sources = sources
        .iter()
        .map(|source| NativeSettingsSourceView {
            scope: source.scope,
            display_path: source.display.clone(),
            present: source.present,
            loaded: source.loaded,
            reason: source.reason.clone(),
        })
        .collect();
    NativeExtensionsView {
        provider: HarnessId::ClaudeCode,
        permission_mode,
        launch_policy: launch.policy.clone(),
        sources: view_sources,
        output_style: output_style(scope, &sources),
        hooks: hooks(&sources),
        commands: scan_commands(scope, &sources),
        native_verified: false,
        native_error: None,
        fingerprint: fingerprint(scope, args),
        limitations: vec![
            "Plugin, nested-directory and managed skills are listed only when Claude reports them."
                .into(),
            "Hook rows come from settings files; Claude's initialize does not report hooks.".into(),
        ],
    }
}

/// Split Claude's `description (scope)` suffix into text and scope.
fn native_scope(description: &str) -> (String, Option<NativeSettingsScope>) {
    for (suffix, scope) in [
        (" (user)", NativeSettingsScope::User),
        (" (project)", NativeSettingsScope::Project),
        (" (managed)", NativeSettingsScope::Managed),
    ] {
        if let Some(text) = description.strip_suffix(suffix) {
            return (text.trim().to_owned(), Some(scope));
        }
        if description == suffix.trim() {
            return (String::new(), Some(scope));
        }
    }
    (description.trim().to_owned(), None)
}

/// Merge the provider's own initialize answer for the same launch.
/// `body` is the control_response's inner `response` object.
pub fn apply_native(view: &mut NativeExtensionsView, body: &Value) {
    let commands = body
        .get("commands")
        .and_then(Value::as_array)
        .map(|entries| super::hooks::slash_commands(entries))
        .unwrap_or_default();
    let native: BTreeMap<String, (String, Option<NativeSettingsScope>, &SlashCommand)> = commands
        .iter()
        .map(|command| {
            let (text, scope) = native_scope(&command.description);
            (command.name.clone(), (text, scope, command))
        })
        .collect();
    for row in &mut view.commands {
        row.native = Some(match native.get(&row.name) {
            Some((text, scope, _)) => {
                *scope == Some(row.scope)
                    && (row.description.is_empty() || text.is_empty() || *text == row.description)
            }
            None => false,
        });
        if row.configured_effective && row.native == Some(false) && row.reason.is_none() {
            row.reason = Some("configured, but Claude did not load it for this launch".into());
        }
    }
    for (name, (text, scope, command)) in &native {
        let Some(scope) = scope else {
            continue;
        };
        if view
            .commands
            .iter()
            .any(|row| &row.name == name && row.native == Some(true))
        {
            continue;
        }
        view.commands.push(NativeCommandView {
            name: name.clone(),
            kind: NativeCommandKind::Command,
            scope: *scope,
            description: text.clone(),
            argument_hint: command.input_hint.clone(),
            configured_effective: false,
            native: Some(true),
            shadowed_by: None,
            reason: Some("reported by Claude; no matching file in the scanned roots".into()),
        });
    }
    if let Some(style) = body.get("output_style").and_then(Value::as_str) {
        view.output_style.native = Some(style.to_owned());
        if style != view.output_style.configured_effective {
            view.limitations.push(format!(
                "Claude reports output style \"{style}\"; the settings files resolve to \"{}\". Claude's answer is authoritative.",
                view.output_style.configured_effective
            ));
        }
    }
    view.output_style.native_available = body
        .get("available_output_styles")
        .and_then(Value::as_array)
        .map(|styles| {
            styles
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
    view.native_verified = true;
    view.native_error = None;
}

fn stamp(hasher: &mut Sha256, path: &Path) {
    hasher.update(path.to_string_lossy().as_bytes());
    match std::fs::metadata(path) {
        Ok(metadata) => {
            hasher.update(metadata.len().to_le_bytes());
            if let Ok(modified) = metadata.modified()
                && let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH)
            {
                hasher.update(since.as_nanos().to_le_bytes());
            }
        }
        Err(_) => hasher.update(b"absent"),
    }
}

/// Changes whenever a settings file or a command/skill/style file that this
/// launch could read is added, removed or edited (path + length + mtime).
pub fn fingerprint(scope: &ExtensionScope, args: &[String]) -> String {
    let launch = launch_sources(args);
    let mut hasher = Sha256::new();
    hasher.update(format!("{launch:?}").as_bytes());
    let managed = scope.managed_dir.join("managed-settings.json");
    stamp(&mut hasher, &managed);
    stamp(&mut hasher, &scope.managed_dir.join("managed-settings.d"));
    for (relative, path) in walk(&scope.managed_dir.join("managed-settings.d"), &|_| true) {
        hasher.update(relative.to_string_lossy().as_bytes());
        stamp(&mut hasher, &path);
    }
    let mut roots = vec![scope.config_dir.clone()];
    if let Some(project) = scope.project_dir() {
        roots.push(project);
    }
    for root in roots {
        for file in ["settings.json", "settings.local.json"] {
            stamp(&mut hasher, &root.join(file));
        }
        for dir in ["commands", "skills", "output-styles"] {
            let dir = root.join(dir);
            stamp(&mut hasher, &dir);
            for (_, path) in walk(&dir, &|_| true) {
                stamp(&mut hasher, &path);
            }
        }
    }
    let digest = hasher.finalize();
    digest
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The launch argv as strings (what [`launch_sources`] reads).
pub(crate) fn command_args(command: &Command) -> Vec<String> {
    command
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

/// The launch argv for an initialize-only readback: no bypass flag (it
/// changes no settings source), and `disableAllHooks` merged into the one
/// `--settings` layer — `initialize` alone runs SessionStart hooks, and hooks
/// do not change the commands or styles it reports.
pub(crate) fn readback_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len() + 2);
    let mut merged = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dangerously-skip-permissions" => {}
            "--permission-mode" => {
                out.push(arg.clone());
                if let Some(mode) = iter.next() {
                    out.push(if mode == "bypassPermissions" {
                        "default".into()
                    } else {
                        mode.clone()
                    });
                }
            }
            "--settings" => {
                let value = iter.next().cloned().unwrap_or_default();
                out.push(arg.clone());
                match serde_json::from_str::<Value>(&value) {
                    Ok(Value::Object(mut map)) => {
                        map.insert("disableAllHooks".into(), Value::Bool(true));
                        out.push(Value::Object(map).to_string());
                        merged = true;
                    }
                    // A settings file path stays; the extra layer below
                    // disables hooks.
                    _ => out.push(value),
                }
            }
            _ => out.push(arg.clone()),
        }
    }
    if !merged {
        out.push("--settings".into());
        out.push(r#"{"disableAllHooks":true}"#.into());
    }
    out
}

/// Run only the `initialize` control request under a launch's own argv, cwd
/// and environment: no user message, no model turn. Returns the inner
/// `response` object of the matching control_response.
pub(crate) async fn initialize_readback(
    exe: &Path,
    args: &[String],
    cwd: Option<&Path>,
    config_dir: Option<&Path>,
    kill_grace: Duration,
) -> Result<Value, HarnessError> {
    let mut cmd = Command::new(exe);
    crate::compose_child_path(&mut cmd, exe);
    cmd.args(readback_args(args));
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    if let Some(dir) = config_dir {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            HarnessError::NotInstalled(exe.display().to_string())
        } else {
            HarnessError::Io(error)
        }
    })?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        crate::shutdown_child(&mut child, kill_grace).await;
        return Err(HarnessError::Protocol("claude child has no stdio".into()));
    };
    const READBACK_ID: &str = "luna-extension-readback";
    let exchange = async {
        let request = serde_json::json!({
            "type": "control_request",
            "request_id": READBACK_ID,
            "request": { "subtype": "initialize" },
        });
        stdin
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(HarnessError::Io)?;
        stdin.flush().await.map_err(HarnessError::Io)?;
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await.map_err(HarnessError::Io)? {
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if frame.get("type").and_then(Value::as_str) != Some("control_response") {
                continue;
            }
            let response = frame.get("response").cloned().unwrap_or(Value::Null);
            if response.get("request_id").and_then(Value::as_str) != Some(READBACK_ID) {
                continue;
            }
            if response.get("subtype").and_then(Value::as_str) == Some("error") {
                return Err(HarnessError::Protocol(
                    response
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("initialize control request failed")
                        .into(),
                ));
            }
            return Ok(response.get("response").cloned().unwrap_or(Value::Null));
        }
        Err(HarnessError::Protocol(
            "claude exited before answering the initialize control request".into(),
        ))
    };
    let result = tokio::time::timeout(READBACK_TIMEOUT, exchange).await;
    crate::shutdown_child(&mut child, kill_grace).await;
    result.unwrap_or_else(|_| {
        Err(HarnessError::Protocol(
            "Claude initialization timed out".into(),
        ))
    })
}

/// A Claude-valid command name: no leading slash, whitespace or control text.
fn valid_command_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
}

/// The one skill a Claude turn invokes, or `None` without typed selections.
/// Plugin/app mentions and multiple skills are rejected before acceptance
/// (the caller keeps the draft): Claude expands one leading slash command.
pub fn typed_skill(selections: &[ComposerInput]) -> Result<Option<String>, HarnessError> {
    if selections.is_empty() {
        return Ok(None);
    }
    let mut skills = Vec::new();
    for selection in selections {
        match selection {
            ComposerInput::Mention { .. } => {
                return Err(HarnessError::RejectedBeforeAcceptance(
                    "Claude cannot receive plugin or app selections. Remove them or choose Codex; your draft is kept.".into(),
                ));
            }
            ComposerInput::Skill { name, .. } => skills.push(name.trim().trim_start_matches('/')),
        }
    }
    if skills.len() > 1 {
        return Err(HarnessError::RejectedBeforeAcceptance(
            "Claude runs one skill or command per message. Keep one selection; your draft is kept."
                .into(),
        ));
    }
    let name = skills[0];
    if !valid_command_name(name) {
        return Err(HarnessError::RejectedBeforeAcceptance(format!(
            "\"{name}\" is not a valid Claude command name; your draft is kept."
        )));
    }
    crate::validate_composer_inputs(&[], selections)?;
    Ok(Some(name.to_owned()))
}

/// Native invocation text: Claude expands a slash command only when the
/// message's (last) text block begins with it, so the selection leads and the
/// prose follows as its arguments. An existing identical prefix is kept.
pub fn skill_invocation_text(name: &str, text: &str) -> String {
    let command = format!("/{name}");
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix(&command)
        && (rest.is_empty() || rest.starts_with(char::is_whitespace))
    {
        return trimmed.to_owned();
    }
    if trimmed.is_empty() {
        command
    } else {
        format!("{command} {trimmed}")
    }
}

/// Reject a typed skill that the effective catalog (`body`, the launch's own
/// initialize answer) does not list. Callers skip this check when the
/// readback itself failed: an unverifiable catalog never blocks dispatch.
pub fn check_effective_skill(
    name: &str,
    body: &Value,
    launch: &LaunchSources,
) -> Result<(), HarnessError> {
    let listed = body
        .get("commands")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        });
    if listed {
        return Ok(());
    }
    let why = match launch.ignored_because {
        Some(_) => format!(
            " {} Choose Full access or install it as a managed skill.",
            launch.policy
        ),
        None => String::new(),
    };
    Err(HarnessError::RejectedBeforeAcceptance(format!(
        "Claude does not load /{name} for this chat's launch.{why} Your draft is kept."
    )))
}

/// Verified views per (cwd, mode). Only provider-verified views are kept, so a
/// failed readback retries on the next request.
#[derive(Default)]
pub(crate) struct ViewCache(
    std::sync::Mutex<HashMap<(Option<PathBuf>, PermissionMode), NativeExtensionsView>>,
);

impl ViewCache {
    fn get(&self, key: &(Option<PathBuf>, PermissionMode)) -> Option<NativeExtensionsView> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .cloned()
    }

    fn put(&self, key: (Option<PathBuf>, PermissionMode), view: NativeExtensionsView) {
        let mut views = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if views.len() >= 64 {
            views.clear();
        }
        views.insert(key, view);
    }
}

/// A launch-shaped request for `cwd` under `mode`: only what selects the
/// launch argv. JSON-built so later additive `RunRequest` fields default.
fn launch_probe_request(
    cwd: Option<&Path>,
    mode: PermissionMode,
) -> Result<luna_proto::RunRequest, HarnessError> {
    let mut request: luna_proto::RunRequest = serde_json::from_value(serde_json::json!({
        "prompt": "",
        "cwd": cwd.map(|cwd| cwd.to_string_lossy().into_owned()).unwrap_or_default(),
        "sandbox": "workspace-write",
        "model": null,
        "reasoning": null,
        "resume": null,
    }))
    .map_err(|error| HarnessError::Protocol(format!("extension probe request: {error}")))?;
    request.permission_policy = Some(luna_proto::PermissionPolicy {
        requested_mode: mode,
        effective_mode: mode,
        ..Default::default()
    });
    Ok(request)
}

impl super::ClaudeHarness {
    /// Read managed settings from this directory instead of the platform
    /// location (tests/fixtures; the real CLI always reads the platform path).
    pub fn with_managed_settings_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.managed_dir = Some(dir.into());
        self
    }

    pub(super) fn extension_scope(&self, cwd: Option<PathBuf>) -> ExtensionScope {
        ExtensionScope::new(
            cwd,
            self.config_home
                .clone()
                .or_else(default_config_dir)
                .unwrap_or_default(),
            self.managed_dir.clone().unwrap_or_else(default_managed_dir),
        )
    }

    pub(super) async fn native_extensions_view(
        &self,
        cwd: Option<&Path>,
        mode: PermissionMode,
        refresh: bool,
    ) -> Result<NativeExtensionsView, HarnessError> {
        let exe = self.resolve_executable()?;
        let request = launch_probe_request(cwd, mode)?;
        let mut cmd = self.build_command(&exe, &request);
        // The same opt-in a normal (non-title) run adds.
        cmd.arg("--include-hook-events");
        let args = command_args(&cmd);
        let scope = self.extension_scope(cwd.map(Path::to_path_buf));
        let key = (cwd.map(Path::to_path_buf), mode);
        let mut view = snapshot(&scope, &args, mode);
        if !refresh
            && let Some(cached) = self.extension_views.get(&key)
            && cached.fingerprint == view.fingerprint
        {
            return Ok(cached);
        }
        match initialize_readback(
            &exe,
            &args,
            cwd,
            self.config_home.as_deref(),
            self.kill_grace,
        )
        .await
        {
            Ok(body) => {
                apply_native(&mut view, &body);
                self.extension_views.put(key, view.clone());
            }
            Err(error) => view.native_error = Some(error.to_string()),
        }
        Ok(view)
    }

    /// The typed skill this turn invokes, checked against the launch's own
    /// effective catalog (same argv/cwd/config dir) before any user message
    /// is written. An unreadable catalog does not block dispatch.
    pub(super) async fn typed_skill_for_launch(
        &self,
        exe: &Path,
        cmd: &Command,
        request: &luna_proto::RunRequest,
    ) -> Result<Option<String>, HarnessError> {
        let Some(name) = typed_skill(&request.input_items)? else {
            return Ok(None);
        };
        let args = command_args(cmd);
        let cwd = (!request.cwd.is_empty()).then(|| Path::new(&request.cwd));
        match initialize_readback(
            exe,
            &args,
            cwd,
            self.config_home.as_deref(),
            self.kill_grace,
        )
        .await
        {
            Ok(body) => check_effective_skill(&name, &body, &launch_sources(&args))?,
            Err(error) => tracing::warn!(
                target: "luna_harness::claude",
                %error,
                "could not verify the effective command catalog; dispatching /{name}"
            ),
        }
        Ok(Some(name))
    }
}
