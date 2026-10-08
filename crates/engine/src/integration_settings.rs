//! LUNA-owned instructions and provider-owned skill library.
//!
//! Every provider path is supplied by engine assembly. This module never
//! discovers a credential home from HOME, CODEX_HOME, or a native login.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use luna_harness::codex::native::{CodexMcpConfigEntry, CodexSkillCatalogEntry};
use luna_proto::{
    HarnessId, InstructionSettingsView, IntegrationActionAvailability, IntegrationActionFacts,
    IntegrationHelpRoute, IntegrationInventoryItem, IntegrationInventoryView, IntegrationKind,
    IntegrationOwner, IntegrationSkillScope, SkillDocumentView,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::EngineError;
use crate::runtime_contract::InstructionContribution;

const MAX_INSTRUCTION_CHARS: usize = 16_000;
const MAX_SKILL_BYTES: usize = 128 * 1024;
const MAX_SKILL_FILES: usize = 128;
const MAX_INSTALL_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SKILL_DEPTH: usize = 8;
// One engine process owns the local IPC data root. This serializes LUNA writes;
// external provider/editor writes are caught by revision checks when observed.
static SKILL_WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Identifier only, never an authorization token. Length framing prevents
/// ambiguous tuples; the private source path is never returned to the UI.
fn provider_identity(
    provider: HarnessId,
    machine_id: &str,
    source: &str,
    root: &Path,
    name: &str,
) -> String {
    let mut hash = Sha256::new();
    let root_text = root.to_string_lossy();
    for value in [
        "luna-integration-v1",
        machine_id,
        match provider {
            HarnessId::Codex => "codex",
            HarnessId::ClaudeCode => "claude-code",
            HarnessId::Opencode => "opencode",
            _ => "unsupported",
        },
        source,
        root_text.as_ref(),
        name,
    ] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    format!("intg_v1_{:x}", hash.finalize())
}

fn inventory_identity(machine_id: &str, source: &str, root: &Path, name: &str) -> String {
    provider_identity(HarnessId::Codex, machine_id, source, root, name)
}

/// Explicit engine-assembled provider locations. The selected Codex account
/// home, Claude config home and OpenCode config home remain separate owners.
pub struct SkillRoots {
    pub codex_home: Option<PathBuf>,
    pub claude_home: PathBuf,
    pub opencode_home: PathBuf,
    pub project_root: Option<PathBuf>,
    pub machine_id: String,
}

struct SkillLocation {
    base: PathBuf,
    root: PathBuf,
    identity_root: PathBuf,
    source: &'static str,
    source_id: &'static str,
    scope: IntegrationSkillScope,
}

impl SkillRoots {
    fn selected_project_root(&self) -> Result<&PathBuf, EngineError> {
        let root = self.project_root.as_ref().ok_or_else(no_project_root)?;
        if !root.is_absolute()
            || !root.is_dir()
            || std::fs::symlink_metadata(root)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(EngineError::Other(
                "Selected project folder is unavailable or symlinked".into(),
            ));
        }
        Ok(root)
    }

    fn location(
        &self,
        provider: HarnessId,
        scope: IntegrationSkillScope,
    ) -> Result<SkillLocation, EngineError> {
        let (base, suffix, source, source_id) = match (provider, scope) {
            (HarnessId::Codex, IntegrationSkillScope::Global) => (
                self.codex_home.as_ref().ok_or_else(|| {
                    EngineError::Other("Selected Codex skill home is unavailable".into())
                })?,
                "skills",
                "Codex home / skills",
                "codex-home-skill",
            ),
            (HarnessId::Codex, IntegrationSkillScope::Project) => (
                self.selected_project_root()?,
                ".agents/skills",
                "project / .agents/skills",
                "project-agents-skill",
            ),
            (HarnessId::ClaudeCode, IntegrationSkillScope::Global) => (
                &self.claude_home,
                "skills",
                "Claude config / skills",
                "claude-home-skill",
            ),
            (HarnessId::ClaudeCode, IntegrationSkillScope::Project) => (
                self.selected_project_root()?,
                ".claude/skills",
                "project / .claude/skills",
                "project-claude-skill",
            ),
            (HarnessId::Opencode, IntegrationSkillScope::Global) => (
                &self.opencode_home,
                "skills",
                "OpenCode config / skills",
                "opencode-home-skill",
            ),
            (HarnessId::Opencode, IntegrationSkillScope::Project) => (
                self.selected_project_root()?,
                ".opencode/skills",
                "project / .opencode/skills",
                "project-opencode-skill",
            ),
            _ => return Err(EngineError::Other("Unsupported skill provider".into())),
        };
        if !base.is_absolute() {
            return Err(EngineError::Other(
                "Provider skill home is not an absolute path".into(),
            ));
        }
        let identity_root = base.join(suffix);
        // A global provider config may deliberately link its skills folder to
        // a shared user-owned library. Resolve that selected root once, then
        // reject symlinks beneath it. Project roots never get this exception.
        let linked_global_root = scope == IntegrationSkillScope::Global
            && std::fs::symlink_metadata(&identity_root)
                .is_ok_and(|metadata| metadata.file_type().is_symlink());
        let root = if linked_global_root {
            let canonical = identity_root.canonicalize()?;
            if !canonical.is_dir() {
                return Err(EngineError::Other(
                    "Global skill root link does not point to a directory".into(),
                ));
            }
            canonical
        } else {
            identity_root.clone()
        };
        Ok(SkillLocation {
            base: if linked_global_root {
                root.clone()
            } else {
                base.clone()
            },
            root,
            identity_root,
            source,
            source_id,
            scope,
        })
    }
}

fn no_project_root() -> EngineError {
    EngineError::Other("Select a project for project skills".into())
}

