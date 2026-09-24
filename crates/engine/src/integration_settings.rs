//! LUNA-owned instruction settings and read-only provider inventory.
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
    IntegrationOwner, IntegrationSkillScope,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::EngineError;
use crate::runtime_contract::InstructionContribution;

const MAX_INSTRUCTION_CHARS: usize = 16_000;

/// Identifier only, never an authorization token. Length framing prevents
/// ambiguous tuples; the private source path is never returned to the UI.
fn inventory_identity(machine_id: &str, source: &str, root: &Path, name: &str) -> String {
    let mut hash = Sha256::new();
    let root_text = root.to_string_lossy();
    for value in [
        "luna-integration-v1",
        machine_id,
        "codex",
        source,
        root_text.as_ref(),
        name,
    ] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    format!("intg_v1_{:x}", hash.finalize())
}

fn unsupported_actions() -> IntegrationActionFacts {
    IntegrationActionFacts {
        enable: IntegrationActionAvailability::Unavailable,
        disable: IntegrationActionAvailability::Unavailable,
        authenticate: IntegrationActionAvailability::Unavailable,
        reason: Some("No verified LUNA action for this integration source.".into()),
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
        help: "This read covers Codex-home skills, project .agents/skills, and mcp_servers in Codex config.toml. Provider-managed skills and live attachment are unverified. Manage entries with their owner, Codex or the chosen gateway; LUNA cannot enable, authenticate, or register a duplicate here.".into(),
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
        });
    }
    view.items.sort_by(|a, b| {
        (&a.kind, &a.scope, &a.name, &a.identity).cmp(&(&b.kind, &b.scope, &b.name, &b.identity))
    });
    view.help = "Codex skills/list was read for the selected workspace and account. File discovery and provider-reported enablement are separate; neither proves selected-session attachment. MCP server health and attachment remain unverified. Manage integrations with Codex or the chosen gateway.".into();
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
        assert!(view.help.contains("chosen gateway"));
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
