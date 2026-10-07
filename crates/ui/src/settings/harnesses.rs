//! Advanced controls embedded below the three provider cards: model catalog
//! refresh, title preferences, the skill and MCP inventory, instructions,
//! the runtime contract and the Claude cards. Device scope follows the parent
//! Providers page.
//!
//! Every card follows the Settings spec
//! (`docs/design/settings-polish-2026-10-06/spec.md`): one `section_card`
//! whose `section_header` carries the title, description and the section's
//! own actions; rows on the `card_row` anatomy with trailing kit controls;
//! wide content (editors, lists, chip groups) in a full-width block; loading
//! as skeleton rows; errors and warnings as strips inside their section.

use gpui::{
    AnyElement, App, Context, Entity, IntoElement, Render, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};

use crate::composer::ComposerInput;
use futures::{StreamExt, stream};
use luna_engine::registry::TitleSettings;
use luna_engine::registry::{HarnessDescriptor, descriptor_enabled};
use std::time::Duration;

use luna_proto::Model;
use luna_proto::{
    AgentAccountsSnapshot, ExecutionSettingField, ExecutionSettings, HarnessId,
    InstructionSettingsView, IntegrationInventoryView, IntegrationKind, IntegrationSkillScope,
    PermissionMode, RuntimeContractView, SandboxLevel, SkillDocumentView, SteeringMode,
    TaskExecutionSummary,
};
use luna_rpc::methods;

use crate::button::{self, ButtonKind, ControlSize, Segment};
use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::{FillTier, Theme};
use crate::typography::ui_rems;

mod mcp;
#[cfg(feature = "appshots-fixture")]
pub use mcp::fixture as mcp_fixture;
mod claude_sessions;

/// A kit push button with its accessible name and visible text. GPUI
/// synthesizes one keyboard click on key-up for a focused `on_click`
/// control, so callers wire only the click (and only when enabled).
pub(super) fn kit_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    kind: ButtonKind,
    size: ControlSize,
    aria: impl Into<SharedString>,
    text: impl Into<SharedString>,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    button::button(id, kind, size, enabled, theme)
        .aria_label(aria.into())
        .child(text.into())
}

/// A section header action: a medium secondary button with a leading glyph.
pub(super) fn section_action(
    theme: &Theme,
    id: impl Into<SharedString>,
    glyph: &'static str,
    aria: impl Into<SharedString>,
    text: impl Into<SharedString>,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    button::button(id, ButtonKind::Secondary, ControlSize::Md, enabled, theme)
        .aria_label(aria.into())
        .child(button::glyph(glyph, ControlSize::Md))
        .child(text.into())
}

/// A row holding one full-width element (a strip, a skeleton, a panel).
pub(super) fn full_row(theme: &Theme, child: impl IntoElement) -> gpui::Div {
    widgets::card_row(theme, false).child(div().w_full().min_w_0().child(child))
}

/// A row whose only content is secondary copy: an empty state or a status.
pub(super) fn note_row(theme: &Theme, copy: impl Into<SharedString>) -> gpui::Div {
    widgets::card_row(theme, false).child(widgets::row_description(theme, copy).flex_1())
}

/// An error or warning strip as a row of its section.
pub(super) fn strip_row(theme: &Theme, strip: gpui::Div) -> gpui::Div {
    full_row(theme, strip.mt_0())
}

/// Pulsing skeleton rows in place of a section's rows while it loads.
pub(super) fn skeleton_row(
    theme: &Theme,
    id: &'static str,
    count: usize,
    view: gpui::EntityId,
    cx: &mut App,
) -> gpui::Div {
    full_row(theme, popover::skeleton_rows(id, theme, count, view, cx))
}

/// A full-width vertical list under a row's text (facts, lines, editors).
pub(super) fn block_list() -> gpui::Div {
    widgets::row_block()
        .flex_col()
        .items_stretch()
        .gap(ui_rems(6.0))
}

/// The framed text area around a multi-line [`ComposerInput`] editor, on the
/// theme's well fill.
fn editor_frame(theme: &Theme, input: AnyElement) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .px(ui_rems(12.0))
        .py(ui_rems(8.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.separator(false))
        .bg(theme.fill(FillTier::Quaternary))
        .text_size(ui_rems(13.0))
        .line_height(ui_rems(19.0))
        .child(input)
}

/// The first `len` characters of an opaque revision, without slicing inside
/// a character or past the end.
fn short_revision(revision: &str, len: usize) -> &str {
    revision
        .char_indices()
        .nth(len)
        .map_or(revision, |(end, _)| &revision[..end])
}