pub fn skill_inventory(provider: HarnessId, roots: &SkillRoots) -> IntegrationInventoryView {
    let mut view = if provider == HarnessId::Codex {
        roots.codex_home.as_ref().map_or_else(
            || unavailable_inventory(provider, &roots.machine_id),
            |home| codex_inventory(home, roots.project_root.as_deref(), &roots.machine_id),
        )
    } else {
        unavailable_inventory(provider, &roots.machine_id)
    };
    if !matches!(
        provider,
        HarnessId::Codex | HarnessId::ClaudeCode | HarnessId::Opencode
    ) {
        return view;
    }
    if provider != HarnessId::Codex {
        view.items.clear();
        view.unavailable_reason = None;
    }
    for scope in [
        IntegrationSkillScope::Global,
        IntegrationSkillScope::Project,
    ] {
        let Ok(location) = roots.location(provider, scope) else {
            continue;
        };
        if provider == HarnessId::Codex {
            continue; // Keep Codex's existing native catalog identity and MCP inventory.
        }
        let Ok(entries) = std::fs::read_dir(&location.root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.path().join("SKILL.md").is_file() {
                continue;
            }
            view.items.push(IntegrationInventoryItem {
                identity: Some(provider_identity(
                    provider,
                    &roots.machine_id,
                    location.source_id,
                    &location.identity_root,
                    &name,
                )),
                name,
                kind: IntegrationKind::Skill,
                source: location.source.into(),
                scope: match scope {
                    IntegrationSkillScope::Global => "global".into(),
                    IntegrationSkillScope::Project => "project".into(),
                },
                configured: true,
                available: None,
                attached: None,
                reason: Some("Found on disk; provider availability and selected-session attachment are unverified.".into()),
                owner: IntegrationOwner::ProviderNative,
                actions: unsupported_actions(),
                help_route: IntegrationHelpRoute::SkillSource { scope },
                mcp: None,
            });
        }
    }
    view.items.sort_by(|a, b| {
        (&a.kind, &a.scope, &a.name, &a.identity).cmp(&(&b.kind, &b.scope, &b.name, &b.identity))
    });
    if provider != HarnessId::Codex {
        view.help = "This library reads the provider's global and selected project's skill folders. File presence does not prove provider availability or selected-session attachment. MCP connections remain with their provider or Executor.".into();
    }
    view
}

