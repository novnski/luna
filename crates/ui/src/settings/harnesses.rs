//! Advanced controls embedded below the three provider cards: model catalog
//! refresh, instructions, integration inventory, runtime contract, and title
//! preferences. Device scope follows the parent Providers page.

use gpui::{
    AnyElement, Context, Entity, IntoElement, Render, SharedString, Subscription, Task, Window,
    div, prelude::*, px,
};

use crate::composer::ComposerInput;
use futures::{StreamExt, stream};
use luna_engine::registry::TitleSettings;
use luna_engine::registry::{HarnessDescriptor, descriptor_enabled};
use std::time::Duration;

use luna_proto::Model;
use luna_proto::{
    AgentAccountsSnapshot, HarnessId, InstructionSettingsView, IntegrationInventoryView,
    IntegrationKind, IntegrationSkillScope, RuntimeContractView, SkillDocumentView,
};
use luna_rpc::methods;

use crate::AccessibleButton as _;
use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

mod mcp;
#[cfg(feature = "appshots-fixture")]
pub use mcp::fixture as mcp_fixture;
mod claude_sessions;

fn meta_text(theme: &Theme, value: impl Into<SharedString>) -> gpui::Div {
    widgets::meta_line(
        theme,
        vec![
            div()
                .flex_1()
                .min_w_0()
                .child(value.into())
                .into_any_element(),
        ],
    )
}

fn skill_button(
    theme: &Theme,
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    // GPUI synthesizes one keyboard click on key-up for a focused on_click
    // control. Keep keyboard behavior there rather than adding key handlers.
    let accent = theme.accent;
    widgets::ghost_action(theme)
        .id(id.into())
        .role(gpui::Role::Button)
        .aria_label(label.into())
        .when(enabled, |el| {
            el.tab_index(0)
                .focus_visible(move |style| crate::theme::focus_ring(style, accent))
        })
        .when(!enabled, |el| el.cursor_default())
}

fn matches_selected_contract(
    view: &RuntimeContractView,
    selected_task: Option<&str>,
    target_device: Option<&str>,
) -> bool {
    target_device.is_none() && selected_task == Some(view.task_id.as_str())
}

pub struct HarnessesPage {
    title_settings: Loadable<TitleSettings>,
    title_models: Loadable<Vec<Model>>,
    title_menu: Option<bool>, // false = harness, true = model
    title_task: Option<Task<()>>,
    title_saving: bool,
    state: Entity<AppState>,
    harnesses: Loadable<Vec<HarnessDescriptor>>,
    /// Which device the parent Providers page selected.
    target_device: Option<String>,
    error: Option<String>,
    load_task: Option<Task<()>>,
    model_refresh_task: Option<Task<()>>,
    model_refresh_running: bool,
    model_refresh_result: Option<String>,
    instructions: Loadable<InstructionSettingsView>,
    inventory: Loadable<IntegrationInventoryView>,
    integration_provider: HarnessId,
    instruction_project_id: Option<String>,
    global_input: Entity<ComposerInput>,
    project_input: Entity<ComposerInput>,
    instruction_load_task: Option<Task<()>>,
    instruction_save_task: Option<Task<()>>,
    inventory_task: Option<Task<()>>,
    instruction_saving: bool,
    skill_document: Loadable<SkillDocumentView>,
    skill_editor_generation: u64,
    skill_identity: Option<String>,
    skill_creating: bool,
    skill_scope: IntegrationSkillScope,
    skill_name_input: Entity<ComposerInput>,
    skill_content_input: Entity<ComposerInput>,
    skill_busy: bool,
    skill_task: Option<Task<()>>,
    skill_error: Option<String>,
    skill_notice: Option<String>,
    skill_remove_armed: bool,
    skill_install_provider: HarnessId,
    skill_install_scope: IntegrationSkillScope,
    runtime_contract: Loadable<RuntimeContractView>,
    runtime_task: Option<Task<()>>,
    runtime_task_id: Option<String>,
    /// CC-11 provider MCP actions and results.
    mcp: mcp::McpPanel,
    claude_sessions: crate::claude_advanced::ExternalSessionsState,
    claude_sessions_task: Option<Task<()>>,
    /// CC-12: effective Claude settings sources, style, hooks, commands.
    native_extensions: Entity<super::native_extensions::NativeExtensionsPanel>,
    _state_observer: Subscription,
}