/// One row of the Execution card's settings table.
struct ExecutionRow {
    setting: &'static str,
    running: Option<String>,
    requested: String,
    pending: bool,
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
    /// A failed title-settings save, shown in the Session titles card.
    title_error: Option<String>,
    /// A failed instruction save, shown in the LUNA instructions card.
    instruction_error: Option<String>,
    load_task: Option<Task<()>>,
    model_refresh_task: Option<Task<()>>,
    model_refresh_running: bool,
    /// The last refresh's outcome: `(all catalogs updated, message)`.
    model_refresh_result: Option<(bool, String)>,
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
    /// The engine connection generation the contract was requested on (a
    /// reconnect or profile change bumps it; older responses never install).
    runtime_generation: u64,
    /// QA: the Execution card's and its summary's painted bounds.
    #[cfg(feature = "appshots-fixture")]
    qa_bounds: std::rc::Rc<
        std::cell::RefCell<std::collections::HashMap<&'static str, gpui::Bounds<gpui::Pixels>>>,
    >,
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
            // The focused chat (a workbench tab, not Main); Claude's MCP
            // attachment below follows that same chat.
            let task_id = page.state.read(cx).execution_focus_chat();
            let generation = page.state.read(cx).engine_generation();
            if task_id != page.runtime_task_id || generation != page.runtime_generation {
                page.load_runtime_contract(cx);
                // Claude and OpenCode MCP attachment is per selected chat
                // session.
                if matches!(
                    page.integration_provider,
                    HarnessId::ClaudeCode | HarnessId::Opencode
                ) {
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
            title_error: None,
            instruction_error: None,
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
            runtime_generation: 0,
            #[cfg(feature = "appshots-fixture")]
            qa_bounds: Default::default(),
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
        self.title_error = None;
        self.instruction_error = None;
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
                            page.title_error = Some(error);
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
                page.title_error = None;
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
                self.instruction_error = None;
                if self.target_device.is_none() {
                    self.load_instructions(cx);
                    self.load_runtime_contract(cx);
                }
            }
            Err(error) => {
                self.instruction_error = Some(error);
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
        let Some(task_id) = self.state.read(cx).execution_focus_chat() else {
            self.runtime_task_id = None;
            self.runtime_task = None;
            self.runtime_contract = Loadable::Idle;
            return;
        };
        let generation = self.state.read(cx).engine_generation();
        self.runtime_task_id = Some(task_id.clone());
        self.runtime_generation = generation;
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
                page.install_runtime_contract(&task_id, generation, result, cx);
            })
            .ok();
        }));
    }

    /// A contract response for `task_id` requested on engine connection
    /// `generation`, unless that chat is no longer the focused one or the
    /// connection (engine, reconnect, profile) has changed since: a late or
    /// earlier-connection response never overwrites the current card.
    fn install_runtime_contract(
        &mut self,
        task_id: &str,
        generation: u64,
        result: Result<RuntimeContractView, String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = self.state.read(cx);
        if self.target_device.is_some()
            || state.execution_focus_chat().as_deref() != Some(task_id)
            || state.engine_generation() != generation
        {
            return false;
        }
        self.runtime_generation = generation;
        self.runtime_contract = match result {
            Ok(view) => Loadable::Ready(view),
            Err(error) => Loadable::Error(error),
        };
        cx.notify();
        true
    }

    /// The focused chat's execution summary, only ever from the live
    /// `WatchChats` accessor. The one-shot contract response's snapshot is
    /// never a fallback: after a stream end, reconnect or a frame without
    /// it, an old Running state must not reappear. `None` means its host
    /// reports nothing now; nothing is inferred locally.
    fn execution_summary<'a>(
        &'a self,
        cx: &'a App,
    ) -> Option<(&'a TaskExecutionSummary, &'static str)> {
        let state = self.state.read(cx);
        let focus = state.execution_focus_chat()?;
        state.execution_for(&focus).map(|live| (live, "watch"))
    }

    fn provider_name(&self, harness: HarnessId) -> String {
        self.harnesses
            .ready()
            .and_then(|list| list.iter().find(|descriptor| descriptor.id == harness))
            .map(|descriptor| descriptor.name.clone())
            .unwrap_or_else(|| format!("{harness:?}"))
    }

    /// One settings value per row, as the card and its readback show it.
    fn execution_values(&self, settings: &ExecutionSettings) -> [String; 7] {
        let options = if settings.model_options.is_empty() {
            "Defaults".to_string()
        } else {
            settings
                .model_options
                .iter()
                .map(|(id, value)| match value.as_str() {
                    Some(choice) => format!("{id}: {choice}"),
                    None => format!("{id}: {value}"),
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        [
            self.provider_name(settings.provider),
            if settings.provider_account_id.is_some() {
                "Selected account".into()
            } else {
                "Default".into()
            },
            settings
                .model
                .clone()
                .unwrap_or_else(|| "Provider default".into()),
            settings
                .reasoning
                .map(|level| crate::pickers::reasoning_label(level).to_string())
                .unwrap_or_else(|| "Default".into()),
            options,
            crate::execution_view::permission_title(settings.permission_mode).into(),
            match settings.sandbox {
                SandboxLevel::ReadOnly => "Read only",
                SandboxLevel::WorkspaceWrite => "Workspace write",
                SandboxLevel::DangerFullAccess => "No sandbox",
            }
            .into(),
        ]
    }

    /// The Setting / Running (or Loaded) / Requested rows, with pending
    /// rows marked from the engine's own pending list.
    fn execution_rows(&self, summary: &TaskExecutionSummary) -> Vec<ExecutionRow> {
        const SETTINGS: [(&str, Option<ExecutionSettingField>); 7] = [
            ("Provider", Some(ExecutionSettingField::Provider)),
            ("Account", Some(ExecutionSettingField::Account)),
            ("Model", Some(ExecutionSettingField::Model)),
            ("Reasoning", Some(ExecutionSettingField::Reasoning)),
            ("Options", Some(ExecutionSettingField::ModelOptions)),
            ("Access", Some(ExecutionSettingField::Permission)),
            ("Sandbox", None),
        ];
        let requested = self.execution_values(&summary.requested);
        let running = summary
            .running
            .as_ref()
            .map(|running| self.execution_values(running));
        SETTINGS
            .iter()
            .enumerate()
            .map(|(index, &(setting, field))| ExecutionRow {
                setting,
                running: running.as_ref().map(|values| values[index].clone()),
                requested: requested[index].clone(),
                pending: field.is_some_and(|field| summary.pending.contains(&field)),
            })
            .collect()
    }

    /// What the adapter declares, labelled as declarations (not proof that
    /// this installed CLI, account or session supports it).
    fn execution_declared(summary: &TaskExecutionSummary) -> Option<String> {
        let capabilities = summary.capabilities.as_ref()?;
        let steering = if !capabilities.supports_steering {
            "no steering"
        } else if capabilities.steering == SteeringMode::StepBoundary {
            "steering inside a turn"
        } else {
            "steering between turns"
        };
        let modes: Vec<&str> = [
            PermissionMode::Ask,
            PermissionMode::Auto,
            PermissionMode::Full,
        ]
        .into_iter()
        .filter(|mode| capabilities.permissions.supported_modes.contains(mode))
        .map(crate::execution_view::permission_title)
        .collect();
        Some(format!(
            "Adapter declares (not verified for this session): {steering} · access {} · {}",
            if modes.is_empty() {
                "none".to_string()
            } else {
                modes.join(", ")
            },
            if capabilities.permissions.supports_live_mode_change {
                "live access changes"
            } else {
                "access changes need a new turn or session"
            }
        ))
    }

    fn render_execution_summary(
        &self,
        theme: &Theme,
        summary: &TaskExecutionSummary,
    ) -> gpui::Stateful<gpui::Div> {
        /// The Setting column: wide enough for "Reasoning" and its pending
        /// dot at every Interface size (it is a rem like the text).
        const SETTING_COLUMN: f32 = 96.0;
        let muted = theme.readable_muted(0.85);
        let heading = summary
            .running
            .as_ref()
            .map(|_| crate::execution_view::running_heading(summary))
            .unwrap_or("Running");
        let cell = |text: SharedString, color: gpui::Hsla| {
            div().flex_1().min_w_0().text_color(color).child(text)
        };
        let header = div()
            .flex()
            .flex_row()
            .gap(ui_rems(12.0))
            .pb(ui_rems(4.0))
            .border_b_1()
            .border_color(theme.border)
            .text_size(ui_rems(11.0))
            .line_height(ui_rems(14.0))
            .text_color(muted)
            .child(
                div()
                    .w(ui_rems(SETTING_COLUMN))
                    .flex_none()
                    .child("Setting"),
            )
            .child(cell(heading.into(), muted))
            .child(cell("Requested".into(), muted));
        let rows = self.execution_rows(summary).into_iter().map(|row| {
            div()
                .id(SharedString::from(format!("execution-row-{}", row.setting)))
                .flex()
                .flex_row()
                .gap(ui_rems(12.0))
                .py(ui_rems(3.0))
                .text_size(ui_rems(12.0))
                .line_height(ui_rems(16.0))
                .child(
                    div()
                        .w(ui_rems(SETTING_COLUMN))
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(ui_rems(6.0))
                        .text_color(muted)
                        .child(row.setting)
                        .when(row.pending, |el| {
                            el.child(
                                div()
                                    .flex_none()
                                    .size(ui_rems(6.0))
                                    .rounded_full()
                                    .bg(theme.warning),
                            )
                        }),
                )
                .child(cell(
                    row.running.unwrap_or_else(|| "—".into()).into(),
                    theme.text,
                ))
                .child(cell(
                    row.requested.into(),
                    if row.pending { theme.text } else { muted },
                ))
        });
        let mut lines: Vec<String> = Vec::new();
        if summary.pending.contains(&ExecutionSettingField::Workspace) {
            lines.push("A workspace change is pending.".into());
        }
        if !summary.pending.is_empty() {
            lines.extend(crate::execution_view::transition_lines(&summary.transition));
        }
        lines.push(format!(
            "Steering: {}",
            crate::execution_view::steer_label(&summary.steer)
        ));
        lines.extend(Self::execution_declared(summary));
        div()
            .id("execution-summary")
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(8.0))
            .when_some(
                summary
                    .observation_note
                    .as_deref()
                    .filter(|note| !note.trim().is_empty())
                    .map(str::to_owned),
                |el, note| el.child(widgets::row_description(theme, note)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(header)
                    .child(div().pt(ui_rems(2.0)).flex().flex_col().children(rows)),
            )
            .child(
                div().flex().flex_col().gap(ui_rems(2.0)).children(
                    lines
                        .into_iter()
                        .map(|line| widgets::row_description(theme, line)),
                ),
            )
    }

    fn render_runtime_contract(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let refresh = section_action(
            theme,
            "refresh-effective-session",
            crate::icons::REFRESH,
            "Refresh execution details",
            "Refresh",
            true,
        )
        .on_click(cx.listener(|page, _, _, cx| {
            page.load_runtime_contract(cx);
            cx.notify();
        }));
        let mut card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            "Execution",
            Some(
                "What the focused chat runs with, as its engine reports it. Instruction text, \
                 credentials and provider identifiers are never shown here."
                    .into(),
            ),
            vec![refresh.into_any_element()],
        ));
        let focus = self.state.read(cx).execution_focus_chat();
        let Some(focus) = focus else {
            return card
                .child(note_row(
                    theme,
                    "Select a chat to inspect what it runs with.",
                ))
                .into_any_element();
        };
        // The focused chat by title (never its id), with its live phase.
        let phase = self
            .execution_summary(cx)
            .map(|(summary, _)| crate::execution_view::phase_label(summary.phase));
        card = card.child(
            widgets::card_row(theme, false)
                .child(widgets::row_text(
                    theme,
                    self.chat_title(&focus, cx),
                    Some("Focused chat".into()),
                ))
                .when_some(phase, |row, phase| {
                    row.child(
                        widgets::row_controls()
                            .child(widgets::badge(theme, format!("State: {phase}"))),
                    )
                }),
        );
        #[cfg(feature = "appshots-fixture")]
        self.qa_bounds.borrow_mut().remove("summary");
        let summary_row = match self.execution_summary(cx) {
            Some((summary, _)) => {
                let section = self.render_execution_summary(theme, summary);
                #[cfg(feature = "appshots-fixture")]
                let section = section.relative().child(self.qa_measure("summary"));
                full_row(theme, section)
            }
            None => note_row(
                theme,
                "This chat's host doesn't report execution details. Nothing is inferred from \
                 this device.",
            ),
        };
        card = card.child(summary_row);
        if let Some(line) = self.background_work_line(&focus, cx) {
            card = card.child(widgets::card_row(theme, false).child(widgets::row_text(
                theme,
                "Background work",
                Some(line.into()),
            )));
        }
        if let Some(line) = self.last_transfer_line(&focus, cx) {
            card = card.child(widgets::card_row(theme, false).child(widgets::row_text(
                theme,
                "Last context transfer",
                Some(line.into()),
            )));
        }
        // The runtime contract (instructions and tools), as before.
        let connection_changed = self.runtime_generation != self.state.read(cx).engine_generation();
        let mut contract: Vec<AnyElement> = Vec::new();
        let line = |text: String| widgets::row_description(theme, text).into_any_element();
        match &self.runtime_contract {
            Loadable::Ready(_) if connection_changed => contract.push(line(
                "The engine connection changed. Refreshing session evidence…".into(),
            )),
            Loadable::Ready(view)
                if !matches_selected_contract(
                    view,
                    Some(focus.as_str()),
                    self.target_device.as_deref(),
                ) =>
            {
                contract.push(line(
                    "The focused chat changed. Refreshing session evidence…".into(),
                ))
            }
            Loadable::Ready(view) => {
                if let Some(effective) = &view.effective {
                    contract.push(line(format!(
                        "Contract v{} · {} · {:?} · {}",
                        effective.contract_version,
                        effective.contract_hash,
                        effective.delivery,
                        if effective.restart_required {
                            "restart required"
                        } else {
                            "current at last provider acknowledgment"
                        },
                    )));
                    for instruction in &effective.instructions {
                        contract.push(line(format!(
                            "{} · {} characters · {}",
                            instruction.name, instruction.character_count, instruction.provenance
                        )));
                    }
                    for tool in &effective.attached_tools {
                        contract.push(line(format!(
                            "Tool attached: {}@{}",
                            tool.name, tool.version
                        )));
                    }
                    for tool in &effective.omitted_tools {
                        contract.push(line(format!(
                            "Tool unavailable: {} · {}",
                            tool.name, tool.reason
                        )));
                    }
                } else {
                    contract.push(line(view.unavailable_reason.clone().unwrap_or_else(|| {
                        "No provider session has acknowledged a LUNA contract.".into()
                    })));
                }
            }
            Loadable::Error(error) => contract.push(
                widgets::error_strip(theme, error.clone())
                    .mt_0()
                    .into_any_element(),
            ),
            Loadable::Idle => {}
            Loadable::Loading => contract.push(popover::skeleton_rows(
                "runtime-contract-skeleton",
                theme,
                2,
                cx.entity_id(),
                cx,
            )),
        }
        card = card.child(
            widgets::card_row(theme, false)
                .child(widgets::row_text(
                    theme,
                    "Runtime contract",
                    Some("Technical · last read from the engine, not live".into()),
                ))
                .when(!contract.is_empty(), |row| {
                    row.child(block_list().gap(ui_rems(2.0)).children(contract))
                }),
        );
        #[cfg(feature = "appshots-fixture")]
        let card = card.relative().child(self.qa_measure("card"));
        card.into_any_element()
    }

    /// QA: records `key`'s painted bounds (an absolute overlay that takes no
    /// layout space).
    #[cfg(feature = "appshots-fixture")]
    fn qa_measure(&self, key: &'static str) -> impl IntoElement {
        let sink = self.qa_bounds.clone();
        gpui::canvas(
            move |bounds, _, _| {
                sink.borrow_mut().insert(key, bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// The focused chat's background work summary (its host's aggregate
    /// counts, or why it is unknown), when it has any.
    fn background_work_line(&self, focus: &str, cx: &App) -> Option<String> {
        self.state
            .read(cx)
            .task_work
            .as_ref()
            .filter(|view| view.task_id == focus && view.has_work())
            .map(|view| view.summary())
    }

    /// The focused chat's newest context transfer receipt, in words (the same
    /// line the context tray shows).
    fn last_transfer_line(&self, focus: &str, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        crate::chat_binding::ChatBinding::Chat(focus.to_string())
            .context_control(state)
            .context_transfers
            .iter()
            .max_by_key(|receipt| receipt.updated_at_ms)
            .map(|receipt| crate::context_transfer::receipt_line(state, receipt, focus))
    }

    /// A chat's title for the card (never its id).
    fn chat_title(&self, chat_id: &str, cx: &App) -> String {
        self.state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "Untitled chat".into())
    }

    fn render_integrations(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.skill_busy;
        let check = section_action(
            theme,
            "refresh-integrations",
            crate::icons::REFRESH,
            "Check provider skill and MCP catalog",
            "Check provider",
            !busy,
        )
        .when(!busy, |el| {
            el.on_click(cx.listener(|page, _, _, cx| {
                page.load_inventory(true, cx);
                cx.notify();
            }))
        });
        let mut card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            "Skills and MCP",
            Some(
                "Browse provider-owned skills by global or project scope. Check provider reads \
                 Codex's native catalog, or Claude's or OpenCode's own MCP server readback; file \
                 presence and selected-session attachment are separate checks."
                    .into(),
            ),
            vec![check.into_any_element()],
        ));

        // Provider: whose catalog the rows below show, and where it was read.
        let context: SharedString = match &self.inventory {
            Loadable::Ready(view) => {
                let mut context = format!("Device: {}.", view.machine_id);
                if let Some(task_id) = self.state.read(cx).selected_chat.as_deref() {
                    context.push_str(&format!(
                        " Selected chat: {}. Skill attachment reflects only exact provider \
                         session evidence; unknown means no verified readback.",
                        self.chat_title(task_id, cx)
                    ));
                }
                context.into()
            }
            _ => "Whose skills and MCP servers to show.".into(),
        };
        let mut provider_row = widgets::card_row(theme, false).child(widgets::row_text(
            theme,
            "Provider",
            Some(context),
        ));
        if let Loadable::Ready(harnesses) = &self.harnesses {
            // A segmented control for the provider (audit DS-16).
            let providers: Vec<(HarnessId, SharedString)> = harnesses
                .iter()
                .filter(|harness| {
                    matches!(
                        harness.id,
                        HarnessId::ClaudeCode | HarnessId::Codex | HarnessId::Opencode
                    )
                })
                .map(|harness| (harness.id, SharedString::from(harness.name.clone())))
                .collect();
            let selected = providers
                .iter()
                .position(|(id, _)| *id == self.integration_provider)
                .unwrap_or(0);
            let ids: Vec<HarnessId> = providers.iter().map(|(id, _)| *id).collect();
            if !providers.is_empty() {
                provider_row = provider_row.child(
                    widgets::row_controls().child(button::segmented(
                        "integration-provider",
                        providers
                            .iter()
                            .map(|(id, name)| Segment::new(format!("{id:?}"), name.clone()))
                            .collect(),
                        selected,
                        104.0 * providers.len() as f32,
                        ControlSize::Md,
                        theme,
                        cx.listener(move |page, ix: &usize, _, cx| {
                            let Some(id) = ids.get(*ix).copied() else {
                                return;
                            };
                            if page.skill_busy || id == page.integration_provider {
                                return;
                            }
                            page.integration_provider = id;
                            page.clear_skill_editor();
                            page.load_inventory(false, cx);
                            cx.notify();
                        }),
                    )),
                );
            }
        }
        card = card.child(provider_row);

        let has_project = self.selected_skill_project_id(cx).is_some();
        card = card.child(
            widgets::card_row(theme, false)
                .child(widgets::row_text(
                    theme,
                    "New skill",
                    Some(
                        "Create a SKILL.md in the provider's global folder, or in the selected \
                         project's folder."
                            .into(),
                    ),
                ))
                .child(
                    widgets::row_controls()
                        .when(has_project, |controls| {
                            controls.child(
                                kit_button(
                                    theme,
                                    "create-project-skill",
                                    ButtonKind::Secondary,
                                    ControlSize::Md,
                                    "New project skill",
                                    "New project skill",
                                    !busy,
                                )
                                .when(!busy, |el| {
                                    el.on_click(cx.listener(|page, _, _, cx| {
                                        page.create_skill(IntegrationSkillScope::Project, cx)
                                    }))
                                }),
                            )
                        })
                        .child(
                            kit_button(
                                theme,
                                "create-global-skill",
                                ButtonKind::Secondary,
                                ControlSize::Md,
                                "New global skill",
                                "New global skill",
                                !busy,
                            )
                            .when(!busy, |el| {
                                el.on_click(cx.listener(|page, _, _, cx| {
                                    page.create_skill(IntegrationSkillScope::Global, cx)
                                }))
                            }),
                        ),
                ),
        );

        let mut footer = None;
        match &self.inventory {
            Loadable::Ready(view) => {
                if let Some(reason) = &view.unavailable_reason {
                    card = card.child(strip_row(
                        theme,
                        widgets::warning_strip(theme, reason.clone()),
                    ));
                } else if view.items.is_empty() {
                    card = card.child(note_row(
                        theme,
                        format!(
                            "No skills or MCP servers found for {}.",
                            self.provider_name(self.integration_provider)
                        ),
                    ));
                }
                for item in &view.items {
                    card = card.child(self.render_inventory_item(theme, item, cx));
                }
                if let Some(panel) = self.render_mcp_panel(theme) {
                    card = card.child(panel);
                }
                footer = Some(view.help.clone());
            }
            Loadable::Error(error) => {
                card = card.child(strip_row(theme, widgets::error_strip(theme, error.clone())))
            }
            _ => {
                card = card.child(skeleton_row(
                    theme,
                    "integrations-skeleton",
                    3,
                    cx.entity_id(),
                    cx,
                ))
            }
        }
        div()
            .flex()
            .flex_col()
            .child(card)
            .when_some(footer, |el, help| {
                el.child(widgets::section_footer(theme, help))
            })
            .into_any_element()
    }

    /// One skill or MCP server: identity and state at the left, its actions
    /// trailing, provider facts (MCP badges, errors, app views) underneath.
    fn render_inventory_item(
        &self,
        theme: &Theme,
        item: &luna_proto::IntegrationInventoryItem,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
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
        let text = widgets::row_text(
            theme,
            item.name.clone(),
            Some(
                format!(
                    "{kind} · {} · {} · configured · {available} · {attached}",
                    item.source, item.scope
                )
                .into(),
            ),
        )
        .when_some(item.reason.clone(), |el, reason| {
            el.child(widgets::row_description(theme, reason))
        })
        .when_some(item.actions.reason.clone(), |el, reason| {
            el.child(widgets::row_description(theme, reason))
        });
        let (facts, mut actions) = match item.mcp.as_ref() {
            Some(details) => self.mcp_row_parts(theme, item, details, cx),
            None => (Vec::new(), Vec::new()),
        };
        if item.kind == IntegrationKind::Skill
            && let Some(identity) = item.identity.clone()
        {
            let enabled = !self.skill_busy;
            actions.push(
                kit_button(
                    theme,
                    SharedString::from(format!("inspect-skill-{identity}")),
                    ButtonKind::Secondary,
                    ControlSize::Sm,
                    format!("Inspect {} {} skill", item.scope, item.name),
                    "Inspect / edit",
                    enabled,
                )
                .when(enabled, |button| {
                    button.on_click(
                        cx.listener(move |page, _, _, cx| page.open_skill(identity.clone(), cx)),
                    )
                })
                .into_any_element(),
            );
        }
        widgets::card_row(theme, false)
            .child(text)
            .when(!actions.is_empty(), |row| {
                row.child(widgets::row_controls().children(actions))
            })
            .when(!facts.is_empty(), |row| {
                row.child(block_list().children(facts))
            })
    }

    fn render_skill_editor(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.skill_busy;
        let title: SharedString = if self.skill_creating {
            if self.skill_scope == IntegrationSkillScope::Global {
                "New global skill".into()
            } else {
                "New project skill".into()
            }
        } else if let Loadable::Ready(document) = &self.skill_document {
            document.name.clone().into()
        } else {
            "Skill details".into()
        };
        let close = section_action(
            theme,
            "close-skill-editor",
            crate::icons::CLOSE,
            "Close skill details",
            "Close",
            !busy,
        )
        .when(!busy, |el| {
            el.on_click(cx.listener(|page, _, _, cx| {
                page.clear_skill_editor();
                cx.notify();
            }))
        });
        let mut card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            title,
            Some(
                "LUNA edits the provider's SKILL.md in its selected global or project folder. \
                 Existing sessions may need a fresh turn to discover a change."
                    .into(),
            ),
            vec![close.into_any_element()],
        ));
        if let Some(error) = &self.skill_error {
            card = card.child(strip_row(theme, widgets::error_strip(theme, error.clone())));
        }
        if let Some(notice) = &self.skill_notice {
            card = card.child(
                note_row(theme, notice.clone())
                    .id("skill-notice")
                    .role(gpui::Role::Status),
            );
        }
        if self.skill_creating {
            card = card.child(
                widgets::card_row(theme, false)
                    .child(widgets::row_text(
                        theme,
                        "Name",
                        Some(
                            "Use a lowercase name with digits or single hyphens. Edit the \
                             description and instructions before saving."
                                .into(),
                        ),
                    ))
                    .child(widgets::row_block().child(editor_frame(
                        theme,
                        self.skill_name_input.clone().into_any_element(),
                    ))),
            );
        }
        match &self.skill_document {
            Loadable::Loading => {
                return card
                    .child(skeleton_row(theme, "skill-skeleton", 3, cx.entity_id(), cx))
                    .into_any_element();
            }
            Loadable::Error(error) => {
                return card
                    .child(strip_row(theme, widgets::error_strip(theme, error.clone())))
                    .into_any_element();
            }
            Loadable::Ready(document) => {
                let scope = if document.scope == IntegrationSkillScope::Global {
                    "Global"
                } else {
                    "Project"
                };
                let mut text = widgets::row_text(
                    theme,
                    "Location",
                    Some(
                        format!(
                            "{scope} · {} · revision {}",
                            self.provider_name(document.provider),
                            short_revision(&document.revision, 12)
                        )
                        .into(),
                    ),
                );
                if !document.supporting_files.is_empty() {
                    let names = document
                        .supporting_files
                        .iter()
                        .take(12)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ");
                    let more = document.supporting_files.len().saturating_sub(12);
                    text = text.child(widgets::row_description(
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
                card = card.child(widgets::card_row(theme, false).child(text).when_some(
                    document.read_only_reason.clone(),
                    |row, reason| {
                        row.child(
                            widgets::row_block()
                                .child(widgets::warning_strip(theme, reason).mt_0().w_full()),
                        )
                    },
                ));
            }
            Loadable::Idle if !self.skill_creating => return card.into_any_element(),
            Loadable::Idle => {}
        }
        let save_enabled = !busy
            && (self.skill_creating
                || matches!(&self.skill_document, Loadable::Ready(document) if document.read_only_reason.is_none()));
        let save_label = if self.skill_creating {
            "Create skill"
        } else {
            "Save skill"
        };
        card = card.child(
            widgets::card_row(theme, false)
                .child(widgets::row_text(theme, "SKILL.md", None))
                .child(
                    widgets::row_controls().child(
                        kit_button(
                            theme,
                            "save-skill-document",
                            ButtonKind::Primary,
                            ControlSize::Md,
                            save_label,
                            if busy {
                                "Saving or installing…"
                            } else {
                                save_label
                            },
                            save_enabled,
                        )
                        .when(save_enabled, |el| {
                            el.on_click(cx.listener(|page, _, _, cx| page.save_skill(cx)))
                        }),
                    ),
                )
                .child(widgets::row_block().child(editor_frame(
                    theme,
                    self.skill_content_input.clone().into_any_element(),
                ))),
        );

        if let Loadable::Ready(document) = &self.skill_document {
            let writable = document.read_only_reason.is_none();
            // Destination provider and scope: fixed short choices, so
            // segmented controls (spec §5).
            let providers = [
                (HarnessId::Codex, "Codex"),
                (HarnessId::ClaudeCode, "Claude Code"),
                (HarnessId::Opencode, "OpenCode"),
            ];
            let provider_ix = providers
                .iter()
                .position(|(provider, _)| *provider == self.skill_install_provider)
                .unwrap_or(0);
            let mut scopes = vec![(IntegrationSkillScope::Global, "Global")];
            if self.selected_skill_project_id(cx).is_some() {
                scopes.push((IntegrationSkillScope::Project, "Current project"));
            }
            let scope_ix = scopes
                .iter()
                .position(|(scope, _)| *scope == self.skill_install_scope)
                .unwrap_or(0);
            let install_valid = self.skill_install_provider != document.provider
                || self.skill_install_scope != document.scope;
            let install_enabled = install_valid && !busy && writable;
            let scope_choices: Vec<IntegrationSkillScope> =
                scopes.iter().map(|(scope, _)| *scope).collect();
            card = card.child(
                widgets::card_row(theme, false)
                    .child(widgets::row_text(
                        theme,
                        "Install a copy",
                        Some(
                            "Copy this skill and its supporting files to another provider or \
                             scope. An existing skill with the same name is never replaced."
                                .into(),
                        ),
                    ))
                    .child(
                        widgets::row_controls().child(
                            kit_button(
                                theme,
                                "install-skill-copy",
                                ButtonKind::Secondary,
                                ControlSize::Md,
                                format!("Install copy of {}", document.name),
                                "Install copy",
                                install_enabled,
                            )
                            .when(install_enabled, |el| {
                                el.on_click(cx.listener(|page, _, _, cx| page.install_skill(cx)))
                            }),
                        ),
                    )
                    .child(
                        widgets::row_block()
                            .child(button::segmented(
                                "skill-install-provider",
                                providers
                                    .iter()
                                    .map(|(provider, label)| {
                                        Segment::new(format!("{provider:?}"), *label)
                                    })
                                    .collect(),
                                provider_ix,
                                312.0,
                                ControlSize::Md,
                                theme,
                                cx.listener(move |page, ix: &usize, _, cx| {
                                    if page.skill_busy {
                                        return;
                                    }
                                    if let Some((provider, _)) = providers.get(*ix) {
                                        page.skill_install_provider = *provider;
                                        cx.notify();
                                    }
                                }),
                            ))
                            .child(button::segmented(
                                "skill-install-scope",
                                scopes
                                    .iter()
                                    .map(|(scope, label)| {
                                        Segment::new(format!("{scope:?}"), *label)
                                    })
                                    .collect(),
                                scope_ix,
                                128.0 * scopes.len() as f32,
                                ControlSize::Md,
                                theme,
                                cx.listener(move |page, ix: &usize, _, cx| {
                                    if page.skill_busy {
                                        return;
                                    }
                                    if let Some(scope) = scope_choices.get(*ix) {
                                        page.skill_install_scope = *scope;
                                        cx.notify();
                                    }
                                }),
                            )),
                    ),
            );
            let remove_enabled = !busy && writable;
            let armed = self.skill_remove_armed;
            card = card.child(
                widgets::card_row(theme, false)
                    .child(widgets::row_text(
                        theme,
                        "Remove skill",
                        Some(
                            format!(
                                "Removing deletes SKILL.md and {} supporting entries from this {} \
                                 folder.",
                                document.supporting_files.len(),
                                if document.scope == IntegrationSkillScope::Global {
                                    "global"
                                } else {
                                    "project"
                                }
                            )
                            .into(),
                        ),
                    ))
                    .child(
                        widgets::row_controls().child(
                            // Secondary until armed; the confirm is the
                            // only destructive-looking control (spec §5).
                            kit_button(
                                theme,
                                "remove-skill",
                                if armed {
                                    ButtonKind::Danger
                                } else {
                                    ButtonKind::Secondary
                                },
                                ControlSize::Md,
                                if armed {
                                    "Confirm remove skill and files"
                                } else {
                                    "Remove skill"
                                },
                                if armed {
                                    "Confirm remove skill and files"
                                } else {
                                    "Remove…"
                                },
                                remove_enabled,
                            )
                            .when(remove_enabled, |el| {
                                el.on_click(cx.listener(|page, _, _, cx| page.remove_skill(cx)))
                            }),
                        ),
                    ),
            );
        }
        card.into_any_element()
    }

    /// One instructions editor: its title and scope copy, the Save action
    /// trailing, the editor underneath.
    #[allow(clippy::too_many_arguments)]
    fn instruction_row(
        &self,
        theme: &Theme,
        title: &'static str,
        description: SharedString,
        input: &Entity<ComposerInput>,
        id: &'static str,
        aria: &'static str,
        project: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let enabled = !self.instruction_saving;
        widgets::card_row(theme, false)
            .child(widgets::row_text(theme, title, Some(description)))
            .child(
                widgets::row_controls().child(
                    kit_button(
                        theme,
                        id,
                        ButtonKind::Secondary,
                        ControlSize::Md,
                        aria,
                        if enabled { "Save" } else { "Saving…" },
                        enabled,
                    )
                    .when(enabled, |el| {
                        el.on_click(
                            cx.listener(move |page, _, _, cx| page.save_instructions(project, cx)),
                        )
                    }),
                ),
            )
            .child(
                widgets::row_block().child(editor_frame(theme, input.clone().into_any_element())),
            )
    }

    fn render_instructions(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let reload = section_action(
            theme,
            "reload-luna-instructions",
            crate::icons::REFRESH,
            "Reload LUNA instructions",
            "Reload",
            true,
        )
        .on_click(cx.listener(|page, _, _, cx| {
            page.load_instructions(cx);
            cx.notify();
        }));
        let mut card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            "LUNA instructions",
            Some(
                "Global instructions precede project instructions. Provider-native files remain \
                 with their provider. Changes apply at the next safe turn or a new provider \
                 session."
                    .into(),
            ),
            vec![reload.into_any_element()],
        ));
        if let Some(error) = &self.instruction_error {
            card = card.child(strip_row(theme, widgets::error_strip(theme, error.clone())));
        }
        match &self.instructions {
            Loadable::Ready(view) => {
                card = card.child(self.instruction_row(
                    theme,
                    "Global",
                    "Every LUNA task, in every project.".into(),
                    &self.global_input,
                    "save-global-instructions",
                    "Save global instructions",
                    false,
                    cx,
                ));
                if view.project_id.is_some() {
                    let project = self
                        .state
                        .read(cx)
                        .selected_space_row()
                        .map(|space| space.display_name().to_owned());
                    card = card.child(self.instruction_row(
                        theme,
                        "Current project",
                        match project {
                            Some(name) => {
                                format!("Tasks in {name}, after the global instructions.").into()
                            }
                            None => "Tasks in this project, after the global instructions.".into(),
                        },
                        &self.project_input,
                        "save-project-instructions",
                        "Save project instructions",
                        true,
                        cx,
                    ));
                } else {
                    card = card.child(note_row(
                        theme,
                        "Select a project to edit its LUNA instructions.",
                    ));
                }
            }
            Loadable::Error(error) => {
                card = card.child(strip_row(theme, widgets::error_strip(theme, error.clone())))
            }
            _ => {
                card = card.child(skeleton_row(
                    theme,
                    "instructions-skeleton",
                    2,
                    cx.entity_id(),
                    cx,
                ))
            }
        }
        card.into_any_element()
    }

    fn render_titles(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut card = widgets::section_card(theme).child(widgets::section_header(
            theme,
            "Session titles",
            Some(
                "Choose the agent and model for automatic titles on this device. Claude Code and \
                 Codex support restricted title generation."
                    .into(),
            ),
            Vec::new(),
        ));
        if let Some(error) = &self.title_error {
            card = card.child(strip_row(theme, widgets::error_strip(theme, error.clone())));
        }
        let settings = match &self.title_settings {
            Loadable::Ready(settings) => settings,
            Loadable::Error(error) => {
                return card
                    .child(strip_row(theme, widgets::error_strip(theme, error.clone())))
                    .into_any_element();
            }
            _ => {
                return card
                    .child(skeleton_row(theme, "title-skeleton", 2, cx.entity_id(), cx))
                    .into_any_element();
            }
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
            let row_title = if is_model {
                "Title model"
            } else {
                "Title agent"
            };
            let description = if is_model {
                if settings.harness.is_some() {
                    "The model the title agent uses."
                } else {
                    "Choose a title agent to pick its model."
                }
            } else {
                "Writes the automatic title for each new chat."
            };
            let title_aria = format!("{row_title}: {label}");
            let open = self.title_menu == Some(is_model);
            let mut row = widgets::card_row(theme, false)
                .child(widgets::row_text(
                    theme,
                    row_title,
                    Some(description.into()),
                ))
                .child(
                    widgets::row_controls().child(
                        button::select(
                            if is_model {
                                "title-model"
                            } else {
                                "title-harness"
                            },
                            label,
                            ControlSize::Md,
                            open,
                            interactive,
                            theme,
                        )
                        .min_w(ui_rems(200.0))
                        .max_w(ui_rems(280.0))
                        .aria_label(title_aria)
                        .when(interactive, |el| {
                            // The inline list has no key handling of its own:
                            // keep Enter/Space from reaching ancestors, as
                            // GPUI's keyboard click already toggles it.
                            el.on_key_down(crate::swallow_activation_key)
                                .on_click(cx.listener(move |page, _, _, cx| {
                                    page.title_menu = if page.title_menu == Some(is_model) {
                                        None
                                    } else {
                                        Some(is_model)
                                    };
                                    cx.notify();
                                }))
                        }),
                    ),
                );
            if open {
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
                // The open choice list sits under the row, in a well.
                row = row.child(
                    widgets::row_block().child(
                        div()
                            .id(if is_model {
                                "title-model-options"
                            } else {
                                "title-harness-options"
                            })
                            .role(gpui::Role::List)
                            .w_full()
                            .max_h(ui_rems(240.0))
                            .overflow_y_scroll()
                            .p(ui_rems(4.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(theme.separator(false))
                            .bg(theme.fill(FillTier::Quaternary))
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
                    ),
                );
            }
            card = card.child(row);
        }
        if let Loadable::Error(error) = &self.title_models {
            card = card.child(strip_row(theme, widgets::error_strip(theme, error.clone())));
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
            self.model_refresh_result =
                Some((false, "No enabled agents on the selected device.".into()));
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
                let clean = failures.is_empty() && account_warning.is_none();
                if let Some(warning) = account_warning {
                    message.push_str(&format!(" Codex account listing failed: {warning}"));
                }
                page.model_refresh_result = Some((clean, message));
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
        let running = self.model_refresh_running;
        let refresh = section_action(
            theme,
            "refresh-model-cache",
            crate::icons::REFRESH,
            "Refresh model cache",
            if running {
                "Refreshing…"
            } else {
                "Refresh models"
            },
            !running,
        )
        .when(!running, |el| {
            el.on_click(cx.listener(|page, _, _, cx| page.refresh_model_cache(cx)))
        });
        widgets::section_card(theme)
            .child(widgets::section_header(
                theme,
                "Models",
                Some(
                    "LUNA saves model lists on each device and refreshes them in the background \
                     after launch. Refresh now to check the selected device for new models \
                     without restarting."
                        .into(),
                ),
                vec![refresh.into_any_element()],
            ))
            .when(running, |card| {
                card.child(skeleton_row(
                    theme,
                    "model-refresh-skeleton",
                    1,
                    cx.entity_id(),
                    cx,
                ))
            })
            .when_some(
                self.model_refresh_result.clone(),
                |card, (clean, message)| {
                    card.child(
                        if clean {
                            note_row(theme, message)
                        } else {
                            strip_row(theme, widgets::warning_strip(theme, message))
                        }
                        .id("model-refresh-result")
                        .role(gpui::Role::Status),
                    )
                },
            )
            .into_any_element()
    }

    /// The advanced sections, grouped by topic: models and titles (the only
    /// cards for a remote device), then skills and MCP with the open skill,
    /// instructions, execution, and the Claude-specific cards. Each card
    /// carries its own top gap ([`widgets::SECTION_GAP`]).
    pub(crate) fn render_advanced_sections(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let local = self.target_device.is_none();
        let skill_open = self.skill_creating
            || !matches!(&self.skill_document, Loadable::Idle)
            || self.skill_notice.is_some();
        div()
            .w_full()
            .flex()
            .flex_col()
            .child(self.render_model_cache(&theme, cx))
            .child(self.render_titles(&theme, cx))
            .when(local, |el| {
                el.child(self.render_integrations(&theme, cx))
                    .when(skill_open, |el| {
                        el.child(self.render_skill_editor(&theme, cx))
                    })
                    .child(self.render_instructions(&theme, cx))
                    .child(self.render_runtime_contract(&theme, cx))
                    .child(self.native_extensions.clone())
                    .child(self.render_claude_advanced(&theme, cx))
            })
            .into_any_element()
    }
}

impl Render for HarnessesPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_advanced_sections(cx)
    }
}

/// Headless QA: the Execution card as rendered.
#[cfg(feature = "appshots-fixture")]
impl HarnessesPage {
    /// The Execution card's and its summary's last painted bounds.
    pub fn fixture_execution_bounds(
        &self,
    ) -> std::collections::HashMap<&'static str, gpui::Bounds<gpui::Pixels>> {
        self.qa_bounds.borrow().clone()
    }

    pub fn fixture_execution_readback(&self, cx: &App) -> serde_json::Value {
        let focus = self.state.read(cx).execution_focus_chat();
        let summary = self.execution_summary(cx);
        serde_json::json!({
            "focus": focus,
            "chatTitle": focus.as_deref().map(|id| self.chat_title(id, cx)),
            "contractTask": match &self.runtime_contract {
                Loadable::Ready(view) => Some(view.task_id.clone()),
                _ => None,
            },
            "contractGeneration": self.runtime_generation,
            // What the one-shot contract carried (never rendered as live).
            "contractExecutionPhase": match &self.runtime_contract {
                Loadable::Ready(view) => view
                    .execution
                    .as_ref()
                    .and_then(|summary| serde_json::to_value(summary.phase).ok()),
                _ => None,
            },
            "contractState": match &self.runtime_contract {
                Loadable::Idle => "idle",
                Loadable::Loading => "loading",
                Loadable::Ready(_) => "ready",
                Loadable::Error(_) => "error",
            },
            "source": summary.map(|(_, source)| source),
            "phase": summary.map(|(summary, _)| crate::execution_view::phase_label(summary.phase)),
            "runningHeading": summary.and_then(|(summary, _)| {
                summary.running.as_ref().map(|_| crate::execution_view::running_heading(summary))
            }),
            "rows": summary.map(|(summary, _)| {
                self.execution_rows(summary)
                    .into_iter()
                    .map(|row| serde_json::json!({
                        "setting": row.setting,
                        "running": row.running,
                        "requested": row.requested,
                        "pending": row.pending,
                    }))
                    .collect::<Vec<_>>()
            }),
            "applies": summary
                .filter(|(summary, _)| !summary.pending.is_empty())
                .map(|(summary, _)| crate::execution_view::transition_lines(&summary.transition)),
            "steering": summary.map(|(summary, _)| crate::execution_view::steer_label(&summary.steer)),
            "declared": summary.and_then(|(summary, _)| Self::execution_declared(summary)),
            "note": summary.and_then(|(summary, _)| summary.observation_note.clone()),
            "lastTransfer": focus.as_deref().and_then(|id| self.last_transfer_line(id, cx)),
            "backgroundWork": focus.as_deref().and_then(|id| self.background_work_line(id, cx)),
        })
    }
    /// Install a contract response as if it arrived for `task_id` (a late
    /// response for a chat that may no longer be focused), through the same
    /// guard the RPC completion uses.
    pub fn fixture_deliver_contract(
        &mut self,
        task_id: &str,
        generation: u64,
        view: serde_json::Value,
        cx: &mut Context<Self>,
    ) -> bool {
        let Ok(view) = serde_json::from_value::<RuntimeContractView>(view) else {
            return false;
        };
        self.install_runtime_contract(task_id, generation, Ok(view), cx)
    }
}
