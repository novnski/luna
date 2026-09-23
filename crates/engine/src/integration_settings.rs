//! LUNA-owned instruction settings and read-only provider inventory.
//!
//! Every provider path is supplied by engine assembly. This module never
//! discovers a credential home from HOME, CODEX_HOME, or a native login.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use zeron_proto::{
    HarnessId, InstructionSettingsView, IntegrationInventoryItem, IntegrationInventoryView,
    IntegrationKind,
};

use crate::EngineError;
use crate::runtime_contract::InstructionContribution;

const MAX_INSTRUCTION_CHARS: usize = 16_000;

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
    for (root, scope, source) in [
        (
            Some(codex_home.join("skills")),
            "global",
            "Codex home / skills",
        ),
        (
            project_root.map(|p| p.join(".agents/skills")),
            "project",
            "project / .agents/skills",
        ),
    ] {
        let Some(root) = root else { continue };
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path().join("SKILL.md");
            if !path.is_file() {
                continue;
            }
            items.push(IntegrationInventoryItem {
                name: entry.file_name().to_string_lossy().into_owned(),
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
                            name: name.clone(),
                            kind: IntegrationKind::McpServer,
                            source: "Codex home / config.toml".into(),
                            scope: "global".into(),
                            configured: true,
                            available: if enabled { None } else { Some(false) },
                            attached: if enabled { None } else { Some(false) },
                            reason: Some(if enabled {
                                "Configured; server availability, authentication, and session attachment are unverified.".into()
                            } else {
                                "Disabled in provider-native configuration.".into()
                            }),
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
    items.sort_by(|a, b| (&a.kind, &a.scope, &a.name).cmp(&(&b.kind, &b.scope, &b.name)));
    IntegrationInventoryView {
        provider: HarnessId::Codex,
        machine_id: machine_id.into(),
        items,
        unavailable_reason,
        help: "This read covers Codex-home skills, project .agents/skills, and mcp_servers in Codex config.toml. Provider-managed skills and live attachment are unverified. Manage entries with their owner, Codex or the chosen gateway; LUNA cannot enable, authenticate, or register a duplicate here.".into(),
    }
}

pub fn unavailable_inventory(provider: HarnessId, machine_id: &str) -> IntegrationInventoryView {
    let reason = match provider {
        HarnessId::ClaudeCode => {
            "Claude's adapter exposes slash commands and MCP tool-use events, but no verified integration catalog, enable, authentication, or session-attachment API."
        }
        HarnessId::Devin
        | HarnessId::Grok
        | HarnessId::Hermes
        | HarnessId::Pi
        | HarnessId::Antigravity => {
            "This ACP adapter exposes available commands and starts sessions with an empty LUNA-supplied MCP server list; it has no verified integration enable, authentication, or session-attachment API."
        }
        HarnessId::Opencode => {
            "OpenCode's adapter discovers slash commands and uses the provider's existing plugin/MCP configuration, but has no verified integration enable, authentication, or session-attachment API."
        }
        HarnessId::Cursor => {
            "Cursor's adapter has no verified skill/MCP catalog, enable, authentication, or session-attachment API."
        }
        HarnessId::Mock => "The synthetic test harness has no provider integrations.",
        HarnessId::Codex => {
            "Codex configuration has a separate read-only inventory; LUNA has no verified enable, authentication, or session-attachment action."
        }
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
        let context = zeron_proto::RuntimeContext {
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
            provider_delivery: zeron_proto::RuntimeDelivery::CodexInstructionsOnly,
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

    #[test]
    fn unsupported_providers_report_specific_limits_without_inventing_attachment() {
        for provider in [
            HarnessId::ClaudeCode,
            HarnessId::Opencode,
            HarnessId::Devin,
            HarnessId::Cursor,
            HarnessId::Mock,
        ] {
            let view = unavailable_inventory(provider, "synthetic-machine");
            assert!(view.items.is_empty());
            assert_eq!(view.machine_id, "synthetic-machine");
            assert!(
                view.unavailable_reason
                    .as_deref()
                    .unwrap()
                    .contains("no verified")
                    || provider == HarnessId::Mock
            );
            assert!(view.help.contains("chosen MCP gateway"));
        }
    }
}