impl HarnessesPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let global_input = cx.new(|cx| ComposerInput::new("Instructions for every LUNA task", cx));
        let project_input = cx.new(|cx| ComposerInput::new("Instructions for this project", cx));
        let skill_name_input = cx.new(|cx| ComposerInput::new("skill-name", cx));
        let skill_content_input = cx.new(|cx| ComposerInput::new("SKILL.md content", cx));
        let state_observer = cx.observe(&state, |page, _, cx| {
            if page.target_device.is_some() {
                return;
            }
            let task_id = page.state.read(cx).selected_chat.clone();
            if task_id != page.runtime_task_id {
                page.load_runtime_contract(cx);
                // Claude MCP attachment is per selected chat session.
                if page.integration_provider == HarnessId::ClaudeCode {
                    page.load_inventory(false, cx);
                }
                cx.notify();
            }
            let project_id = page
                .state
                .read(cx)
                .selected_space_row()
                .map(|space| space.id.clone());
            if project_id != page.instruction_project_id {
                page.load_instructions(cx);
                page.load_inventory(false, cx);
                page.clear_skill_editor();
                cx.notify();
            }
        });
        let native_extensions =
            cx.new(|cx| super::native_extensions::NativeExtensionsPanel::new(state.clone(), cx));
        let mut page = Self {
            title_settings: Loadable::Idle,
            title_models: Loadable::Idle,
            title_menu: None,
            title_task: None,
            title_saving: false,
            state,
            harnesses: Loadable::Idle,
            target_device: None,
            error: None,
            load_task: None,
            model_refresh_task: None,
            model_refresh_running: false,
            model_refresh_result: None,
            instructions: Loadable::Idle,
            inventory: Loadable::Idle,
            integration_provider: HarnessId::Codex,
            instruction_project_id: None,
            global_input,
            project_input,
            instruction_load_task: None,
            instruction_save_task: None,
            inventory_task: None,
            instruction_saving: false,
            skill_document: Loadable::Idle,
            skill_editor_generation: 0,
            skill_identity: None,
            skill_creating: false,
            skill_scope: IntegrationSkillScope::Global,
            skill_name_input,
            skill_content_input,
            skill_busy: false,
            skill_task: None,
            skill_error: None,
            skill_notice: None,
            skill_remove_armed: false,
            skill_install_provider: HarnessId::ClaudeCode,
            skill_install_scope: IntegrationSkillScope::Global,
            runtime_contract: Loadable::Idle,
            runtime_task: None,
            runtime_task_id: None,
            mcp: mcp::McpPanel::default(),
            claude_sessions: Default::default(),
            claude_sessions_task: None,
            native_extensions,
            _state_observer: state_observer,
        };
        page.load(cx);
        page.load_claude_sessions(cx);
        page
    }

    /// Params with the `targetDeviceId` passthrough merged in.
    fn with_target(&self, mut value: serde_json::Value) -> serde_json::Value {
        if let (Some(target), Some(object)) = (&self.target_device, value.as_object_mut()) {
            object.insert("targetDeviceId".into(), serde_json::json!(target));
        }
        value
    }

    /// Retarget the page at another device: a different device is a different
    /// install/enablement world, so drop the rows and reload through it.
    pub(crate) fn set_target_device(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        if self.target_device == target {
            cx.notify();
            return;
        }
        self.title_task = None;
        self.model_refresh_task = None;
        self.model_refresh_running = false;
        self.model_refresh_result = None;
        self.title_settings = Loadable::Idle;
        self.title_models = Loadable::Idle;
        self.title_menu = None;
        self.title_saving = false;
        self.target_device = target;
        self.error = None;
        self.harnesses = Loadable::Idle;
        self.instructions = Loadable::Idle;
        self.inventory = Loadable::Idle;
        self.clear_skill_editor();
        self.runtime_contract = Loadable::Idle;
        self.runtime_task_id = None;
        self.load(cx);
        cx.notify();
    }

    /// `ListHarnesses` against the target device (installed probe + enabled
    /// set both come from where the CLIs actually live).
    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = self.with_target(serde_json::json!({}));
        self.load_titles(None, cx);
        if self.target_device.is_none() {
            self.load_instructions(cx);
            self.load_inventory(false, cx);
            self.load_runtime_contract(cx);
        }
        self.harnesses = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::LIST_HARNESSES, params).await;
            this.update(cx, |page, cx| {
                page.harnesses = match result {
                    Ok(value) => match serde_json::from_value::<Vec<HarnessDescriptor>>(value) {
                        Ok(list) => Loadable::Ready(list),
                        Err(err) => Loadable::Error(err.to_string()),
                    },
                    Err(err) => Loadable::Error(err.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn load_titles(&mut self, save: Option<TitleSettings>, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let saving = save.is_some();
        let method = if saving {
            methods::SET_TITLE_SETTINGS
        } else {
            methods::GET_TITLE_SETTINGS
        };
        let params = self.with_target(
            save.map(|s| serde_json::to_value(s).unwrap())
                .unwrap_or_else(|| serde_json::json!({})),
        );
        let target = self.target_device.clone();
        self.title_menu = None;
        self.title_saving = saving;
        self.title_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(method, params)
                .await
                .map_err(|e| e.to_string())
                .and_then(|v| {
                    serde_json::from_value::<TitleSettings>(v).map_err(|e| e.to_string())
                });
            let settings = match result {
                Ok(settings) => settings,
                Err(error) => {
                    this.update(cx, |page, cx| {
                        if saving {
                            page.error = Some(error);
                        } else {
                            page.title_settings = Loadable::Error(error);
                        }
                        page.title_saving = false;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let harness = settings.harness;
            this.update(cx, |page, cx| {
                page.title_settings = Loadable::Ready(settings);
                page.title_models = if harness.is_some() {
                    Loadable::Loading
                } else {
                    Loadable::Idle
                };
                page.title_saving = false;
                page.error = None;
                cx.notify();
            })
            .ok();
            if let Some(harness) = harness {
                let result = engine
                    .client()
                    .call(
                        methods::LIST_MODELS,
                        serde_json::json!({"harness": harness, "targetDeviceId": target}),
                    )
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|v| {
                        serde_json::from_value::<Vec<Model>>(v).map_err(|e| e.to_string())
                    });
                this.update(cx, |page, cx| {
                    page.title_models = match result {
                        Ok(models) => Loadable::Ready(models),
                        Err(error) => Loadable::Error(error),
                    };
                    cx.notify();
                })
                .ok();
            }
        }));
        cx.notify();
    }

    fn load_instructions(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let project_id = self
            .state
            .read(cx)
            .selected_space_row()
            .map(|space| space.id.clone());
        self.instruction_project_id = project_id.clone();
        self.instructions = Loadable::Loading;
        self.instruction_load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::GET_INSTRUCTION_SETTINGS,
                    serde_json::json!({"projectId": project_id}),
                )
                .await
                .map_err(|e| e.to_string())
                .and_then(|value| {
                    serde_json::from_value::<InstructionSettingsView>(value)
                        .map_err(|e| e.to_string())
                });
            this.update(cx, |page, cx| {
                if page.target_device.is_some()
                    || page
                        .state
                        .read(cx)
                        .selected_space_row()
                        .map(|space| space.id.clone())
                        != project_id
                {
                    return;
                }
                page.instructions = match result {
                    Ok(view) => {
                        page.global_input
                            .update(cx, |input, cx| input.set_text(view.global.clone(), cx));
                        page.project_input
                            .update(cx, |input, cx| input.set_text(view.project.clone(), cx));
                        Loadable::Ready(view)
                    }
                    Err(error) => Loadable::Error(error),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn save_instructions(&mut self, project: bool, cx: &mut Context<Self>) {
        let Loadable::Ready(view) = &self.instructions else {
            return;
        };
        if self.instruction_saving {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let project_id = if project {
            self.instruction_project_id.clone()
        } else {
            None
        };
        if project && project_id.is_none() {
            return;
        }
        let text = if project {
            self.project_input.read(cx).text()
        } else {
            self.global_input.read(cx).text()
        }
        .to_string();
        let revision = view.revision;
        let display_project_id = self.instruction_project_id.clone();
        self.instruction_saving = true;
        self.instruction_save_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(
                methods::SET_INSTRUCTION_SETTINGS,
                serde_json::json!({"projectId": project_id, "text": text, "expectedRevision": revision}),
            ).await.map_err(|e| e.to_string()).and_then(|value| {
                serde_json::from_value::<InstructionSettingsView>(value).map_err(|e| e.to_string())
            });
            this.update(cx, |page, cx| {
                page.finish_instruction_save(result, display_project_id, cx);
            }).ok();
        }));
        cx.notify();
    }

    fn finish_instruction_save(
        &mut self,
        result: Result<InstructionSettingsView, String>,
        origin_project_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.instruction_saving = false;
        let current_project_id = self
            .state
            .read(cx)
            .selected_space_row()
            .map(|space| space.id.clone());
        let same_scope = self.target_device.is_none()
            && self.instruction_project_id == origin_project_id
            && current_project_id == origin_project_id;
        match result {
            Ok(_) => {
                // A global write returns a global-only view. Reload the
                // selected project and its new revision instead of installing
                // that response over the project's editor.
                self.error = None;
                if self.target_device.is_none() {
                    self.load_instructions(cx);
                    self.load_runtime_contract(cx);
                }
            }
            Err(error) => {
                self.error = Some(error);
                // Keep unsaved input for a same-scope retry. A changed scope
                // must show the newly selected project's own content.
                if !same_scope && self.target_device.is_none() {
                    self.load_instructions(cx);
                }
            }
        }
        cx.notify();
    }

    fn load_inventory(&mut self, probe_provider: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let provider = self.integration_provider;
        let project_id = self
            .state
            .read(cx)
            .selected_space_row()
            .map(|space| space.id.clone());
        let task_id = self.state.read(cx).selected_chat.clone();
        self.mcp_sync_scope(cx);
        self.inventory = Loadable::Loading;
        self.inventory_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::GET_INTEGRATION_INVENTORY,
                    serde_json::json!({"provider": provider, "projectId": project_id, "taskId": task_id, "probeProvider": probe_provider}),
                )
                .await
                .map_err(|e| e.to_string())
                .and_then(|value| {
                    serde_json::from_value::<IntegrationInventoryView>(value)
                        .map_err(|e| e.to_string())
                });
            this.update(cx, |page, cx| {
                if page.target_device.is_some()
                    || page.integration_provider != provider
                    || page
                        .state
                        .read(cx)
                        .selected_space_row()
                        .map(|space| space.id.clone())
                        != project_id
                {
                    return;
                }
                page.inventory = match result {
                    Ok(view) => Loadable::Ready(view),
                    Err(error) => Loadable::Error(error),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn clear_skill_editor(&mut self) {
        self.skill_editor_generation = self.skill_editor_generation.wrapping_add(1);
        self.skill_document = Loadable::Idle;
        self.skill_identity = None;
        self.skill_creating = false;
        self.skill_remove_armed = false;
        self.skill_error = None;
        self.skill_notice = None;
    }

    fn selected_skill_project_id(&self, cx: &Context<Self>) -> Option<String> {
        self.state
            .read(cx)
            .selected_space_row()
            .map(|space| space.id.clone())
    }

    fn create_skill(&mut self, scope: IntegrationSkillScope, cx: &mut Context<Self>) {
        if self.skill_busy
            || (scope == IntegrationSkillScope::Project
                && self.selected_skill_project_id(cx).is_none())
        {
            return;
        }
        self.clear_skill_editor();
        self.skill_creating = true;
        self.skill_scope = scope;
        self.skill_name_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.skill_content_input.update(cx, |input, cx| {
            input.set_text(
                "---\nname: new-skill\ndescription: Describe when this skill should be used.\n---\n\n# Instructions\n\n",
                cx,
            )
        });
        cx.notify();
    }

    fn open_skill(&mut self, identity: String, cx: &mut Context<Self>) {
        if self.skill_busy {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let provider = self.integration_provider;
        let project_id = self.selected_skill_project_id(cx);
        self.clear_skill_editor();
        self.skill_identity = Some(identity.clone());
        self.skill_document = Loadable::Loading;
        self.skill_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::GET_SKILL_DOCUMENT,
                    serde_json::json!({"provider": provider, "projectId": project_id, "identity": identity}),
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<SkillDocumentView>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |page, cx| {
                if page.target_device.is_some()
                    || page.integration_provider != provider
                    || page.selected_skill_project_id(cx) != project_id
                    || page.skill_identity.as_deref() != Some(identity.as_str())
                {
                    return;
                }
                page.skill_document = match result {
                    Ok(document) => {
                        page.skill_scope = document.scope;
                        page.skill_install_provider = if provider == HarnessId::Codex {
                            HarnessId::ClaudeCode
                        } else {
                            HarnessId::Codex
                        };
                        page.skill_install_scope = document.scope;
                        page.skill_content_input.update(cx, |input, cx| {
                            input.set_text(document.content.clone(), cx)
                        });
                        Loadable::Ready(document)
                    }
                    Err(error) => Loadable::Error(error),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn save_skill(&mut self, cx: &mut Context<Self>) {
        if self.skill_busy || self.target_device.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let provider = self.integration_provider;
        let project_id = self.selected_skill_project_id(cx);
        let editor_generation = self.skill_editor_generation;
        let (scope, name, expected_revision) = if self.skill_creating {
            (
                self.skill_scope,
                self.skill_name_input.read(cx).text().trim().to_owned(),
                None,
            )
        } else if let Loadable::Ready(document) = &self.skill_document {
            if document.read_only_reason.is_some() {
                return;
            }
            (
                document.scope,
                document.name.clone(),
                Some(document.revision.clone()),
            )
        } else {
            return;
        };
        let mut content = self.skill_content_input.read(cx).text().to_string();
        if self.skill_creating {
            content = content.replacen("name: new-skill\n", &format!("name: {name}\n"), 1);
        }
        self.skill_busy = true;
        self.skill_error = None;
        self.skill_notice = None;
        self.skill_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SAVE_SKILL_DOCUMENT,
                    serde_json::json!({"provider": provider, "projectId": project_id, "scope": scope, "name": name, "content": content, "expectedRevision": expected_revision}),
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<SkillDocumentView>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |page, cx| {
                page.skill_busy = false;
                if page.target_device.is_some()
                    || page.integration_provider != provider
                    || page.selected_skill_project_id(cx) != project_id
                {
                    return;
                }
                if page.skill_editor_generation != editor_generation {
                    if result.is_ok() {
                        page.load_inventory(false, cx);
                    }
                    cx.notify();
                    return;
                }
                match result {
                    Ok(document) => {
                        page.skill_creating = false;
                        page.skill_identity = Some(document.identity.clone());
                        page.skill_content_input.update(cx, |input, cx| {
                            input.set_text(document.content.clone(), cx)
                        });
                        page.skill_document = Loadable::Ready(document);
                        page.skill_remove_armed = false;
                        page.skill_notice = Some("Skill saved. Existing provider sessions may need a fresh turn to discover it.".into());
                        page.load_inventory(false, cx);
                    }
                    Err(error) => page.skill_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn install_skill(&mut self, cx: &mut Context<Self>) {
        let Loadable::Ready(document) = &self.skill_document else {
            return;
        };
        if self.skill_busy || document.read_only_reason.is_some() {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let source_provider = document.provider;
        let source_identity = document.identity.clone();
        let expected_source_revision = document.revision.clone();
        let destination_provider = self.skill_install_provider;
        let destination_scope = self.skill_install_scope;
        let project_id = self.selected_skill_project_id(cx);
        if destination_scope == IntegrationSkillScope::Project && project_id.is_none() {
            return;
        }
        self.skill_busy = true;
        self.skill_error = None;
        self.skill_notice = None;
        self.skill_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::INSTALL_SKILL,
                    serde_json::json!({"sourceProvider": source_provider, "projectId": project_id, "sourceIdentity": source_identity, "expectedSourceRevision": expected_source_revision, "destinationProvider": destination_provider, "destinationScope": destination_scope}),
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<SkillDocumentView>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |page, cx| {
                page.skill_busy = false;
                if page.target_device.is_some()
                    || page.integration_provider != source_provider
                    || page.selected_skill_project_id(cx) != project_id
                    || page.skill_identity.as_deref() != Some(source_identity.as_str())
                {
                    return;
                }
                match result {
                    Ok(document) => {
                        page.integration_provider = destination_provider;
                        page.skill_identity = Some(document.identity.clone());
                        page.skill_content_input.update(cx, |input, cx| {
                            input.set_text(document.content.clone(), cx)
                        });
                        page.skill_document = Loadable::Ready(document);
                        page.skill_remove_armed = false;
                        page.skill_notice = Some("Skill installed with its supporting files.".into());
                        page.load_inventory(false, cx);
                    }
                    Err(error) => page.skill_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn remove_skill(&mut self, cx: &mut Context<Self>) {
        let Loadable::Ready(document) = &self.skill_document else {
            return;
        };
        if self.skill_busy || document.read_only_reason.is_some() {
            return;
        }
        if !self.skill_remove_armed {
            self.skill_remove_armed = true;
            cx.notify();
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let provider = document.provider;
        let identity = document.identity.clone();
        let expected_revision = document.revision.clone();
        let project_id = self.selected_skill_project_id(cx);
        self.skill_busy = true;
        self.skill_error = None;
        self.skill_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::REMOVE_SKILL,
                    serde_json::json!({"provider": provider, "projectId": project_id, "identity": identity, "expectedRevision": expected_revision}),
                )
                .await;
            this.update(cx, |page, cx| {
                page.skill_busy = false;
                if page.target_device.is_some()
                    || page.integration_provider != provider
                    || page.selected_skill_project_id(cx) != project_id
                    || page.skill_identity.as_deref() != Some(identity.as_str())
                {
                    return;
                }
                match result {
                    Ok(_) => {
                        page.clear_skill_editor();
                        page.skill_notice = Some("Skill and its supporting files removed.".into());
                        page.load_inventory(false, cx);
                    }
                    Err(error) => {
                        page.skill_error = Some(error.to_string());
                        page.skill_remove_armed = false;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn load_runtime_contract(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(task_id) = self.state.read(cx).selected_chat.clone() else {
            self.runtime_task_id = None;
            self.runtime_task = None;
            self.runtime_contract = Loadable::Idle;
            return;
        };
        self.runtime_task_id = Some(task_id.clone());
        self.runtime_contract = Loadable::Loading;
        self.runtime_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::GET_RUNTIME_CONTRACT,
                    serde_json::json!({"taskId": task_id}),
                )
                .await
                .map_err(|e| e.to_string())
                .and_then(|value| {
                    serde_json::from_value::<RuntimeContractView>(value).map_err(|e| e.to_string())
                });
            this.update(cx, |page, cx| {
                if page.target_device.is_some()
                    || page.state.read(cx).selected_chat.as_deref() != Some(task_id.as_str())
                {
                    return;
                }
                page.runtime_contract = match result {
                    Ok(view) => Loadable::Ready(view),
                    Err(error) => Loadable::Error(error),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn render_runtime_contract(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut card = widgets::section_card(theme).mt(px(20.0)).p(px(16.0))
            .child(widgets::row_title(theme, "Effective session"))
            .child(widgets::page_subtitle(theme,
                "Redacted readback for the selected task. Instruction text and provider credentials are never shown here."))
            .child(widgets::ghost_action(theme).id("refresh-effective-session")
                .accessible_button("Refresh effective session", theme.accent)
                .mt(px(8.0)).hover(|s| widgets::ghost_hover(theme, s))
                .on_click(cx.listener(|page, _, _, cx| { page.load_runtime_contract(cx); cx.notify(); }))
                .child("Refresh"));
        let selected_task = self.state.read(cx).selected_chat.clone();
        match &self.runtime_contract {
            Loadable::Ready(view)
                if !matches_selected_contract(
                    view,
                    selected_task.as_deref(),
                    self.target_device.as_deref(),
                ) =>
            {
                card = card.child(meta_text(
                    theme,
                    "The selected task changed. Refreshing session evidence…",
                ));
            }
            Loadable::Ready(view) => {
                card = card.child(meta_text(theme, format!("Task: {}", view.task_id)));
                if let Some(effective) = &view.effective {
                    card = card.child(meta_text(
                        theme,
                        format!(
                            "Contract v{} · {} · {:?} · {}",
                            effective.contract_version,
                            effective.contract_hash,
                            effective.delivery,
                            if effective.restart_required {
                                "restart required"
                            } else {
                                "current at last provider acknowledgment"
                            },
                        ),
                    ));
                    for instruction in &effective.instructions {
                        card = card.child(div().mt(px(6.0)).child(meta_text(
                            theme,
                            format!(
                                "{} · {} characters · {}",
                                instruction.name,
                                instruction.character_count,
                                instruction.provenance
                            ),
                        )));
                    }
                    for tool in &effective.attached_tools {
                        card = card.child(div().mt(px(6.0)).child(meta_text(
                            theme,
                            format!("Tool attached: {}@{}", tool.name, tool.version),
                        )));
                    }
                    for tool in &effective.omitted_tools {
                        card = card.child(div().mt(px(6.0)).child(meta_text(
                            theme,
                            format!("Tool unavailable: {} · {}", tool.name, tool.reason),
                        )));
                    }
                } else {
                    card = card.child(meta_text(
                        theme,
                        view.unavailable_reason.clone().unwrap_or_else(|| {
                            "No provider session has acknowledged a LUNA contract.".into()
                        }),
                    ));
                }
            }
            Loadable::Error(error) => card = card.child(widgets::error_strip(theme, error.clone())),
            Loadable::Idle => {
                card = card.child(meta_text(
                    theme,
                    "Select a task to inspect its effective session.",
                ))
            }
            Loadable::Loading => card = card.child(meta_text(theme, "Loading effective session…")),
        }
        card.into_any_element()
    }

    fn render_integrations(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut card = widgets::section_card(theme).mt(px(20.0)).p(px(16.0))
            .child(widgets::row_title(theme, "Skill library and MCP"))
            .child(widgets::page_subtitle(theme,
                "Browse provider-owned skills by global or project scope. Check provider reads Codex's native catalog or Claude's own MCP server readback; file presence and selected-session attachment are separate checks."))
            .child(skill_button(theme, "refresh-integrations", "Check provider skill and MCP catalog", !self.skill_busy)
                .mt(px(8.0))
                .when(!self.skill_busy, |el| el.hover(|s| widgets::ghost_hover(theme, s))
                    .on_click(cx.listener(|page, _, _, cx| { page.load_inventory(true, cx); cx.notify(); })))
                .child("Check provider"));
        if let Loadable::Ready(harnesses) = &self.harnesses {
            let mut picker = div()
                .mt(px(10.0))
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0));
            for harness in harnesses {
                let id = harness.id;
                if !matches!(
                    id,
                    HarnessId::ClaudeCode | HarnessId::Codex | HarnessId::Opencode
                ) {
                    continue;
                }
                let selected = id == self.integration_provider;
                picker = picker.child(
                    skill_button(
                        theme,
                        SharedString::from(format!("integration-provider-{id:?}")),
                        format!("Show {} skills", harness.name),
                        !self.skill_busy,
                    )
                    .aria_toggled(if selected {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .when(selected, |el| el.bg(theme.accent_wash))
                    .when(!self.skill_busy, |el| {
                        el.hover(|s| widgets::ghost_hover(theme, s))
                            .on_click(cx.listener(move |page, _, _, cx| {
                                page.integration_provider = id;
                                page.clear_skill_editor();
                                page.load_inventory(false, cx);
                                cx.notify();
                            }))
                    })
                    .child(harness.name.clone()),
                );
            }
            card = card.child(picker);
        }
        let has_project = self.selected_skill_project_id(cx).is_some();
        card = card.child(
            div()
                .mt(px(10.0))
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0))
                .child(
                    skill_button(
                        theme,
                        "create-global-skill",
                        "New global skill",
                        !self.skill_busy,
                    )
                    .when(!self.skill_busy, |el| {
                        el.hover(|style| widgets::ghost_hover(theme, style))
                            .on_click(cx.listener(|page, _, _, cx| {
                                page.create_skill(IntegrationSkillScope::Global, cx)
                            }))
                    })
                    .child("New global skill"),
                )
                .when(has_project, |row| {
                    row.child(
                        skill_button(
                            theme,
                            "create-project-skill",
                            "New project skill",
                            !self.skill_busy,
                        )
                        .when(!self.skill_busy, |el| {
                            el.hover(|style| widgets::ghost_hover(theme, style))
                                .on_click(cx.listener(|page, _, _, cx| {
                                    page.create_skill(IntegrationSkillScope::Project, cx)
                                }))
                        })
                        .child("New project skill"),
                    )
                }),
        );
        match &self.inventory {
            Loadable::Ready(view) => {
                card = card.child(meta_text(theme, format!("Device: {}", view.machine_id)));
                if let Some(task_id) = self.state.read(cx).selected_chat.as_deref() {
                    card = card.child(meta_text(theme, format!("Selected task: {task_id}. Skill attachment reflects only exact provider session evidence; unknown means no verified readback.")));
                }
                if let Some(reason) = &view.unavailable_reason {
                    card = card.child(meta_text(theme, reason.clone()));
                }
                for item in &view.items {
                    let kind = if item.kind == IntegrationKind::Skill {
                        "Skill"
                    } else {
                        "MCP"
                    };
                    let available = match item.available {
                        Some(true) => "available",
                        Some(false) => "unavailable",
                        None => "availability unknown",
                    };
                    let attached = match item.attached {
                        Some(true) => "attached",
                        Some(false) => "not attached",
                        None => "attachment unknown",
                    };
                    card = card.child(
                        div()
                            .mt(px(12.0))
                            .child(widgets::row_title(theme, format!("{kind} · {}", item.name)))
                            .child(meta_text(
                                theme,
                                format!(
                                    "{} · {} · configured · {available} · {attached}",
                                    item.source, item.scope
                                ),
                            ))
                            .when_some(item.reason.clone(), |el, reason| {
                                el.child(meta_text(theme, reason))
                            })
                            .when_some(item.actions.reason.clone(), |el, reason| {
                                el.child(meta_text(theme, reason))
                            })
                            .when_some(item.mcp.as_ref(), |el, details| {
                                el.child(self.render_mcp_details(theme, item, details, cx))
                            })
                            .when(
                                item.kind == IntegrationKind::Skill && item.identity.is_some(),
                                |el| {
                                    let identity = item.identity.clone().unwrap_or_default();
                                    el.child(
                                        skill_button(
                                            theme,
                                            SharedString::from(format!("inspect-skill-{identity}")),
                                            format!("Inspect {} {} skill", item.scope, item.name),
                                            !self.skill_busy,
                                        )
                                        .mt(px(6.0))
                                        .when(!self.skill_busy, |button| {
                                            button
                                                .hover(|style| widgets::ghost_hover(theme, style))
                                                .on_click(cx.listener(move |page, _, _, cx| {
                                                    page.open_skill(identity.clone(), cx)
                                                }))
                                        })
                                        .child("Inspect / edit"),
                                    )
                                },
                            ),
                    );
                }
                if let Some(panel) = self.render_mcp_panel(theme) {
                    card = card.child(panel);
                }
                card = card.child(div().mt(px(8.0)).child(meta_text(theme, view.help.clone())));
            }
            Loadable::Error(error) => card = card.child(widgets::error_strip(theme, error.clone())),
            _ => card = card.child(meta_text(theme, "Loading provider inventory…")),
        }
        card.into_any_element()
    }

    fn render_skill_editor(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut card = widgets::section_card(theme).mt(px(20.0)).p(px(16.0))
            .child(widgets::row_title(theme, "Skill details"))
            .child(widgets::page_subtitle(theme,
                "LUNA edits the provider's SKILL.md in its selected global or project folder. Existing sessions may need a fresh turn to discover a change."))
            .child(skill_button(theme, "close-skill-editor", "Close skill details", !self.skill_busy)
                .mt(px(8.0))
                .when(!self.skill_busy, |el| el.hover(|style| widgets::ghost_hover(theme, style))
                    .on_click(cx.listener(|page, _, _, cx| { page.clear_skill_editor(); cx.notify(); })))
                .when(self.skill_busy, |el| el.opacity(0.5))
                .child("Close"));
        if let Some(error) = &self.skill_error {
            card = card.child(widgets::error_strip(theme, error.clone()));
        }
        if let Some(notice) = &self.skill_notice {
            card = card.child(div().mt(px(8.0)).child(meta_text(theme, notice.clone())));
        }
        if self.skill_creating {
            let scope = if self.skill_scope == IntegrationSkillScope::Global {
                "Global"
            } else {
                "Project"
            };
            card = card
                .child(div().mt(px(12.0)).child(widgets::row_title(theme, format!("New {scope} skill"))))
                .child(meta_text(theme, "Use a lowercase name with digits or single hyphens. Edit the description and instructions before saving."))
                .child(div().mt(px(8.0)).child(popover::dialog_field(self.skill_name_input.clone().into_any_element())));
        }
        match &self.skill_document {
            Loadable::Loading => {
                return card
                    .child(meta_text(theme, "Loading skill…"))
                    .into_any_element();
            }
            Loadable::Error(error) => {
                return card
                    .child(widgets::error_strip(theme, error.clone()))
                    .into_any_element();
            }
            Loadable::Ready(document) => {
                let scope = if document.scope == IntegrationSkillScope::Global {
                    "global"
                } else {
                    "project"
                };
                card = card
                    .child(
                        div()
                            .mt(px(12.0))
                            .child(widgets::row_title(theme, document.name.clone())),
                    )
                    .child(meta_text(
                        theme,
                        format!(
                            "{scope} · {:?} · revision {}",
                            document.provider,
                            &document.revision[..12]
                        ),
                    ))
                    .when_some(document.read_only_reason.clone(), |el, reason| {
                        el.child(meta_text(theme, reason))
                    });
                if !document.supporting_files.is_empty() {
                    let names = document
                        .supporting_files
                        .iter()
                        .take(12)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ");
                    let more = document.supporting_files.len().saturating_sub(12);
                    card = card.child(meta_text(
                        theme,
                        format!(
                            "Supporting files: {names}{}",
                            if more > 0 {
                                format!(" +{more} more")
                            } else {
                                String::new()
                            }
                        ),
                    ));
                }
            }
            Loadable::Idle if !self.skill_creating => return card.into_any_element(),
            Loadable::Idle => {}
        }
        let save_enabled = !self.skill_busy
            && (self.skill_creating
                || matches!(&self.skill_document, Loadable::Ready(document) if document.read_only_reason.is_none()));
        card = card
            .child(
                div()
                    .mt(px(12.0))
                    .child(widgets::row_title(theme, "SKILL.md")),
            )
            .child(div().mt(px(6.0)).child(popover::dialog_field(
                self.skill_content_input.clone().into_any_element(),
            )))
            .child(
                skill_button(
                    theme,
                    "save-skill-document",
                    if self.skill_creating {
                        "Create skill"
                    } else {
                        "Save skill"
                    },
                    save_enabled,
                )
                .mt(px(8.0))
                .when(save_enabled, |el| {
                    el.hover(|style| widgets::ghost_hover(theme, style))
                        .on_click(cx.listener(|page, _, _, cx| page.save_skill(cx)))
                })
                .when(!save_enabled, |el| el.opacity(0.5))
                .child(if self.skill_busy {
                    "Saving or installing…"
                } else if self.skill_creating {
                    "Create skill"
                } else {
                    "Save skill"
                }),
            );

        if let Loadable::Ready(document) = &self.skill_document {
            card = card
                .child(div().mt(px(18.0)).child(widgets::row_title(theme, "Install a copy")))
                .child(meta_text(theme, "Copy this skill and its supporting files to another provider or scope. An existing skill with the same name is never replaced."));
            let mut providers = div().mt(px(8.0)).flex().flex_row().flex_wrap().gap(px(6.0));
            for (provider, label) in [
                (HarnessId::Codex, "Codex"),
                (HarnessId::ClaudeCode, "Claude Code"),
                (HarnessId::Opencode, "OpenCode"),
            ] {
                providers = providers.child(
                    skill_button(
                        theme,
                        SharedString::from(format!("skill-install-provider-{provider:?}")),
                        format!("Install to {label}"),
                        !self.skill_busy,
                    )
                    .aria_toggled(if self.skill_install_provider == provider {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .when(self.skill_install_provider == provider, |el| {
                        el.bg(theme.accent_wash)
                    })
                    .when(!self.skill_busy, |el| {
                        el.hover(|style| widgets::ghost_hover(theme, style))
                            .on_click(cx.listener(move |page, _, _, cx| {
                                page.skill_install_provider = provider;
                                cx.notify();
                            }))
                    })
                    .child(label),
                );
            }
            card = card.child(providers);
            let mut scopes = div().mt(px(8.0)).flex().flex_row().flex_wrap().gap(px(6.0));
            for (scope, label) in [
                (IntegrationSkillScope::Global, "Global"),
                (IntegrationSkillScope::Project, "Current project"),
            ] {
                if scope == IntegrationSkillScope::Project
                    && self.selected_skill_project_id(cx).is_none()
                {
                    continue;
                }
                scopes = scopes.child(
                    skill_button(
                        theme,
                        SharedString::from(format!("skill-install-scope-{scope:?}")),
                        format!("Install to {label}"),
                        !self.skill_busy,
                    )
                    .aria_toggled(if self.skill_install_scope == scope {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                    .when(self.skill_install_scope == scope, |el| {
                        el.bg(theme.accent_wash)
                    })
                    .when(!self.skill_busy, |el| {
                        el.hover(|style| widgets::ghost_hover(theme, style))
                            .on_click(cx.listener(move |page, _, _, cx| {
                                page.skill_install_scope = scope;
                                cx.notify();
                            }))
                    })
                    .child(label),
                );
            }
            card = card.child(scopes);
            let install_valid = self.skill_install_provider != document.provider
                || self.skill_install_scope != document.scope;
            card = card.child(
                skill_button(
                    theme,
                    "install-skill-copy",
                    format!("Install copy of {}", document.name),
                    install_valid && !self.skill_busy && document.read_only_reason.is_none(),
                )
                .mt(px(8.0))
                .when(
                    install_valid && !self.skill_busy && document.read_only_reason.is_none(),
                    |el| {
                        el.hover(|style| widgets::ghost_hover(theme, style))
                            .on_click(cx.listener(|page, _, _, cx| page.install_skill(cx)))
                    },
                )
                .when(
                    !install_valid || self.skill_busy || document.read_only_reason.is_some(),
                    |el| el.opacity(0.5),
                )
                .child("Install copy"),
            );
            card = card
                .child(
                    div()
                        .mt(px(18.0))
                        .child(widgets::row_title(theme, "Remove skill")),
                )
                .child(meta_text(
                    theme,
                    format!(
                        "Removing deletes SKILL.md and {} supporting entries from this {} folder.",
                        document.supporting_files.len(),
                        if document.scope == IntegrationSkillScope::Global {
                            "global"
                        } else {
                            "project"
                        }
                    ),
                ))
                .child(
                    skill_button(
                        theme,
                        "remove-skill",
                        if self.skill_remove_armed {
                            "Confirm remove skill and files"
                        } else {
                            "Remove skill"
                        },
                        !self.skill_busy && document.read_only_reason.is_none(),
                    )
                    .mt(px(8.0))
                    .when(
                        !self.skill_busy && document.read_only_reason.is_none(),
                        |el| {
                            el.hover(|style| widgets::ghost_hover(theme, style))
                                .on_click(cx.listener(|page, _, _, cx| page.remove_skill(cx)))
                        },
                    )
                    .when(
                        self.skill_busy || document.read_only_reason.is_some(),
                        |el| el.opacity(0.5),
                    )
                    .child(if self.skill_remove_armed {
                        "Confirm remove skill and files"
                    } else {
                        "Remove…"
                    }),
                );
        }
        card.into_any_element()
    }

    fn render_instructions(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut card = widgets::section_card(theme).mt(px(20.0)).p(px(16.0))
            .child(widgets::row_title(theme, "LUNA instructions"))
            .child(widgets::page_subtitle(theme,
                "Global instructions precede project instructions. Provider-native files remain with their provider. Changes apply at the next safe turn or a new provider session."))
            .child(widgets::ghost_action(theme).id("reload-luna-instructions")
                .accessible_button("Reload LUNA instructions", theme.accent)
                .mt(px(8.0)).hover(|s| widgets::ghost_hover(theme, s))
                .on_click(cx.listener(|page, _, _, cx| { page.load_instructions(cx); cx.notify(); }))
                .child("Reload"));
        match &self.instructions {
            Loadable::Ready(view) => {
                card = card
                    .child(
                        div()
                            .mt(px(12.0))
                            .child(widgets::row_title(theme, "Global")),
                    )
                    .child(div().mt(px(6.0)).child(popover::dialog_field(
                        self.global_input.clone().into_any_element(),
                    )))
                    .child(
                        widgets::ghost_action(theme)
                            .id("save-global-instructions")
                            .when(!self.instruction_saving, |el| {
                                el.accessible_button("Save global instructions", theme.accent)
                            })
                            .mt(px(8.0))
                            .hover(|s| widgets::ghost_hover(theme, s))
                            .when(!self.instruction_saving, |el| {
                                el.on_click(
                                    cx.listener(|page, _, _, cx| page.save_instructions(false, cx)),
                                )
                            })
                            .child("Save global"),
                    );
                if view.project_id.is_some() {
                    card = card
                        .child(
                            div()
                                .mt(px(16.0))
                                .child(widgets::row_title(theme, "Current project")),
                        )
                        .child(div().mt(px(6.0)).child(popover::dialog_field(
                            self.project_input.clone().into_any_element(),
                        )))
                        .child(
                            widgets::ghost_action(theme)
                                .id("save-project-instructions")
                                .when(!self.instruction_saving, |el| {
                                    el.accessible_button("Save project instructions", theme.accent)
                                })
                                .mt(px(8.0))
                                .hover(|s| widgets::ghost_hover(theme, s))
                                .when(!self.instruction_saving, |el| {
                                    el.on_click(cx.listener(|page, _, _, cx| {
                                        page.save_instructions(true, cx)
                                    }))
                                })
                                .child("Save project"),
                        );
                } else {
                    card = card.child(meta_text(
                        theme,
                        "Select a project to edit its LUNA instructions.",
                    ));
                }
            }
            Loadable::Error(error) => card = card.child(widgets::error_strip(theme, error.clone())),
            _ => card = card.child(meta_text(theme, "Loading instructions…")),
        }
        card.into_any_element()
    }

    fn render_titles(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut card = widgets::section_card(theme).mt(px(20.0)).p(px(16.0))
            .child(widgets::row_title(theme, "Session titles"))
            .child(widgets::page_subtitle(theme, "Choose the agent and model for automatic titles on this device. Claude Code and Codex support restricted title generation."));
        let Loadable::Ready(settings) = &self.title_settings else {
            let message = match &self.title_settings {
                Loadable::Error(error) => error.clone(),
                _ => "Loading title settings…".into(),
            };
            return card
                .child(div().mt(px(8.0)).child(message))
                .into_any_element();
        };
        for is_model in [false, true] {
            let label = if is_model {
                settings
                    .model
                    .as_ref()
                    .map(|id| {
                        if let Loadable::Ready(models) = &self.title_models {
                            models
                                .iter()
                                .find(|m| &m.id == id)
                                .map(|m| m.label.clone())
                                .unwrap_or_else(|| id.clone())
                        } else {
                            id.clone()
                        }
                    })
                    .unwrap_or_else(|| "Automatic (cheapest model)".into())
            } else {
                settings
                    .harness
                    .map(|id| match id {
                        HarnessId::ClaudeCode => "Claude Code".to_string(),
                        HarnessId::Codex => "Codex".to_string(),
                        _ => format!("{id:?}"),
                    })
                    .unwrap_or_else(|| "Automatic (session agent when supported)".into())
            };
            let interactive = !self.title_saving && (!is_model || settings.harness.is_some());
            let title_aria = format!(
                "{}: {label}",
                if is_model {
                    "Title model"
                } else {
                    "Title harness"
                }
            );
            let mut row = div()
                .mt(px(12.0))
                .child(widgets::row_title(
                    theme,
                    if is_model {
                        "Title model"
                    } else {
                        "Title harness"
                    },
                ))
                .child(
                    widgets::ghost_action(theme)
                        .id(if is_model {
                            "title-model"
                        } else {
                            "title-harness"
                        })
                        .when(interactive, |el| {
                            el.accessible_button(title_aria.clone(), theme.accent)
                        })
                        .when(interactive, |el| {
                            el.cursor_pointer()
                                .on_click(cx.listener(move |page, _, _, cx| {
                                    page.title_menu = if page.title_menu == Some(is_model) {
                                        None
                                    } else {
                                        Some(is_model)
                                    };
                                    cx.notify();
                                }))
                        })
                        .when(!interactive, |el| el.opacity(0.5))
                        .child(label),
                );
            if self.title_menu == Some(is_model) {
                let mut choices = vec![(
                    "Automatic".to_string(),
                    TitleSettings {
                        harness: if is_model { settings.harness } else { None },
                        model: None,
                    },
                )];
                if is_model {
                    if let Loadable::Ready(models) = &self.title_models {
                        choices.extend(models.iter().map(|m| {
                            (
                                m.label.clone(),
                                TitleSettings {
                                    harness: settings.harness,
                                    model: Some(m.id.clone()),
                                },
                            )
                        }));
                    }
                } else if let Loadable::Ready(harnesses) = &self.harnesses {
                    choices.extend(
                        harnesses
                            .iter()
                            .filter(|h| {
                                descriptor_enabled(h)
                                    && h.installed
                                    && luna_harness::supports_titles(h.id)
                                    && h.id != HarnessId::Mock
                            })
                            .map(|h| {
                                (
                                    h.name.clone(),
                                    TitleSettings {
                                        harness: Some(h.id),
                                        model: None,
                                    },
                                )
                            }),
                    );
                }
                row =
                    row.child(
                        div()
                            .id(if is_model {
                                "title-model-options"
                            } else {
                                "title-harness-options"
                            })
                            .max_h(px(240.0))
                            .overflow_y_scroll()
                            .children(choices.into_iter().enumerate().map(
                                |(ix, (label, choice))| {
                                    popover::menu_row(
                                        theme,
                                        &choice == settings,
                                        format!("title-choice-{is_model}-{ix}"),
                                    )
                                    .id(("title-choice", ix))
                                    .on_click(cx.listener(move |page, _, _, cx| {
                                        page.load_titles(Some(choice.clone()), cx)
                                    }))
                                    .child(label)
                                },
                            )),
                    );
            }
            card = card.child(row);
        }
        if let Loadable::Error(error) = &self.title_models {
            card = card.child(widgets::error_strip(theme, error.clone()));
        }
        card.into_any_element()
    }

    fn refresh_model_cache(&mut self, cx: &mut Context<Self>) {
        if self.model_refresh_running {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let harnesses: Vec<_> = self
            .harnesses
            .ready()
            .into_iter()
            .flat_map(|items| items.iter())
            .filter(|item| item.installed && descriptor_enabled(item) && item.id != HarnessId::Mock)
            .map(|item| item.id)
            .collect();
        if harnesses.is_empty() {
            self.model_refresh_result = Some("No enabled agents on the selected device.".into());
            cx.notify();
            return;
        }
        let target = self.target_device.clone();
        let executor = cx.background_executor().clone();
        self.model_refresh_running = true;
        self.model_refresh_result = None;
        self.model_refresh_task = Some(cx.spawn(async move |this, cx| {
            let mut targets: Vec<(HarnessId, Option<String>)> = harnesses
                .iter()
                .copied()
                .filter(|harness| *harness != HarnessId::Codex)
                .map(|harness| (harness, None))
                .collect();
            let mut account_warning = None;
            if harnesses.contains(&HarnessId::Codex) {
                let mut params = serde_json::json!({"harness": HarnessId::Codex});
                if let Some(target) = &target {
                    params["targetDeviceId"] = target.clone().into();
                }
                match crate::attachments::call_with_timeout(
                    &engine,
                    &executor,
                    methods::LIST_AGENT_ACCOUNTS,
                    params,
                    Duration::from_secs(20),
                )
                .await
                {
                    Ok(value) => match serde_json::from_value::<AgentAccountsSnapshot>(value) {
                        Ok(snapshot) => {
                            let mut ids: Vec<_> = snapshot
                                .accounts
                                .into_iter()
                                .filter(|account| account.harness == HarnessId::Codex)
                                .map(|account| account.id)
                                .collect();
                            ids.sort();
                            ids.dedup();
                            targets.extend(ids.into_iter().map(|id| (HarnessId::Codex, Some(id))));
                        }
                        Err(error) => account_warning = Some(error.to_string()),
                    },
                    Err(error) => account_warning = Some(error.to_string()),
                }
                if !targets
                    .iter()
                    .any(|(harness, _)| *harness == HarnessId::Codex)
                {
                    targets.push((HarnessId::Codex, None));
                }
            }
            let total = targets.len();
            let results = stream::iter(targets)
                .map(|(harness, account_id)| {
                    let engine = engine.clone();
                    let target = target.clone();
                    let executor = executor.clone();
                    async move {
                        let mut params = serde_json::json!({"harness": harness, "refresh": true});
                        if let Some(account_id) = account_id {
                            params["accountId"] = account_id.into();
                        }
                        if let Some(target) = target {
                            params["targetDeviceId"] = target.into();
                        }
                        let timeout = if harness == HarnessId::Opencode {
                            Duration::from_secs(60)
                        } else {
                            Duration::from_secs(25)
                        };
                        let result = crate::attachments::call_with_timeout(
                            &engine,
                            &executor,
                            methods::LIST_MODELS,
                            params,
                            timeout,
                        )
                        .await
                        .and_then(|value| {
                            serde_json::from_value::<Vec<Model>>(value)
                                .map_err(|error| error.to_string())
                        });
                        (harness, result)
                    }
                })
                .buffer_unordered(2)
                .collect::<Vec<_>>()
                .await;
            this.update(cx, |page, cx| {
                if page.target_device != target {
                    return;
                }
                let failures: Vec<_> = results
                    .iter()
                    .filter_map(|(harness, result)| {
                        result
                            .as_ref()
                            .err()
                            .map(|error| format!("{harness:?}: {error}"))
                    })
                    .collect();
                let updated = total - failures.len();
                page.model_refresh_running = false;
                let mut message = if failures.is_empty() {
                    format!("Updated {updated} model catalogs on the selected device.")
                } else {
                    format!("Updated {updated}/{total}. {}", failures.join(" · "))
                };
                if let Some(warning) = account_warning {
                    message.push_str(&format!(" Codex account listing failed: {warning}"));
                }
                page.model_refresh_result = Some(message);
                if updated > 0 {
                    crate::pickers::bump_harness_catalog(cx);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn render_model_cache(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        widgets::section_card(theme)
            .mt(px(20.0))
            .p(px(16.0))
            .child(widgets::row_title(theme, "Model cache"))
            .child(widgets::page_subtitle(
                theme,
                "LUNA saves model lists on each device and refreshes them in the background after launch. Refresh now to check the selected device for new models without restarting.",
            ))
            .child(
                widgets::ghost_action(theme)
                    .id("refresh-model-cache")
                    .when(!self.model_refresh_running, |el| el.accessible_button("Refresh model cache", theme.accent))
                    .mt(px(8.0))
                    .when(!self.model_refresh_running, |element| {
                        element
                            .hover(|style| widgets::ghost_hover(theme, style))
                            .on_click(cx.listener(|page, _, _, cx| {
                                page.refresh_model_cache(cx);
                            }))
                    })
                    .when(self.model_refresh_running, |element| element.opacity(0.5))
                    .child(if self.model_refresh_running {
                        "Refreshing…"
                    } else {
                        "Refresh models"
                    }),
            )
            .when_some(self.model_refresh_result.clone(), |element, result| {
                element.child(div().mt(px(8.0)).child(meta_text(theme, result)))
            })
            .into_any_element()
    }

    pub(crate) fn render_advanced_sections(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let local = self.target_device.is_none();
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .when_some(self.error.clone(), |el, error| {
                el.child(widgets::error_strip(&theme, error))
            })
            .child(self.render_model_cache(&theme, cx))
            .when(local, |el| {
                el.child(self.render_integrations(&theme, cx))
                    .when(
                        self.skill_creating
                            || !matches!(&self.skill_document, Loadable::Idle)
                            || self.skill_notice.is_some(),
                        |el| el.child(self.render_skill_editor(&theme, cx)),
                    )
                    .child(self.native_extensions.clone())
                    .child(self.render_instructions(&theme, cx))
                    .child(self.render_runtime_contract(&theme, cx))
                    .child(self.render_claude_advanced(&theme, cx))
            })
            .child(self.render_titles(&theme, cx))
            .into_any_element()
    }
}

impl Render for HarnessesPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_advanced_sections(cx)
    }
}