fn unsupported_actions() -> IntegrationActionFacts {
    IntegrationActionFacts {
        enable: IntegrationActionAvailability::Unavailable,
        disable: IntegrationActionAvailability::Unavailable,
        authenticate: IntegrationActionAvailability::Unavailable,
        reason: Some(
            "Enable, disable, and authentication actions are unavailable for this source.".into(),
        ),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SettingsFile {
    revision: u64,
    global: String,
    projects: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct IntegrationSettings {
    path: PathBuf,
    state: Arc<Mutex<SettingsFile>>,
}

impl IntegrationSettings {
    pub fn open(path: PathBuf) -> Result<Self, EngineError> {
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => SettingsFile::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            path,
            state: Arc::new(Mutex::new(state)),
        })
    }

    pub fn view(&self, project_id: Option<&str>) -> InstructionSettingsView {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        InstructionSettingsView {
            revision: state.revision,
            global: state.global.clone(),
            project_id: project_id.map(str::to_owned),
            project: project_id
                .and_then(|id| state.projects.get(id))
                .cloned()
                .unwrap_or_default(),
        }
    }

    pub fn contributions(
        &self,
        project_id: Option<&str>,
    ) -> (Vec<InstructionContribution>, Vec<InstructionContribution>) {
        let view = self.view(project_id);
        let global = (!view.global.trim().is_empty())
            .then(|| InstructionContribution {
                name: "LUNA global instructions".into(),
                provenance: "LUNA settings / global".into(),
                text: view.global,
            })
            .into_iter()
            .collect();
        let project = (!view.project.trim().is_empty())
            .then(|| InstructionContribution {
                name: "LUNA project instructions".into(),
                provenance: format!("LUNA settings / project {}", project_id.unwrap_or_default()),
                text: view.project,
            })
            .into_iter()
            .collect();
        (global, project)
    }

    pub fn save(
        &self,
        project_id: Option<&str>,
        text: &str,
        expected_revision: u64,
    ) -> Result<InstructionSettingsView, EngineError> {
        if text.chars().count() > MAX_INSTRUCTION_CHARS || text.contains('\0') {
            return Err(EngineError::Other(
                "LUNA instructions must be at most 16,000 characters and contain no NUL".into(),
            ));
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.revision != expected_revision {
            return Err(EngineError::Other(
                "LUNA instructions changed elsewhere; reload before saving".into(),
            ));
        }
        let mut next = state.clone();
        if let Some(id) = project_id {
            if id.is_empty() || id.len() > 200 {
                return Err(EngineError::Other("Invalid LUNA project id".into()));
            }
            if text.trim().is_empty() {
                next.projects.remove(id);
            } else {
                next.projects.insert(id.into(), text.into());
            }
        } else {
            next.global = text.into();
        }
        if next.global == state.global && next.projects == state.projects {
            return Ok(InstructionSettingsView {
                revision: state.revision,
                global: state.global.clone(),
                project_id: project_id.map(str::to_owned),
                project: project_id
                    .and_then(|id| state.projects.get(id))
                    .cloned()
                    .unwrap_or_default(),
            });
        }
        next.revision = next.revision.saturating_add(1);
        let parent = self.path.parent().ok_or_else(|| {
            EngineError::Other("LUNA instructions path has no parent directory".into())
        })?;
        std::fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(temporary.as_file_mut(), &next)?;
        temporary.as_file_mut().sync_all()?;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        *state = next;
        drop(state);
        Ok(self.view(project_id))
    }
}

/// Inventory of the source that Codex owns. A file's presence establishes
/// configuration on disk, never provider availability or session attachment.
/// No CLI, login, MCP server, or gateway is started here.
pub fn codex_inventory(
    codex_home: &Path,
    project_root: Option<&Path>,
    machine_id: &str,
) -> IntegrationInventoryView {
    let mut items = Vec::new();
    for (root, scope, source, source_id, owner, help_route) in [
        (
            Some(codex_home.join("skills")),
            "global",
            "Codex home / skills",
            "codex-home-skill",
            IntegrationOwner::ProviderNative,
            IntegrationHelpRoute::SkillSource {
                scope: IntegrationSkillScope::Global,
            },
        ),
        (
            project_root.map(|p| p.join(".agents/skills")),
            "project",
            "project / .agents/skills",
            "project-agents-skill",
            IntegrationOwner::Unknown,
            IntegrationHelpRoute::SkillSource {
                scope: IntegrationSkillScope::Project,
            },
        ),
    ] {
        let Some(root) = root else { continue };
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path().join("SKILL.md");
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            items.push(IntegrationInventoryItem {
                identity: Some(inventory_identity(machine_id, source_id, &root, &name)),
                name,
                kind: IntegrationKind::Skill,
                source: source.into(),
                scope: scope.into(),
                configured: true,
                available: None,
                attached: None,
                reason: Some(
                    "Found on disk; provider availability and session inclusion are unverified."
                        .into(),
                ),
                owner,
                actions: unsupported_actions(),
                help_route: help_route.clone(),
                mcp: None,
            });
        }
    }
    let mut unavailable_reason = None;
    match std::fs::read_to_string(codex_home.join("config.toml")) {
        Ok(config) => match config.parse::<toml::Value>() {
            Ok(config) => {
                if let Some(servers) = config.get("mcp_servers").and_then(toml::Value::as_table) {
                    for (name, value) in servers {
                        let enabled = value
                            .get("enabled")
                            .and_then(toml::Value::as_bool)
                            .unwrap_or(true);
                        items.push(IntegrationInventoryItem {
                            identity: Some(inventory_identity(
                                machine_id,
                                "codex-mcp-config",
                                &codex_home.join("config.toml"),
                                name,
                            )),
                            name: name.clone(),
                            kind: IntegrationKind::McpServer,
                            source: "Codex home / config.toml".into(),
                            scope: "global".into(),
                            configured: true,
                            available: if enabled { None } else { Some(false) },
                            attached: None,
                            reason: Some(if enabled {
                                "Found in Codex home config.toml; selected-workspace configuration, server health, and session attachment are unverified.".into()
                            } else {
                                "Disabled in Codex home config.toml; a selected workspace may override it, and an existing session may still have it loaded.".into()
                            }),
                            owner: IntegrationOwner::ProviderNative,
                            actions: unsupported_actions(),
                            help_route: IntegrationHelpRoute::ProviderMcpConfig,
                            mcp: None,
                        });
                    }
                }
            }
            Err(_) => {
                unavailable_reason = Some(
                    "Codex configuration could not be parsed; MCP inventory is unavailable.".into(),
                )
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            unavailable_reason =
                Some("Codex configuration could not be read; MCP inventory is unavailable.".into())
        }
    }
    items.sort_by(|a, b| {
        (&a.kind, &a.scope, &a.name, &a.identity).cmp(&(&b.kind, &b.scope, &b.name, &b.identity))
    });
    IntegrationInventoryView {
        provider: HarnessId::Codex,
        machine_id: machine_id.into(),
        items,
        unavailable_reason,
        help: "This read covers Codex-home skills, project .agents/skills, and mcp_servers in Codex config.toml. LUNA can manage regular skill folders here; provider availability and selected-session attachment remain separate checks. MCP servers stay with Codex or Executor.".into(),
    }
}

/// Enrich file discovery only with an exact native `skills/list` observation.
/// The catalog process is separate from the selected task, so attachment stays
/// unknown even for an enabled skill. Provider paths are used only for matching
/// and opaque identity; they never enter the returned view.
pub fn apply_codex_skill_catalog(
    view: &mut IntegrationInventoryView,
    catalog: &[CodexSkillCatalogEntry],
    codex_home: &Path,
    project_root: Option<&Path>,
) {
    fn same_file(left: &Path, right: &Path) -> bool {
        left == right
            || matches!((left.canonicalize(), right.canonicalize()), (Ok(a), Ok(b)) if a == b)
    }
    for skill in catalog {
        let global_file = codex_home.join("skills").join(&skill.name).join("SKILL.md");
        let project_file = project_root.map(|root| {
            root.join(".agents/skills")
                .join(&skill.name)
                .join("SKILL.md")
        });
        let source_id = if same_file(&skill.path, &global_file) {
            Some(("codex-home-skill", codex_home.join("skills")))
        } else if project_file
            .as_deref()
            .is_some_and(|file| same_file(&skill.path, file))
        {
            project_root.map(|root| ("project-agents-skill", root.join(".agents/skills")))
        } else {
            None
        };
        if let Some((source, root)) = source_id {
            let identity = inventory_identity(&view.machine_id, source, &root, &skill.name);
            if let Some(item) = view
                .items
                .iter_mut()
                .find(|item| item.identity.as_deref() == Some(identity.as_str()))
            {
                item.available = Some(skill.enabled);
                item.reason = Some(if skill.enabled {
                    "Codex skills/list reports this skill enabled for the selected workspace; selected-session inclusion is unverified.".into()
                } else {
                    "Codex skills/list reports this skill disabled for the selected workspace."
                        .into()
                });
                continue;
            }
        }
        let identity = inventory_identity(
            &view.machine_id,
            "codex-native-catalog-skill",
            &skill.path,
            &skill.name,
        );
        if view
            .items
            .iter()
            .any(|item| item.identity.as_deref() == Some(identity.as_str()))
        {
            continue;
        }
        view.items.push(IntegrationInventoryItem {
            identity: Some(identity),
            name: skill.name.clone(),
            kind: IntegrationKind::Skill,
            source: "Codex app-server / skills/list".into(),
            scope: "provider".into(),
            configured: true,
            available: Some(skill.enabled),
            attached: None,
            reason: Some(if skill.enabled {
                "Reported enabled by Codex; selected-session inclusion is unverified.".into()
            } else {
                "Reported disabled by Codex.".into()
            }),
            owner: IntegrationOwner::ProviderNative,
            actions: unsupported_actions(),
            help_route: IntegrationHelpRoute::Unknown,
            mcp: None,
        });
    }
    view.items.sort_by(|a, b| {
        (&a.kind, &a.scope, &a.name, &a.identity).cmp(&(&b.kind, &b.scope, &b.name, &b.identity))
    });
    view.help = "Codex skills/list was read for the selected workspace and account. LUNA can manage regular skill folders; provider-reported enablement still does not prove selected-session attachment. MCP server health and attachment remain unverified.".into();
}

pub fn mark_codex_catalog_unavailable(view: &mut IntegrationInventoryView) {
    let reason = "Codex skills/list could not be read for the selected workspace; provider availability remains unknown.";
    view.unavailable_reason = Some(match view.unavailable_reason.take() {
        Some(existing) => format!("{existing} {reason}"),
        None => reason.into(),
    });
}

pub fn apply_codex_mcp_config(
    view: &mut IntegrationInventoryView,
    servers: &[CodexMcpConfigEntry],
    workspace: &Path,
) {
    for server in servers {
        let identity = inventory_identity(
            &view.machine_id,
            "codex-mcp-selected-workspace",
            workspace,
            &server.name,
        );
        let reason = if server.enabled {
            "Codex config/read reports this MCP server configured; health, authentication and selected-session attachment are unverified."
        } else {
            "Codex config/read reports this MCP server disabled; an existing session may still have it loaded."
        };
        if let Some(item) = view
            .items
            .iter_mut()
            .find(|item| item.identity.as_deref() == Some(identity.as_str()))
        {
            item.available = if server.enabled { None } else { Some(false) };
            item.attached = None;
            item.reason = Some(reason.into());
            continue;
        }
        view.items.push(IntegrationInventoryItem {
            identity: Some(identity),
            name: server.name.clone(),
            kind: IntegrationKind::McpServer,
            source: "Codex app-server / config/read".into(),
            scope: "selected workspace".into(),
            configured: true,
            available: if server.enabled { None } else { Some(false) },
            attached: None,
            reason: Some(reason.into()),
            owner: IntegrationOwner::ProviderNative,
            actions: unsupported_actions(),
            help_route: IntegrationHelpRoute::ProviderMcpConfig,
            mcp: None,
        });
    }
    view.items.sort_by(|a, b| {
        (&a.kind, &a.scope, &a.name, &a.identity).cmp(&(&b.kind, &b.scope, &b.name, &b.identity))
    });
}

pub fn mark_codex_mcp_config_unavailable(view: &mut IntegrationInventoryView) {
    let reason = "Codex config/read could not be read; effective provider MCP configuration remains unverified.";
    view.unavailable_reason = Some(match view.unavailable_reason.take() {
        Some(existing) => format!("{existing} {reason}"),
        None => reason.into(),
    });
}

pub fn unavailable_inventory(provider: HarnessId, machine_id: &str) -> IntegrationInventoryView {
    let reason = match provider {
        HarnessId::ClaudeCode => {
            "Claude's adapter exposes slash commands and MCP tool-use events, but no verified integration catalog, enable, authentication, or session-attachment API."
        }
        HarnessId::Opencode => {
            "OpenCode's adapter discovers slash commands and uses the provider's existing plugin/MCP configuration, but has no verified integration enable, authentication, or session-attachment API."
        }
        HarnessId::Mock => "The synthetic test harness has no provider integrations.",
        HarnessId::Codex => {
            "Codex configuration has a separate read-only inventory; LUNA has no verified enable, authentication, or session-attachment action."
        }
        _ => "This provider is no longer supported by LUNA.",
    };
    IntegrationInventoryView {
        provider,
        machine_id: machine_id.into(),
        items: Vec::new(),
        unavailable_reason: Some(reason.into()),
        help: "Manage integrations in the provider's own settings or the chosen MCP gateway. LUNA does not register duplicate servers or report an unverified live attachment.".into(),
    }
}

fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn read_regular_file(path: &Path, limit: usize) -> Result<Vec<u8>, EngineError> {
    use std::io::Read as _;

    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(EngineError::Other(
            "Symlinked or special skill file cannot be read".into(),
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let mut content = Vec::new();
    file.take((limit as u64).saturating_add(1))
        .read_to_end(&mut content)?;
    if content.len() > limit {
        return Err(EngineError::Other(
            "Skill file exceeds the library limit".into(),
        ));
    }
    Ok(content)
}

/// This revision includes scripts and references so an old read cannot remove
/// or overwrite a skill after any supporting file has changed.
fn tree_revision(directory: &Path) -> Result<String, EngineError> {
    fn walk(
        directory: &Path,
        relative: &str,
        depth: usize,
        count: &mut usize,
        bytes: &mut u64,
        hash: &mut Sha256,
    ) -> Result<(), EngineError> {
        if depth > MAX_SKILL_DEPTH {
            return Err(EngineError::Other("Skill folder is too deep".into()));
        }
        let mut entries = std::fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            *count += 1;
            if *count > MAX_SKILL_FILES {
                return Err(EngineError::Other("Skill has too many files".into()));
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| EngineError::Other("Skill filename is not UTF-8".into()))?;
            let path = format!("{relative}{name}");
            hash.update((path.len() as u64).to_be_bytes());
            hash.update(path.as_bytes());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                hash.update(b"directory");
                walk(
                    &entry.path(),
                    &format!("{path}/"),
                    depth + 1,
                    count,
                    bytes,
                    hash,
                )?;
            } else if kind.is_file() {
                hash.update(b"file");
                let content = read_regular_file(&entry.path(), MAX_INSTALL_BYTES as usize)?;
                *bytes = bytes.saturating_add(content.len() as u64);
                if *bytes > MAX_INSTALL_BYTES {
                    return Err(EngineError::Other("Skill files exceed 2 MiB".into()));
                }
                hash.update((content.len() as u64).to_be_bytes());
                hash.update(&content);
            } else {
                return Err(EngineError::Other(
                    "Symlinked or special files in a skill folder are not managed".into(),
                ));
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    hash.update(b"luna-skill-tree-v1\0");
    let mut count = 0;
    let mut bytes = 0;
    walk(directory, "", 0, &mut count, &mut bytes, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

fn validate_skill_content(
    provider: HarnessId,
    name: &str,
    content: &str,
) -> Result<(), EngineError> {
    if !valid_skill_name(name)
        || content.len() > MAX_SKILL_BYTES
        || content.contains('\0')
        || !content.starts_with("---\n")
    {
        return Err(EngineError::Other(
            "Skill requires a safe lowercase name and a SKILL.md under 128 KiB with YAML frontmatter"
                .into(),
        ));
    }
    let frontmatter = content[4..]
        .split_once("\n---\n")
        .map(|(frontmatter, _)| frontmatter)
        .ok_or_else(|| EngineError::Other("SKILL.md frontmatter is not closed".into()))?;
    let mut has_description = false;
    let mut has_name = false;
    for line in frontmatter.lines() {
        if let Some(value) = line.strip_prefix("description:") {
            has_description = !value.trim().is_empty();
        }
        if let Some(value) = line.strip_prefix("name:") {
            let value = value.trim().trim_matches(['\'', '"']);
            if value != name {
                return Err(EngineError::Other(
                    "Skill frontmatter name must match its folder".into(),
                ));
            }
            has_name = true;
        }
    }
    if !has_description || (provider == HarnessId::Opencode && !has_name) {
        return Err(EngineError::Other(
            "Skill frontmatter needs a description; OpenCode also needs a matching name".into(),
        ));
    }
    Ok(())
}

fn has_symlink_between(base: &Path, target: &Path) -> Result<bool, EngineError> {
    let relative = target
        .strip_prefix(base)
        .map_err(|_| EngineError::Other("Skill path escapes its owner root".into()))?;
    let mut path = base.to_path_buf();
    if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Ok(true);
    }
    for component in relative.components() {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

fn skill_path(location: &SkillLocation, name: &str) -> Result<PathBuf, EngineError> {
    if !valid_skill_name(name) {
        return Err(EngineError::Other("Invalid skill name".into()));
    }
    Ok(location.root.join(name))
}

fn resolved_skill(
    provider: HarnessId,
    roots: &SkillRoots,
    identity: &str,
) -> Result<(SkillLocation, String), EngineError> {
    for scope in [
        IntegrationSkillScope::Global,
        IntegrationSkillScope::Project,
    ] {
        let Ok(location) = roots.location(provider, scope) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(&location.root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if valid_skill_name(&name)
                && provider_identity(
                    provider,
                    &roots.machine_id,
                    location.source_id,
                    &location.identity_root,
                    &name,
                ) == identity
                && entry.path().join("SKILL.md").is_file()
            {
                return Ok((location, name));
            }
        }
    }
    Err(EngineError::Other(
        "Skill is no longer present in the selected provider and scope".into(),
    ))
}

fn supporting_files(skill_dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, output: &mut Vec<String>, depth: usize) {
        if depth > MAX_SKILL_DEPTH || output.len() >= MAX_SKILL_FILES {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if output.len() >= MAX_SKILL_FILES {
                return;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "SKILL.md" && prefix.is_empty() {
                continue;
            }
            let relative = format!("{prefix}{name}");
            output.push(relative.clone());
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                walk(&entry.path(), &format!("{relative}/"), output, depth + 1);
            }
        }
    }
    let mut files = Vec::new();
    walk(skill_dir, "", &mut files, 0);
    files.sort();
    files
}

pub fn skill_document(
    provider: HarnessId,
    roots: &SkillRoots,
    identity: &str,
) -> Result<SkillDocumentView, EngineError> {
    let (location, name) = resolved_skill(provider, roots, identity)?;
    let directory = skill_path(&location, &name)?;
    let file = directory.join("SKILL.md");
    if has_symlink_between(&location.base, &file)? {
        return Err(EngineError::Other(
            "Symlinked skill or owner folder cannot be inspected through LUNA".into(),
        ));
    }
    let revision_before = tree_revision(&directory)?;
    let bytes = read_regular_file(&file, MAX_SKILL_BYTES)?;
    let content =
        String::from_utf8(bytes).map_err(|_| EngineError::Other("SKILL.md is not UTF-8".into()))?;
    let revision = tree_revision(&directory)?;
    if revision != revision_before {
        return Err(EngineError::Other(
            "Skill changed while being inspected; reload it".into(),
        ));
    }
    Ok(SkillDocumentView {
        provider,
        scope: location.scope,
        identity: identity.into(),
        name,
        content,
        revision,
        supporting_files: supporting_files(&directory),
        read_only_reason: None,
    })
}

pub fn save_skill_document(
    provider: HarnessId,
    roots: &SkillRoots,
    scope: IntegrationSkillScope,
    name: &str,
    content: &str,
    expected_revision: Option<&str>,
) -> Result<SkillDocumentView, EngineError> {
    let _write_guard = SKILL_WRITE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    validate_skill_content(provider, name, content)?;
    let location = roots.location(provider, scope)?;
    let directory = skill_path(&location, name)?;
    let file = directory.join("SKILL.md");
    if has_symlink_between(&location.base, &file)? {
        return Err(EngineError::Other(
            "Symlinked skill roots are read only".into(),
        ));
    }
    match expected_revision {
        Some(expected) => {
            if tree_revision(&directory)? != expected {
                return Err(EngineError::Other(
                    "Skill changed elsewhere; reload before saving".into(),
                ));
            }
        }
        None if directory.exists() => {
            return Err(EngineError::Other(
                "Skill name already exists; choose another name or open it to edit".into(),
            ));
        }
        None => {}
    }
    if expected_revision.is_none() {
        std::fs::create_dir_all(&location.root)?;
        std::fs::create_dir(&directory)?;
    }
    if has_symlink_between(&location.base, &file)? {
        return Err(EngineError::Other(
            "Symlinked skill roots are read only".into(),
        ));
    }
    // Stage beside the skill directory, outside the tree being revisioned.
    // The sibling stays on the same filesystem for the final atomic rename.
    let mut temporary = tempfile::NamedTempFile::new_in(&location.root)?;
    use std::io::Write as _;
    temporary.write_all(content.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    if let Some(expected) = expected_revision {
        if tree_revision(&directory)? != expected {
            return Err(EngineError::Other(
                "Skill changed before save; reload it".into(),
            ));
        }
    }
    if expected_revision.is_some() {
        temporary.persist(&file).map_err(|error| error.error)?;
    } else {
        temporary
            .persist_noclobber(&file)
            .map_err(|error| error.error)?;
    }
    let identity = provider_identity(
        provider,
        &roots.machine_id,
        location.source_id,
        &location.identity_root,
        name,
    );
    skill_document(provider, roots, &identity)
}

fn copy_skill_tree(
    source: &Path,
    destination: &Path,
    file_count: &mut usize,
    total_bytes: &mut u64,
    depth: usize,
) -> Result<(), EngineError> {
    if depth > MAX_SKILL_DEPTH {
        return Err(EngineError::Other("Skill folder is too deep".into()));
    }
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        *file_count += 1;
        if *file_count > MAX_SKILL_FILES {
            return Err(EngineError::Other(
                "Skill has too many supporting files".into(),
            ));
        }
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if kind.is_symlink() {
            return Err(EngineError::Other(
                "Cannot install a skill containing symlinked files".into(),
            ));
        }
        if kind.is_dir() {
            copy_skill_tree(&entry.path(), &target, file_count, total_bytes, depth + 1)?;
        } else if kind.is_file() {
            *total_bytes = total_bytes.saturating_add(entry.metadata()?.len());
            if *total_bytes > MAX_INSTALL_BYTES {
                return Err(EngineError::Other(
                    "Skill files exceed the 2 MiB install limit".into(),
                ));
            }
            std::fs::copy(entry.path(), target)?;
        } else {
            return Err(EngineError::Other(
                "Unsupported file in skill folder".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rename_directory_noreplace(source: &Path, destination: &Path) -> Result<(), EngineError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| EngineError::Other("Invalid install path".into()))?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| EngineError::Other("Invalid install path".into()))?;
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(windows)]
fn rename_directory_noreplace(source: &Path, destination: &Path) -> Result<(), EngineError> {
    // Windows RenameFile does not replace an existing directory.
    std::fs::rename(source, destination)?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn rename_directory_noreplace(_source: &Path, _destination: &Path) -> Result<(), EngineError> {
    Err(EngineError::Other(
        "Atomic no-replace skill install is unavailable on this platform".into(),
    ))
}

/// Install a local skill into a different provider or scope. Copy the complete
/// folder so relative scripts/references remain with their SKILL.md. The source
/// revision and an absent destination are both required; no network registry
/// or MCP server is involved.
pub fn install_skill(
    source_provider: HarnessId,
    source_identity: &str,
    expected_source_revision: &str,
    destination_provider: HarnessId,
    destination_scope: IntegrationSkillScope,
    roots: &SkillRoots,
) -> Result<SkillDocumentView, EngineError> {
    let _write_guard = SKILL_WRITE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let source = skill_document(source_provider, roots, source_identity)?;
    if source.revision != expected_source_revision {
        return Err(EngineError::Other(
            "Source skill changed elsewhere; reload before installing".into(),
        ));
    }
    validate_skill_content(destination_provider, &source.name, &source.content)?;
    let (source_location, _) = resolved_skill(source_provider, roots, source_identity)?;
    let source_dir = skill_path(&source_location, &source.name)?;
    if has_symlink_between(&source_location.base, &source_dir)? {
        return Err(EngineError::Other(
            "Install from the original folder of this symlinked skill".into(),
        ));
    }
    let destination = roots.location(destination_provider, destination_scope)?;
    let destination_dir = skill_path(&destination, &source.name)?;
    if destination_dir.exists() || has_symlink_between(&destination.base, &destination_dir)? {
        return Err(EngineError::Other(
            "Destination skill exists or its owner folder is symlinked".into(),
        ));
    }
    std::fs::create_dir_all(&destination.root)?;
    let temporary = tempfile::Builder::new()
        .prefix(".luna-skill-install-")
        .tempdir_in(&destination.root)?;
    let staged = temporary.path().join(&source.name);
    let mut file_count = 0;
    let mut total_bytes = 0;
    copy_skill_tree(&source_dir, &staged, &mut file_count, &mut total_bytes, 0)?;
    if tree_revision(&source_dir)? != expected_source_revision
        || tree_revision(&staged)? != expected_source_revision
    {
        return Err(EngineError::Other(
            "Source skill changed during install; retry from the library".into(),
        ));
    }
    if destination_dir.exists() || has_symlink_between(&destination.base, &destination_dir)? {
        return Err(EngineError::Other(
            "Destination skill changed during install".into(),
        ));
    }
    rename_directory_noreplace(&staged, &destination_dir)?;
    let identity = provider_identity(
        destination_provider,
        &roots.machine_id,
        destination.source_id,
        &destination.identity_root,
        &source.name,
    );
    skill_document(destination_provider, roots, &identity)
}

/// Remove the complete provider-owned skill folder after a fresh revision
/// check. The UI presents its supporting files and requires an armed second
/// click before calling this operation.
pub fn remove_skill(
    provider: HarnessId,
    roots: &SkillRoots,
    identity: &str,
    expected_revision: &str,
) -> Result<(), EngineError> {
    let _write_guard = SKILL_WRITE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let document = skill_document(provider, roots, identity)?;
    if document.revision != expected_revision {
        return Err(EngineError::Other(
            "Skill changed elsewhere; reload before removing".into(),
        ));
    }
    let (location, name) = resolved_skill(provider, roots, identity)?;
    let directory = skill_path(&location, &name)?;
    if has_symlink_between(&location.base, &directory)? {
        return Err(EngineError::Other(
            "Symlinked skill roots are read only".into(),
        ));
    }
    if tree_revision(&directory)? != expected_revision {
        return Err(EngineError::Other(
            "Skill changed before removal; reload it".into(),
        ));
    }
    std::fs::remove_dir_all(&directory)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_catalog_enriches_exact_files_without_claiming_session_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("PRIVATE_HOME_SENTINEL");
        let project = dir.path().join("PRIVATE_PROJECT_SENTINEL");
        for skill in [
            home.join("skills/global/SKILL.md"),
            project.join(".agents/skills/project/SKILL.md"),
        ] {
            std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
            std::fs::write(skill, "synthetic").unwrap();
        }
        let mut view = codex_inventory(&home, Some(&project), "fake-device");
        apply_codex_skill_catalog(
            &mut view,
            &[
                CodexSkillCatalogEntry {
                    name: "global".into(),
                    path: home.join("skills/global/SKILL.md"),
                    enabled: true,
                },
                CodexSkillCatalogEntry {
                    name: "project".into(),
                    path: project.join(".agents/skills/project/SKILL.md"),
                    enabled: false,
                },
                CodexSkillCatalogEntry {
                    name: "managed".into(),
                    path: home.join("managed/managed/SKILL.md"),
                    enabled: true,
                },
            ],
            &home,
            Some(&project),
        );
        apply_codex_mcp_config(
            &mut view,
            &[
                CodexMcpConfigEntry {
                    name: "gateway".into(),
                    enabled: true,
                },
                CodexMcpConfigEntry {
                    name: "off".into(),
                    enabled: false,
                },
            ],
            &project,
        );
        assert_eq!(view.items.len(), 5);
        assert_eq!(
            view.items
                .iter()
                .find(|item| item.name == "global")
                .unwrap()
                .available,
            Some(true)
        );
        assert_eq!(
            view.items
                .iter()
                .find(|item| item.name == "project")
                .unwrap()
                .available,
            Some(false)
        );
        assert_eq!(
            view.items
                .iter()
                .find(|item| item.name == "managed")
                .unwrap()
                .source,
            "Codex app-server / skills/list"
        );
        assert!(
            view.items
                .iter()
                .filter(|item| item.kind == IntegrationKind::Skill)
                .all(|item| item.attached.is_none())
        );
        assert_eq!(
            view.items
                .iter()
                .find(|item| item.name == "gateway")
                .unwrap()
                .available,
            None
        );
        assert_eq!(
            view.items
                .iter()
                .find(|item| item.name == "off")
                .unwrap()
                .attached,
            None,
            "disabled current config cannot prove an existing session detached"
        );
        let serialized = serde_json::to_string(&view).unwrap();
        assert!(!serialized.contains("PRIVATE_HOME_SENTINEL"));
        assert!(!serialized.contains("PRIVATE_PROJECT_SENTINEL"));

        mark_codex_catalog_unavailable(&mut view);
        mark_codex_mcp_config_unavailable(&mut view);
        assert!(
            view.unavailable_reason
                .unwrap()
                .contains("availability remains unknown")
        );
    }

    #[test]
    fn settings_are_scoped_and_revision_guarded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("luna/instructions.json");
        let store = IntegrationSettings::open(path.clone()).unwrap();
        assert_eq!(store.save(None, "global", 0).unwrap().revision, 1);
        assert_eq!(store.save(Some("p1"), "project", 1).unwrap().revision, 2);
        assert!(store.save(Some("p2"), "stale", 1).is_err());
        let reopened = IntegrationSettings::open(path).unwrap();
        let (global, project) = reopened.contributions(Some("p1"));
        assert_eq!(global[0].text, "global");
        assert_eq!(project[0].text, "project");
        assert!(reopened.contributions(Some("p2")).1.is_empty());
    }

    #[test]
    fn inventory_reads_only_explicit_synthetic_sources() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("codex");
        let project = dir.path().join("project");
        std::fs::create_dir_all(home.join("skills/example")).unwrap();
        std::fs::create_dir_all(project.join(".agents/skills/local")).unwrap();
        std::fs::write(home.join("skills/example/SKILL.md"), "# Example").unwrap();
        std::fs::write(project.join(".agents/skills/local/SKILL.md"), "# Local").unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[mcp_servers.gateway]\ncommand = 'gateway'\n[mcp_servers.off]\nenabled = false\n",
        )
        .unwrap();
        let view = codex_inventory(&home, Some(&project), "synthetic-machine");
        assert_eq!(view.machine_id, "synthetic-machine");
        assert_eq!(view.items.len(), 4);
        assert_eq!(
            view.items
                .iter()
                .find(|item| item.name == "gateway")
                .unwrap()
                .owner,
            IntegrationOwner::ProviderNative,
            "a server name cannot prove gateway ownership"
        );
        assert!(
            view.items
                .iter()
                .filter(|item| item.name != "off")
                .all(|item| item.attached.is_none())
        );
        assert_eq!(
            view.items
                .iter()
                .find(|item| item.name == "off")
                .unwrap()
                .available,
            Some(false)
        );
        assert_eq!(
            view.items
                .iter()
                .find(|item| item.name == "off")
                .unwrap()
                .attached,
            None,
            "file-only inventory cannot establish selected-session absence"
        );
    }

    #[test]
    fn mcp_readback_uses_workspace_identity_without_rewriting_home_facts() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("PRIVATE_HOME_SENTINEL");
        let project_a = dir.path().join("PRIVATE_PROJECT_A_SENTINEL");
        let project_b = dir.path().join("PRIVATE_PROJECT_B_SENTINEL");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[mcp_servers.gateway]\nenabled = false\n",
        )
        .unwrap();
        let mut a = codex_inventory(&home, Some(&project_a), "fake-device");
        let mut b = codex_inventory(&home, Some(&project_b), "fake-device");
        let observed = [CodexMcpConfigEntry {
            name: "gateway".into(),
            enabled: true,
        }];
        apply_codex_mcp_config(&mut a, &observed, &project_a);
        apply_codex_mcp_config(&mut b, &observed, &project_b);
        assert_eq!(a.items.len(), 2);
        let home_item = a.items.iter().find(|item| item.scope == "global").unwrap();
        let workspace_item = a
            .items
            .iter()
            .find(|item| item.scope == "selected workspace")
            .unwrap();
        assert_eq!(home_item.available, Some(false));
        assert_eq!(workspace_item.available, None);
        assert!(home_item.attached.is_none() && workspace_item.attached.is_none());
        assert_ne!(home_item.identity, workspace_item.identity);
        assert_ne!(
            workspace_item.identity,
            b.items
                .iter()
                .find(|item| item.scope == "selected workspace")
                .unwrap()
                .identity
        );
        let serialized = serde_json::to_string(&a).unwrap();
        assert!(!serialized.contains("PRIVATE_HOME_SENTINEL"));
        assert!(!serialized.contains("PRIVATE_PROJECT_A_SENTINEL"));
    }

    #[test]
    fn synthetic_duplicate_names_keep_source_identity_and_leak_no_private_values() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("PRIVATE_PATH_SENTINEL/codex");
        let project = dir.path().join("project-one");
        let other_project = dir.path().join("project-two");
        for root in [
            home.join("skills/duplicate"),
            project.join(".agents/skills/duplicate"),
            other_project.join(".agents/skills/duplicate"),
        ] {
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("SKILL.md"), "# Synthetic").unwrap();
        }
        std::fs::write(
            home.join("config.toml"),
            "[mcp_servers.duplicate]\ncommand = 'SECRET_COMMAND_SENTINEL'\nenv = { TOKEN = 'SECRET_TOKEN_SENTINEL' }\n",
        )
        .unwrap();

        let first = codex_inventory(&home, Some(&project), "synthetic-machine");
        let again = codex_inventory(&home, Some(&project), "synthetic-machine");
        let other = codex_inventory(&home, Some(&other_project), "synthetic-machine");
        assert_eq!(first.items, again.items);
        assert_eq!(first.items.len(), 3);
        assert!(first.items.iter().all(|item| item.name == "duplicate"));
        let ids: std::collections::HashSet<_> = first
            .items
            .iter()
            .map(|item| item.identity.as_deref().unwrap())
            .collect();
        assert_eq!(ids.len(), 3);
        fn project_skill(view: &IntegrationInventoryView) -> &IntegrationInventoryItem {
            view.items
                .iter()
                .find(|item| item.scope == "project")
                .unwrap()
        }
        assert_ne!(
            project_skill(&first).identity,
            project_skill(&other).identity
        );
        assert_eq!(
            first
                .items
                .iter()
                .find(|item| item.scope == "global" && item.kind == IntegrationKind::Skill)
                .unwrap()
                .identity,
            other
                .items
                .iter()
                .find(|item| item.scope == "global" && item.kind == IntegrationKind::Skill)
                .unwrap()
                .identity,
        );
        assert!(first.items.iter().all(|item| {
            item.owner
                == (if item.scope == "project" {
                    IntegrationOwner::Unknown
                } else {
                    IntegrationOwner::ProviderNative
                })
                && item.actions.enable == IntegrationActionAvailability::Unavailable
                && item.actions.disable == IntegrationActionAvailability::Unavailable
                && item.actions.authenticate == IntegrationActionAvailability::Unavailable
                && item.attached.is_none()
        }));
        assert!(matches!(
            project_skill(&first).help_route,
            IntegrationHelpRoute::SkillSource {
                scope: IntegrationSkillScope::Project
            }
        ));
        assert!(matches!(
            first
                .items
                .iter()
                .find(|item| item.kind == IntegrationKind::McpServer)
                .unwrap()
                .help_route,
            IntegrationHelpRoute::ProviderMcpConfig
        ));
        let public_json = serde_json::to_string(&first).unwrap();
        for private in [
            "PRIVATE_PATH_SENTINEL",
            "SECRET_COMMAND_SENTINEL",
            "SECRET_TOKEN_SENTINEL",
        ] {
            assert!(!public_json.contains(private));
        }
    }

    #[test]
    fn global_then_project_compile_with_redacted_readback() {
        let dir = tempfile::tempdir().unwrap();
        let store = IntegrationSettings::open(dir.path().join("settings.json")).unwrap();
        store.save(None, "private global text", 0).unwrap();
        store
            .save(Some("project-1"), "private project text", 1)
            .unwrap();
        let (user_instructions, workspace_instructions) = store.contributions(Some("project-1"));
        let context = luna_proto::RuntimeContext {
            app_version: "test".into(),
            protocol_version: 1,
            thread_id: "task-1".into(),
            parent_thread_id: None,
            source_thread_id: None,
            project_id: Some("project-1".into()),
            project_name: Some("Project".into()),
            environment_id: "env-1".into(),
            machine_id: "machine-1".into(),
            machine_name: "Machine".into(),
            workspace_root: dir.path().display().to_string(),
            worktree_id: None,
            binding_generation: None,
            provider_profile_id: None,
            branch: None,
            harness: HarnessId::Mock,
            model: None,
            reasoning: None,
            execution_route: "local".into(),
            granted_scopes: Vec::new(),
            chat: None,
            board: None,
        };
        let compiled = crate::runtime_contract::compile(crate::runtime_contract::CompileInput {
            context,
            provider_delivery: luna_proto::RuntimeDelivery::CodexInstructionsOnly,
            user_instructions,
            workspace_instructions,
            resume: false,
            acknowledged_contract: None,
            cross_task_tools: Vec::new(),
            cross_task_omissions: Vec::new(),
        })
        .unwrap();
        let sections = &compiled.contract.instructions;
        assert!(
            sections
                .iter()
                .position(|s| s.text == "private global text")
                .unwrap()
                < sections
                    .iter()
                    .position(|s| s.text == "private project text")
                    .unwrap()
        );
        let diagnostic = compiled.contract.diagnostics("synthetic-session");
        let serialized = serde_json::to_string(&diagnostic).unwrap();
        assert!(!serialized.contains("private global text"));
        assert!(!serialized.contains("private project text"));
    }
}
