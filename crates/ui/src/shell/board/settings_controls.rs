//! Implementation/review settings controls (A9): harness/account/model,
//! supported reasoning/options, access, checkout and the reviewer profile,
//! fed by the host's real catalog (`BoardApi::execution_options`).
//!
//! Rules (ticket-authoring §3, proposal "Execution settings"):
//! * Every choice comes from the catalog; nothing here invents an id.
//! * Inherited values show their effective value and origin, never a bare
//!   "default".
//! * Only reasoning levels and options the selected model supports are
//!   offered; a harness or model change resets incompatible fields with a
//!   visible notice.
//! * Account and host controls appear only with more than one valid choice.
//! * "Use project defaults" and "Decide later" (an explicit unresolved
//!   state) are valid choices.
//! * The engine validates the full combination on save; hiding a control is
//!   never the enforcement.
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, IntoElement, ParentElement, Render,
    SharedString, Styled, Task, Window, div, prelude::*,
};
use luna_proto::{
    BoardCheckoutChoice, BoardExecutionOptions, BoardExecutionOptionsRequest,
    BoardExecutionProfile, BoardHarnessOptions, BoardReviewProfile, BoardSettingOrigin, HarnessId,
    Model, PermissionMode, ProjectCheckout, ReasoningLevel, ResolvedExecution,
};

use super::api::BoardCallError;
use super::detail::BoardProbe;
use super::model::BoardModel;
use crate::button::{self, ButtonKind, ControlSize};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;
use crate::typography::ui_rems;

/// Shown wherever the review profile inherits and no reviewer is configured.
pub(crate) const REVIEW_FALLBACK_TEXT: &str =
    "No reviewer is configured: the implementation model reviews in a separate reviewer chat.";

/// Catalog models listed at once per harness; "Show all" lists the rest.
const MODEL_CHIPS: usize = 12;

/// The current choices in a settings editor.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExecutionSettingsDraft {
    pub implementation: BoardExecutionProfile,
    pub review: BoardReviewProfile,
}

/// How the implementation profile is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImplementationMode {
    /// Inherit everything (shown with the effective values).
    ProjectDefaults,
    /// Explicit fields; unset fields still inherit.
    Custom,
    /// Deliberately unresolved: Ready must resolve it before dispatch.
    Deferred,
}

impl ImplementationMode {
    pub fn of(profile: &BoardExecutionProfile) -> Self {
        if profile.deferred {
            Self::Deferred
        } else if profile.is_inherited() {
            Self::ProjectDefaults
        } else {
            Self::Custom
        }
    }

    #[cfg(feature = "appshots-fixture")]
    #[cfg(feature = "appshots-fixture")]
    pub fn key(self) -> &'static str {
        match self {
            Self::ProjectDefaults => "defaults",
            Self::Custom => "custom",
            Self::Deferred => "deferred",
        }
    }
}

/// How the review profile is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    ProjectDefault,
    Custom,
}

impl ReviewMode {
    pub fn of(profile: &BoardReviewProfile) -> Self {
        if profile == &BoardReviewProfile::default() {
            Self::ProjectDefault
        } else {
            Self::Custom
        }
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn key(self) -> &'static str {
        match self {
            Self::ProjectDefault => "default",
            Self::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Implementation,
    Review,
}

impl Which {
    fn key(self) -> &'static str {
        match self {
            Self::Implementation => "impl",
            Self::Review => "review",
        }
    }
}

/// The host's catalog as the editor knows it.
pub(crate) enum OptionsState {
    Loading,
    Ready(Box<BoardExecutionOptions>),
    Error(String),
}

pub enum SettingsEditorEvent {
    Changed,
}

// ---------------------------------------------------------------------------
// Labels shared by the form, the drawer and receipts
// ---------------------------------------------------------------------------

pub(crate) fn harness_label(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::ClaudeCode => "Claude Code",
        HarnessId::Codex => "Codex",
        HarnessId::Opencode => "OpenCode",
        HarnessId::Mock => "Mock",
        HarnessId::Cursor => "Cursor",
        HarnessId::Devin => "Devin",
        HarnessId::Grok => "Grok",
        HarnessId::Hermes => "Hermes",
        HarnessId::Pi => "Pi",
        HarnessId::Antigravity => "Antigravity",
    }
}

/// The wire spelling (`claude-code`), used in element ids.
pub(crate) fn harness_key(harness: HarnessId) -> String {
    serde_json::to_value(harness)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{harness:?}"))
}

pub(crate) fn reasoning_key(level: ReasoningLevel) -> String {
    serde_json::to_value(level)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{level:?}"))
}

pub(crate) fn permission_key(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "ask",
        PermissionMode::Auto => "auto",
        PermissionMode::Full => "full",
    }
}

pub(crate) fn reasoning_label(level: ReasoningLevel) -> &'static str {
    crate::pickers::reasoning_label(level)
}

pub(crate) fn permission_label(mode: PermissionMode) -> &'static str {
    crate::execution_view::permission_title(mode)
}

pub(crate) fn origin_label(origin: BoardSettingOrigin) -> &'static str {
    match origin {
        BoardSettingOrigin::StartOverride => "Start override",
        BoardSettingOrigin::Ticket => "ticket",
        BoardSettingOrigin::Parent => "parent chat",
        BoardSettingOrigin::Project => "project",
        BoardSettingOrigin::ProviderDefault => "provider default",
        BoardSettingOrigin::CatalogDefault => "catalog default",
        BoardSettingOrigin::ImplementationFallback => "implementation model",
    }
}

fn source(resolved: &ResolvedExecution, field: &str) -> Option<BoardSettingOrigin> {
    resolved
        .sources
        .iter()
        .find(|source| source.field == field)
        .map(|source| source.source)
}

fn harness_options(
    options: &BoardExecutionOptions,
    harness: HarnessId,
) -> Option<&BoardHarnessOptions> {
    options
        .harnesses
        .iter()
        .find(|entry| entry.harness == harness)
}

fn catalog_model<'a>(
    options: &'a BoardExecutionOptions,
    harness: HarnessId,
    id: &str,
) -> Option<&'a Model> {
    harness_options(options, harness)?
        .models
        .iter()
        .find(|model| model.id == id)
}

/// A model's display label from the catalog, else its id.
pub(crate) fn model_label(
    options: Option<&BoardExecutionOptions>,
    harness: HarnessId,
    id: &str,
) -> String {
    options
        .and_then(|options| catalog_model(options, harness, id))
        .map(|model| model.label.clone())
        .unwrap_or_else(|| id.to_owned())
}

/// No reviewer is configured anywhere: review uses the implementation model.
pub(crate) fn review_falls_back(options: &BoardExecutionOptions) -> bool {
    match &options.effective_review {
        None => true,
        Some(review) => review
            .sources
            .iter()
            .any(|source| source.source == BoardSettingOrigin::ImplementationFallback),
    }
}

fn checkout_label(checkout: &BoardCheckoutChoice) -> &'static str {
    match checkout {
        BoardCheckoutChoice::Current => "Current checkout",
        BoardCheckoutChoice::NewWorktree { .. } => "New worktree",
    }
}

fn project_checkout_label(options: &BoardExecutionOptions) -> &'static str {
    match options.project_defaults.checkout {
        Some(ProjectCheckout::NewWorktree) => "New worktree",
        Some(ProjectCheckout::Local) | None => "Current checkout",
    }
}

/// "Codex · GPT-6.1 Sol · High · Approve for me" for frozen/effective
/// settings, with model labels from the catalog when known.
pub(crate) fn describe_resolved(
    resolved: &ResolvedExecution,
    options: Option<&BoardExecutionOptions>,
    with_access: bool,
) -> String {
    let mut parts = vec![harness_label(resolved.harness).to_owned()];
    match &resolved.model {
        Some(model) => parts.push(model_label(options, resolved.harness, model)),
        None => parts.push("provider default model".into()),
    }
    match resolved.reasoning {
        Some(level) => parts.push(reasoning_label(level).to_owned()),
        None => parts.push("model default reasoning".into()),
    }
    if with_access {
        parts.push(permission_label(resolved.permission_mode).to_owned());
    }
    parts.join(" · ")
}

/// A saved implementation profile in words (requested values only; inherited
/// fields name what they inherit).
pub(crate) fn describe_implementation(
    profile: &BoardExecutionProfile,
    options: Option<&BoardExecutionOptions>,
) -> String {
    if profile.deferred {
        return "Decide later: unresolved until it is resolved before Ready".into();
    }
    if profile.is_inherited() {
        return match options.and_then(|options| options.effective_implementation.as_ref()) {
            Some(resolved) => format!(
                "Project defaults · {}",
                describe_resolved(resolved, options, true)
            ),
            None => "Project defaults".into(),
        };
    }
    let mut parts = Vec::new();
    match profile.harness {
        Some(harness) => parts.push(harness_label(harness).to_owned()),
        None => parts.push("inherited harness".into()),
    }
    if let Some(account) = &profile.provider_account_id {
        parts.push(format!("account {account}"));
    }
    match (&profile.model, profile.harness) {
        (Some(model), Some(harness)) => parts.push(model_label(options, harness, model)),
        (Some(model), None) => parts.push(model.clone()),
        (None, _) => parts.push("inherited model".into()),
    }
    match profile.reasoning {
        Some(level) => parts.push(reasoning_label(level).to_owned()),
        None => parts.push("inherited reasoning".into()),
    }
    for (option, choice) in &profile.model_options {
        parts.push(format!("{option}: {}", choice.as_str().unwrap_or("?")));
    }
    match profile.permission_mode {
        Some(mode) => parts.push(permission_label(mode).to_owned()),
        None => parts.push("inherited access".into()),
    }
    if let Some(checkout) = &profile.checkout {
        parts.push(checkout_label(checkout).to_owned());
    }
    parts.join(" · ")
}

/// A saved review profile in words.
pub(crate) fn describe_review(
    profile: &BoardReviewProfile,
    options: Option<&BoardExecutionOptions>,
) -> String {
    if profile.disabled {
        return "Automatic review skipped (manual acceptance)".into();
    }
    if profile == &BoardReviewProfile::default() {
        return match options {
            Some(options) if review_falls_back(options) => {
                format!("Project default · {REVIEW_FALLBACK_TEXT}")
            }
            Some(options) => match &options.effective_review {
                Some(review) => format!(
                    "Project default · {}",
                    describe_resolved(review, Some(options), false)
                ),
                None => "Project default".into(),
            },
            None => "Project default".into(),
        };
    }
    let mut parts = Vec::new();
    match profile.harness {
        Some(harness) => parts.push(harness_label(harness).to_owned()),
        None => parts.push("inherited harness".into()),
    }
    match (&profile.model, profile.harness) {
        (Some(model), Some(harness)) => parts.push(model_label(options, harness, model)),
        (Some(model), None) => parts.push(model.clone()),
        (None, _) => parts.push("inherited model".into()),
    }
    match profile.reasoning {
        Some(level) => parts.push(reasoning_label(level).to_owned()),
        None => parts.push("inherited reasoning".into()),
    }
    parts.join(" · ")
}

// ---------------------------------------------------------------------------
// The editor
// ---------------------------------------------------------------------------

/// Mutable view of the fields both profiles share.
struct Fields<'a> {
    harness: &'a mut Option<HarnessId>,
    account: &'a mut Option<String>,
    model: &'a mut Option<String>,
    reasoning: &'a mut Option<ReasoningLevel>,
    options: &'a mut serde_json::Map<String, serde_json::Value>,
}

/// Read-only copy of the shared fields.
#[derive(Clone)]
struct FieldValues {
    harness: Option<HarnessId>,
    account: Option<String>,
    model: Option<String>,
    reasoning: Option<ReasoningLevel>,
    options: serde_json::Map<String, serde_json::Value>,
}

/// The settings editor shared by Write manually and the detail drawer.
pub struct ExecutionSettingsEditor {
    model: Entity<BoardModel>,
    state: Entity<AppState>,
    project_id: String,
    /// Element-id prefix (`manual`, `detail`), so two editors never collide.
    prefix: &'static str,
    probe: BoardProbe,
    /// Custom fields; kept while another mode is selected so switching back
    /// restores them.
    implementation: BoardExecutionProfile,
    review: BoardReviewProfile,
    implementation_mode: ImplementationMode,
    review_mode: ReviewMode,
    options: OptionsState,
    /// What a harness/model change reset, newest last.
    notices: Vec<String>,
    /// Harnesses whose full model list is shown.
    expanded_models: Vec<(Which, HarnessId)>,
    more_task: Option<Task<()>>,
    load_task: Option<Task<()>>,
}

impl EventEmitter<SettingsEditorEvent> for ExecutionSettingsEditor {}

impl ExecutionSettingsEditor {
    pub fn new(
        model: Entity<BoardModel>,
        state: Entity<AppState>,
        project_id: String,
        initial: ExecutionSettingsDraft,
        prefix: &'static str,
        probe: BoardProbe,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut editor = Self {
            model,
            state,
            project_id,
            prefix,
            probe,
            implementation_mode: ImplementationMode::of(&initial.implementation),
            review_mode: ReviewMode::of(&initial.review),
            implementation: initial.implementation,
            review: initial.review,
            options: OptionsState::Loading,
            notices: Vec::new(),
            expanded_models: Vec::new(),
            more_task: None,
            load_task: None,
        };
        editor.reload(cx);
        editor
    }

    /// The choices as they would be saved.
    pub fn draft(&self) -> ExecutionSettingsDraft {
        let implementation = match self.implementation_mode {
            ImplementationMode::ProjectDefaults => BoardExecutionProfile::default(),
            ImplementationMode::Deferred => BoardExecutionProfile {
                deferred: true,
                ..Default::default()
            },
            ImplementationMode::Custom => BoardExecutionProfile {
                deferred: false,
                ..self.implementation.clone()
            },
        };
        let review = match self.review_mode {
            ReviewMode::ProjectDefault => BoardReviewProfile::default(),
            ReviewMode::Custom => self.review.clone(),
        };
        ExecutionSettingsDraft {
            implementation,
            review,
        }
    }

    /// Replace the choices (after a conflict reload).
    pub fn set_draft(&mut self, draft: ExecutionSettingsDraft, cx: &mut Context<Self>) {
        self.implementation_mode = ImplementationMode::of(&draft.implementation);
        self.review_mode = ReviewMode::of(&draft.review);
        self.implementation = draft.implementation;
        self.review = draft.review;
        self.notices.clear();
        cx.notify();
    }

    /// The loaded catalog, if any.
    pub fn options(&self) -> Option<&BoardExecutionOptions> {
        match &self.options {
            OptionsState::Ready(options) => Some(options.as_ref()),
            _ => None,
        }
    }

    /// Load (or reload) the host's catalog for this project.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.model.read(cx).api(cx) else {
            self.options = OptionsState::Error("Engine not connected.".into());
            cx.notify();
            return;
        };
        self.options = OptionsState::Loading;
        let request = BoardExecutionOptionsRequest {
            project_id: self.project_id.clone(),
            harness: None,
            query: None,
            offset: 0,
            limit: None,
        };
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = api.execution_options(request).await;
            this.update(cx, |editor, cx| {
                editor.load_task = None;
                editor.options = match result {
                    Ok(options) => OptionsState::Ready(Box::new(options)),
                    Err(error) => OptionsState::Error(options_error(&error)),
                };
                cx.emit(SettingsEditorEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Fetch the next catalog page for one harness and append it.
    fn load_more_models(&mut self, harness: HarnessId, cx: &mut Context<Self>) {
        let Some(offset) = self
            .options()
            .and_then(|options| harness_options(options, harness))
            .and_then(|entry| entry.next_offset)
        else {
            return;
        };
        let Some(api) = self.model.read(cx).api(cx) else {
            return;
        };
        let request = BoardExecutionOptionsRequest {
            project_id: self.project_id.clone(),
            harness: Some(harness),
            query: None,
            offset,
            limit: None,
        };
        self.more_task = Some(cx.spawn(async move |this, cx| {
            let result = api.execution_options(request).await;
            this.update(cx, |editor, cx| {
                editor.more_task = None;
                match result {
                    Ok(page) => {
                        if let OptionsState::Ready(options) = &mut editor.options
                            && let Some(entry) = options
                                .harnesses
                                .iter_mut()
                                .find(|entry| entry.harness == harness)
                            && let Some(more) =
                                page.harnesses.into_iter().find(|e| e.harness == harness)
                        {
                            for model in more.models {
                                if !entry.models.iter().any(|known| known.id == model.id) {
                                    entry.models.push(model);
                                }
                            }
                            entry.next_offset = more.next_offset;
                        }
                    }
                    Err(error) => editor
                        .notices
                        .push(format!("More models could not load: {}", error.message())),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    // ---- effective values ----

    fn values(&self, which: Which) -> FieldValues {
        match which {
            Which::Implementation => FieldValues {
                harness: self.implementation.harness,
                account: self.implementation.provider_account_id.clone(),
                model: self.implementation.model.clone(),
                reasoning: self.implementation.reasoning,
                options: self.implementation.model_options.clone(),
            },
            Which::Review => FieldValues {
                harness: self.review.harness,
                account: self.review.provider_account_id.clone(),
                model: self.review.model.clone(),
                reasoning: self.review.reasoning,
                options: self.review.model_options.clone(),
            },
        }
    }

    fn fields(&mut self, which: Which) -> Fields<'_> {
        match which {
            Which::Implementation => Fields {
                harness: &mut self.implementation.harness,
                account: &mut self.implementation.provider_account_id,
                model: &mut self.implementation.model,
                reasoning: &mut self.implementation.reasoning,
                options: &mut self.implementation.model_options,
            },
            Which::Review => Fields {
                harness: &mut self.review.harness,
                account: &mut self.review.provider_account_id,
                model: &mut self.review.model,
                reasoning: &mut self.review.reasoning,
                options: &mut self.review.model_options,
            },
        }
    }

    /// The implementation values the draft would launch with, for a review
    /// that falls back to the implementation model.
    fn implementation_in_effect(&self) -> FieldValues {
        match self.implementation_mode {
            ImplementationMode::Custom => self.values(Which::Implementation),
            _ => FieldValues {
                harness: None,
                account: None,
                model: None,
                reasoning: None,
                options: Default::default(),
            },
        }
    }

    /// The inherited baseline for a profile (what an unset field resolves
    /// to, as the engine reports it).
    fn baseline(&self, which: Which) -> Option<&ResolvedExecution> {
        let options = self.options()?;
        match which {
            Which::Implementation => options.effective_implementation.as_ref(),
            Which::Review if review_falls_back(options) => None,
            Which::Review => options.effective_review.as_ref(),
        }
    }

    fn review_inherits_implementation(&self, which: Which) -> bool {
        which == Which::Review && self.options().is_some_and(review_falls_back)
    }

    fn effective_harness(&self, which: Which) -> Option<HarnessId> {
        let values = self.values(which);
        if let Some(harness) = values.harness {
            return Some(harness);
        }
        if self.review_inherits_implementation(which) {
            return self
                .effective_harness_of(&self.implementation_in_effect(), Which::Implementation);
        }
        self.effective_harness_of(&values, which)
    }

    fn effective_harness_of(&self, values: &FieldValues, which: Which) -> Option<HarnessId> {
        values
            .harness
            .or_else(|| self.baseline(which).map(|resolved| resolved.harness))
            .or_else(|| {
                self.options()
                    .and_then(|options| options.project_defaults.provider.as_ref())
                    .map(|provider| provider.harness)
            })
    }

    /// The model an unset model field resolves to for `harness`.
    fn inherited_model(&self, which: Which, harness: HarnessId) -> Option<(String, &'static str)> {
        if self.review_inherits_implementation(which)
            && self.values(Which::Review).harness.is_none()
        {
            let implementation = self.implementation_in_effect();
            if let Some(model) = implementation.model {
                return Some((model, "implementation model"));
            }
            if let Some(resolved) = self.baseline(Which::Implementation)
                && resolved.harness == harness
                && let Some(model) = &resolved.model
            {
                return Some((model.clone(), "implementation model"));
            }
        }
        if let Some(resolved) = self.baseline(which)
            && resolved.harness == harness
            && let Some(model) = &resolved.model
        {
            let origin = source(resolved, "model").map_or("project", origin_label);
            return Some((model.clone(), origin));
        }
        let entry = harness_options(self.options()?, harness)?;
        entry
            .default_model
            .clone()
            .map(|model| (model, "provider default"))
    }

    fn effective_model(&self, which: Which) -> Option<String> {
        let values = self.values(which);
        if values.model.is_some() {
            return values.model;
        }
        let harness = self.effective_harness(which)?;
        self.inherited_model(which, harness).map(|(model, _)| model)
    }

    fn effective_model_info(&self, which: Which) -> Option<&Model> {
        let harness = self.effective_harness(which)?;
        let model = self.effective_model(which)?;
        catalog_model(self.options()?, harness, &model)
    }

    /// Reasoning levels the selected (or inherited) model supports.
    fn supported_levels(&self, which: Which) -> Vec<ReasoningLevel> {
        self.effective_model_info(which)
            .map(|model| model.reasoning_levels.clone())
            .unwrap_or_default()
    }

    fn inherited_reasoning(&self, which: Which) -> Option<(ReasoningLevel, &'static str)> {
        let harness = self.effective_harness(which)?;
        let model = self.effective_model(which)?;
        let resolved = if self.review_inherits_implementation(which) {
            self.baseline(Which::Implementation)
        } else {
            self.baseline(which)
        }?;
        (resolved.harness == harness && resolved.model.as_deref() == Some(model.as_str()))
            .then_some(())?;
        let level = resolved.reasoning?;
        let origin = source(resolved, "reasoning").map_or("project", origin_label);
        Some((level, origin))
    }

    fn inherited_permission(&self) -> Option<(PermissionMode, &'static str)> {
        let harness = self.effective_harness(Which::Implementation)?;
        if let Some(resolved) = self.baseline(Which::Implementation)
            && resolved.harness == harness
        {
            let origin = source(resolved, "permissionMode").map_or("project", origin_label);
            return Some((resolved.permission_mode, origin));
        }
        self.options()?
            .project_defaults
            .permission_mode
            .map(|mode| (mode, "project"))
    }

    // ---- choices ----

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(SettingsEditorEvent::Changed);
        cx.notify();
    }

    fn set_implementation_mode(&mut self, mode: ImplementationMode, cx: &mut Context<Self>) {
        self.implementation_mode = mode;
        self.changed(cx);
    }

    fn set_review_mode(&mut self, mode: ReviewMode, cx: &mut Context<Self>) {
        self.review_mode = mode;
        self.changed(cx);
    }

    fn select_harness(&mut self, which: Which, harness: Option<HarnessId>, cx: &mut Context<Self>) {
        if self.values(which).harness == harness {
            return;
        }
        *self.fields(which).harness = harness;
        self.reconcile(which, true);
        self.changed(cx);
    }

    fn select_model(&mut self, which: Which, model: Option<String>, cx: &mut Context<Self>) {
        // Pin the harness the model belongs to, so a later project-default
        // change can never pair this model with another provider.
        if model.is_some() && self.values(which).harness.is_none() {
            let harness = self.effective_harness(which);
            *self.fields(which).harness = harness;
        }
        *self.fields(which).model = model;
        self.reconcile(which, false);
        self.changed(cx);
    }

    fn select_reasoning(
        &mut self,
        which: Which,
        level: Option<ReasoningLevel>,
        cx: &mut Context<Self>,
    ) {
        *self.fields(which).reasoning = level;
        self.changed(cx);
    }

    fn select_account(&mut self, which: Which, account: Option<String>, cx: &mut Context<Self>) {
        *self.fields(which).account = account;
        self.changed(cx);
    }

    fn select_model_option(
        &mut self,
        which: Which,
        option: String,
        choice: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let fields = self.fields(which);
        match choice {
            Some(choice) => {
                fields
                    .options
                    .insert(option, serde_json::Value::String(choice));
            }
            None => {
                fields.options.remove(&option);
            }
        }
        self.changed(cx);
    }

    fn select_permission(&mut self, mode: Option<PermissionMode>, cx: &mut Context<Self>) {
        self.implementation.permission_mode = mode;
        self.changed(cx);
    }

    fn select_checkout(&mut self, checkout: Option<BoardCheckoutChoice>, cx: &mut Context<Self>) {
        self.implementation.checkout = checkout;
        self.changed(cx);
    }

    /// Drop fields the new harness/model does not support, saying so.
    fn reconcile(&mut self, which: Which, harness_changed: bool) {
        let Some(harness) = self.effective_harness(which) else {
            return;
        };
        let Some(options) = self.options() else {
            return;
        };
        let Some(entry) = harness_options(options, harness).cloned() else {
            return;
        };
        let label = harness_label(harness);
        let mut notices = Vec::new();
        let values = self.values(which);
        if let Some(account) = &values.account
            && !entry
                .accounts
                .iter()
                .any(|known| &known.account_id == account)
        {
            *self.fields(which).account = None;
            notices.push(format!(
                "{label} has no account {account}; the account now inherits."
            ));
        }
        if harness_changed
            && let Some(model) = &values.model
            && !entry.models.iter().any(|known| &known.id == model)
        {
            *self.fields(which).model = None;
            notices.push(format!(
                "{model} is not offered by {label}; the model now inherits."
            ));
        }
        let model_label = self
            .effective_model_info(which)
            .map(|model| model.label.clone())
            .or_else(|| self.effective_model(which))
            .unwrap_or_else(|| "this model".into());
        let levels = self.supported_levels(which);
        if let Some(level) = self.values(which).reasoning
            && !levels.contains(&level)
        {
            *self.fields(which).reasoning = None;
            notices.push(format!(
                "{} reasoning is not supported by {model_label}; reasoning now inherits.",
                reasoning_label(level)
            ));
        }
        let model_options = self
            .effective_model_info(which)
            .map(|model| model.options.clone())
            .unwrap_or_default();
        let chosen: Vec<(String, serde_json::Value)> =
            self.values(which).options.into_iter().collect();
        for (option, choice) in chosen {
            let supported = model_options.iter().any(|known| {
                known.id == option
                    && known
                        .choices
                        .iter()
                        .any(|c| Some(c.id.as_str()) == choice.as_str())
            });
            if !supported {
                self.fields(which).options.remove(&option);
                notices.push(format!(
                    "{option} is not an option of {model_label}; it was removed."
                ));
            }
        }
        if which == Which::Implementation
            && let Some(mode) = self.implementation.permission_mode
            && !entry.permission_modes.contains(&mode)
        {
            self.implementation.permission_mode = None;
            notices.push(format!(
                "{} is not available for {label}; access now inherits.",
                permission_label(mode)
            ));
        }
        self.notices.extend(notices);
        // Keep the notice list short: the newest few explain the last change.
        let excess = self.notices.len().saturating_sub(4);
        self.notices.drain(..excess);
    }

    // ---- rendering ----

    fn id(&self, which: Which, rest: &str) -> String {
        format!("{}-{}-{rest}", self.prefix, which.key())
    }

    #[allow(clippy::too_many_arguments)]
    fn chip(
        &self,
        id: String,
        label: impl Into<SharedString>,
        selected: bool,
        enabled: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let label: SharedString = label.into();
        self.probe.chip(&id, &label, selected);
        let probe = self.probe.bounds_el(id.clone());
        let mut chip = button::chip(id, selected, ControlSize::Sm, enabled, theme)
            .role(gpui::Role::RadioButton)
            .aria_label(label.clone())
            .max_w_full();
        if selected {
            // The check glyph carries the choice without relying on colour.
            chip = chip.child(
                button::glyph(crate::icons::CHECK, ControlSize::Sm)
                    .text_color(theme.readable_tone(theme.accent, 1.0)),
            );
        }
        chip = chip
            .child(div().min_w_0().truncate().child(label))
            .children(probe);
        if enabled {
            chip = chip.on_click(cx.listener(move |this, _, _, cx| on(this, cx)));
        }
        chip.into_any_element()
    }

    fn row(
        theme: &Theme,
        title: &'static str,
        description: Option<SharedString>,
        chips: Vec<AnyElement>,
    ) -> gpui::Div {
        widgets::card_row(theme, false)
            .child(widgets::row_text(theme, title, description))
            .child(widgets::row_block().children(chips))
    }

    fn text_row(&self, key: &str, theme: &Theme, copy: impl Into<SharedString>) -> gpui::Div {
        let copy: SharedString = copy.into();
        self.probe
            .text(format!("{}-{key}", self.prefix), copy.to_string());
        widgets::card_row(theme, false).child(
            div()
                .w_full()
                .min_w_0()
                .child(widgets::row_description(theme, copy)),
        )
    }

    fn harness_chips(
        &self,
        which: Which,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(options) = self.options() else {
            return Vec::new();
        };
        let values = self.values(which);
        let inherited = if values.harness.is_some() {
            None
        } else {
            self.effective_harness(which)
        };
        let inherit_label = match (inherited, self.review_inherits_implementation(which)) {
            (Some(harness), true) => {
                format!("Inherit · {} (implementation)", harness_label(harness))
            }
            (Some(harness), false) => {
                let origin = self
                    .baseline(which)
                    .and_then(|resolved| source(resolved, "harness"))
                    .map_or("project", origin_label);
                format!("Inherit · {} ({origin})", harness_label(harness))
            }
            (None, _) => "Inherit · not resolved yet".to_owned(),
        };
        let mut chips = vec![self.chip(
            self.id(which, "harness-inherit"),
            inherit_label,
            values.harness.is_none(),
            true,
            theme,
            cx,
            move |this, cx| this.select_harness(which, None, cx),
        )];
        let harnesses: Vec<HarnessId> = options
            .harnesses
            .iter()
            .filter(|entry| entry.available)
            .map(|entry| entry.harness)
            .collect();
        for harness in harnesses {
            chips.push(self.chip(
                self.id(which, &format!("harness:{}", harness_key(harness))),
                harness_label(harness),
                values.harness == Some(harness),
                true,
                theme,
                cx,
                move |this, cx| this.select_harness(which, Some(harness), cx),
            ));
        }
        chips
    }

    fn account_row(
        &self,
        which: Which,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let harness = self.effective_harness(which)?;
        let entry = harness_options(self.options()?, harness)?;
        // Account controls appear only when there is a real choice.
        if entry.accounts.len() < 2 {
            return None;
        }
        let accounts = entry.accounts.clone();
        let values = self.values(which);
        let active = accounts
            .iter()
            .find(|account| account.active)
            .map(|account| account.label.clone())
            .unwrap_or_else(|| "the active account".into());
        let mut chips = vec![self.chip(
            self.id(which, "account-inherit"),
            format!("Inherit · {active} (active)"),
            values.account.is_none(),
            true,
            theme,
            cx,
            move |this, cx| this.select_account(which, None, cx),
        )];
        for account in accounts {
            let id = account.account_id.clone();
            chips.push(self.chip(
                self.id(which, &format!("account:{id}")),
                account.label.clone(),
                values.account.as_deref() == Some(id.as_str()),
                true,
                theme,
                cx,
                move |this, cx| this.select_account(which, Some(id.clone()), cx),
            ));
        }
        Some(Self::row(
            theme,
            "Account",
            Some(harness_label(harness).into()),
            chips,
        ))
    }

    fn model_row(&self, which: Which, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let harness = self.effective_harness(which)?;
        let entry = harness_options(self.options()?, harness)?.clone();
        let values = self.values(which);
        let inherit_label = match self.inherited_model(which, harness) {
            Some((model, origin)) => format!(
                "Inherit · {} ({origin})",
                model_label(self.options(), harness, &model)
            ),
            None => "Inherit · the provider's default model".to_owned(),
        };
        let mut chips = vec![self.chip(
            self.id(which, "model-inherit"),
            inherit_label,
            values.model.is_none(),
            true,
            theme,
            cx,
            move |this, cx| this.select_model(which, None, cx),
        )];
        let expanded = self.expanded_models.contains(&(which, harness));
        let shown = if expanded {
            entry.models.len()
        } else {
            MODEL_CHIPS.min(entry.models.len())
        };
        for model in entry.models.iter().take(shown) {
            let id = model.id.clone();
            let selected = values.model.as_deref() == Some(id.as_str());
            chips.push(self.chip(
                self.id(which, &format!("model:{id}")),
                model.label.clone(),
                selected,
                true,
                theme,
                cx,
                move |this, cx| this.select_model(which, Some(id.clone()), cx),
            ));
        }
        // A saved model the loaded page does not list stays visible as such.
        if let Some(saved) = &values.model
            && values.harness.is_none_or(|h| h == harness)
            && !entry.models.iter().any(|model| &model.id == saved)
        {
            chips.push(self.chip(
                self.id(which, &format!("model:{saved}")),
                format!("{saved} (not in the loaded catalog)"),
                true,
                true,
                theme,
                cx,
                |_, _| {},
            ));
        }
        if entry.models.len() > shown {
            let key = (which, harness);
            chips.push(
                button::button(
                    self.id(which, "model-show-all"),
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label("Show all models")
                .child(format!("Show all {}", entry.models.len()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.expanded_models.push(key);
                    cx.notify();
                }))
                .into_any_element(),
            );
        }
        if entry.next_offset.is_some() {
            chips.push(
                button::button(
                    self.id(which, "model-more"),
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    self.more_task.is_none(),
                    theme,
                )
                .aria_label("Load more models")
                .child("More models…")
                .on_click(cx.listener(move |this, _, _, cx| this.load_more_models(harness, cx)))
                .into_any_element(),
            );
        }
        Some(Self::row(
            theme,
            "Model",
            Some(format!("{} catalog on this host", harness_label(harness)).into()),
            chips,
        ))
    }

    fn reasoning_row(&self, which: Which, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let levels = self.supported_levels(which);
        let model = self
            .effective_model_info(which)
            .map(|model| model.label.clone());
        if levels.is_empty() {
            let copy = match model {
                Some(model) => format!("Reasoning: {model} has no reasoning levels to choose."),
                None => "Reasoning: choose a model from the catalog to see its levels.".into(),
            };
            return self.text_row(&format!("{}-reasoning-none", which.key()), theme, copy);
        }
        let values = self.values(which);
        let inherit_label = match self.inherited_reasoning(which) {
            Some((level, origin)) => format!("Inherit · {} ({origin})", reasoning_label(level)),
            None => "Inherit · the model's default".to_owned(),
        };
        let mut chips = vec![self.chip(
            self.id(which, "reasoning-inherit"),
            inherit_label,
            values.reasoning.is_none(),
            true,
            theme,
            cx,
            move |this, cx| this.select_reasoning(which, None, cx),
        )];
        for level in levels {
            chips.push(self.chip(
                self.id(which, &format!("reasoning:{}", reasoning_key(level))),
                reasoning_label(level),
                values.reasoning == Some(level),
                true,
                theme,
                cx,
                move |this, cx| this.select_reasoning(which, Some(level), cx),
            ));
        }
        Self::row(
            theme,
            "Reasoning",
            model.map(|model| format!("Supported by {model}").into()),
            chips,
        )
    }

    fn option_rows(&self, which: Which, theme: &Theme, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        let Some(model) = self.effective_model_info(which).cloned() else {
            return Vec::new();
        };
        let values = self.values(which);
        let mut rows = Vec::new();
        for option in model.options {
            let current = values
                .options
                .get(&option.id)
                .and_then(|value| value.as_str())
                .map(str::to_owned);
            let default_label = option
                .choices
                .iter()
                .find(|choice| choice.id == option.default_choice)
                .map(|choice| choice.label.clone())
                .unwrap_or_else(|| option.default_choice.clone());
            let option_id = option.id.clone();
            let mut chips = vec![self.chip(
                self.id(which, &format!("option:{}:default", option.id)),
                format!("Default · {default_label}"),
                current.is_none(),
                true,
                theme,
                cx,
                {
                    let option_id = option_id.clone();
                    move |this, cx| this.select_model_option(which, option_id.clone(), None, cx)
                },
            )];
            for choice in option.choices {
                let choice_id = choice.id.clone();
                let option_id = option_id.clone();
                chips.push(self.chip(
                    self.id(which, &format!("option:{}:{}", option.id, choice.id)),
                    choice.label.clone(),
                    current.as_deref() == Some(choice.id.as_str()),
                    true,
                    theme,
                    cx,
                    move |this, cx| {
                        this.select_model_option(
                            which,
                            option_id.clone(),
                            Some(choice_id.clone()),
                            cx,
                        )
                    },
                ));
            }
            rows.push(
                widgets::card_row(theme, false)
                    .child(widgets::row_text(
                        theme,
                        "Model option",
                        Some(option.label.into()),
                    ))
                    .child(widgets::row_block().children(chips)),
            );
        }
        rows
    }

    fn access_row(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let harness = self.effective_harness(Which::Implementation)?;
        let modes = harness_options(self.options()?, harness)?
            .permission_modes
            .clone();
        let inherit_label = match self.inherited_permission() {
            Some((mode, origin)) => format!("Inherit · {} ({origin})", permission_label(mode)),
            None => "Inherit · the provider's default access".to_owned(),
        };
        let current = self.implementation.permission_mode;
        let mut chips = vec![self.chip(
            self.id(Which::Implementation, "access-inherit"),
            inherit_label,
            current.is_none(),
            true,
            theme,
            cx,
            |this, cx| this.select_permission(None, cx),
        )];
        for mode in modes {
            chips.push(self.chip(
                self.id(
                    Which::Implementation,
                    &format!("access:{}", permission_key(mode)),
                ),
                permission_label(mode),
                current == Some(mode),
                true,
                theme,
                cx,
                move |this, cx| this.select_permission(Some(mode), cx),
            ));
        }
        Some(Self::row(
            theme,
            "Access",
            Some(
                "Requested for the ticket worker. Saving it grants nothing; launch applies the \
                 existing permission rules."
                    .into(),
            ),
            chips,
        ))
    }

    fn checkout_row(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let options = self.options()?;
        let inherit = format!("Inherit · {} (project)", project_checkout_label(options));
        let current = self.implementation.checkout.clone();
        let chips = vec![
            self.chip(
                self.id(Which::Implementation, "checkout-inherit"),
                inherit,
                current.is_none(),
                true,
                theme,
                cx,
                |this, cx| this.select_checkout(None, cx),
            ),
            self.chip(
                self.id(Which::Implementation, "checkout:current"),
                "Current checkout",
                current == Some(BoardCheckoutChoice::Current),
                true,
                theme,
                cx,
                |this, cx| this.select_checkout(Some(BoardCheckoutChoice::Current), cx),
            ),
            self.chip(
                self.id(Which::Implementation, "checkout:worktree"),
                "New worktree",
                matches!(current, Some(BoardCheckoutChoice::NewWorktree { .. })),
                true,
                theme,
                cx,
                |this, cx| {
                    this.select_checkout(
                        Some(BoardCheckoutChoice::NewWorktree {
                            branch: None,
                            base: None,
                        }),
                        cx,
                    )
                },
            ),
        ];
        Some(Self::row(
            theme,
            "Checkout",
            Some("A new worktree is only created when you choose it.".into()),
            chips,
        ))
    }

    fn host_line(&self, cx: &App) -> Option<String> {
        let options = self.options()?;
        let state = self.state.read(cx);
        let name = state
            .device_name(&options.host_device_id)
            .map(str::to_owned)
            .unwrap_or_else(|| "the project's host".into());
        Some(format!(
            "Runs on {name}, the host that owns this project (the only valid host)."
        ))
    }

    fn notices_block(&self, theme: &Theme) -> Option<gpui::Div> {
        if self.notices.is_empty() {
            return None;
        }
        let mut block = div()
            .w_full()
            .mt(ui_rems(8.0))
            .flex()
            .flex_col()
            .gap(ui_rems(6.0));
        for (index, notice) in self.notices.iter().enumerate() {
            self.probe
                .text(format!("{}-notice-{index}", self.prefix), notice.clone());
            block = block.child(
                widgets::warning_strip(theme, notice.clone())
                    .mt_0()
                    .w_full(),
            );
        }
        Some(block)
    }

    fn implementation_section(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let which = Which::Implementation;
        let mode = self.implementation_mode;
        let modes = vec![
            self.chip(
                self.id(which, "mode-defaults"),
                "Use project defaults",
                mode == ImplementationMode::ProjectDefaults,
                true,
                theme,
                cx,
                |this, cx| this.set_implementation_mode(ImplementationMode::ProjectDefaults, cx),
            ),
            self.chip(
                self.id(which, "mode-custom"),
                "Choose settings",
                mode == ImplementationMode::Custom,
                true,
                theme,
                cx,
                |this, cx| this.set_implementation_mode(ImplementationMode::Custom, cx),
            ),
            self.chip(
                self.id(which, "mode-deferred"),
                "Decide later",
                mode == ImplementationMode::Deferred,
                true,
                theme,
                cx,
                |this, cx| this.set_implementation_mode(ImplementationMode::Deferred, cx),
            ),
        ];
        let mut card = widgets::section_card(theme)
            .mt(ui_rems(12.0))
            .child(widgets::section_header(
                theme,
                "Implementation",
                Some("The agent that works on the ticket after it moves to Ready.".into()),
                vec![],
            ))
            .child(widgets::card_row(theme, false).child(widgets::row_block().children(modes)));
        match mode {
            ImplementationMode::ProjectDefaults => {
                let copy = match self
                    .options()
                    .and_then(|options| options.effective_implementation.as_ref())
                {
                    Some(resolved) => format!(
                        "Uses the project's defaults, currently {}.",
                        describe_resolved(resolved, self.options(), true)
                    ),
                    None => "Uses the project's defaults, resolved when the ticket starts.".into(),
                };
                card = card.child(self.text_row("impl-summary", theme, copy));
            }
            ImplementationMode::Deferred => {
                card = card.child(self.text_row(
                    "impl-summary",
                    theme,
                    "Settings stay unresolved. Before this ticket can start, Ready asks for them; \
                     LUNA never picks greater access on its own.",
                ));
            }
            ImplementationMode::Custom => {
                let harness_chips = self.harness_chips(which, theme, cx);
                card = card.child(Self::row(theme, "Harness", None, harness_chips));
                if let Some(row) = self.account_row(which, theme, cx) {
                    card = card.child(row);
                }
                if let Some(row) = self.model_row(which, theme, cx) {
                    card = card.child(row);
                }
                card = card.child(self.reasoning_row(which, theme, cx));
                for row in self.option_rows(which, theme, cx) {
                    card = card.child(row);
                }
                if let Some(row) = self.access_row(theme, cx) {
                    card = card.child(row);
                }
                if let Some(row) = self.checkout_row(theme, cx) {
                    card = card.child(row);
                }
            }
        }
        if let Some(line) = self.host_line(cx) {
            card = card.child(self.text_row("host", theme, line));
        }
        card.into_any_element()
    }

    fn review_section(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let which = Which::Review;
        let mode = self.review_mode;
        let modes = vec![
            self.chip(
                self.id(which, "mode-default"),
                "Project default",
                mode == ReviewMode::ProjectDefault,
                true,
                theme,
                cx,
                |this, cx| this.set_review_mode(ReviewMode::ProjectDefault, cx),
            ),
            self.chip(
                self.id(which, "mode-custom"),
                "Choose reviewer",
                mode == ReviewMode::Custom,
                true,
                theme,
                cx,
                |this, cx| this.set_review_mode(ReviewMode::Custom, cx),
            ),
        ];
        let mut card = widgets::section_card(theme)
            .mt(ui_rems(12.0))
            .child(widgets::section_header(
                theme,
                "Review",
                Some(
                    "A temporary reviewer side chat checks each result. Its access is limited \
                     to the review contract."
                        .into(),
                ),
                vec![],
            ))
            .child(widgets::card_row(theme, false).child(widgets::row_block().children(modes)));
        match mode {
            ReviewMode::ProjectDefault => {
                let copy = match self.options() {
                    Some(options) if review_falls_back(options) => REVIEW_FALLBACK_TEXT.to_owned(),
                    Some(options) => match &options.effective_review {
                        Some(review) => format!(
                            "Uses the project's reviewer, currently {}.",
                            describe_resolved(review, Some(options), false)
                        ),
                        None => REVIEW_FALLBACK_TEXT.to_owned(),
                    },
                    None => "Uses the project's reviewer.".into(),
                };
                card = card.child(self.text_row("review-summary", theme, copy));
            }
            ReviewMode::Custom => {
                let harness_chips = self.harness_chips(which, theme, cx);
                card = card.child(Self::row(theme, "Harness", None, harness_chips));
                if let Some(row) = self.account_row(which, theme, cx) {
                    card = card.child(row);
                }
                if let Some(row) = self.model_row(which, theme, cx) {
                    card = card.child(row);
                }
                card = card.child(self.reasoning_row(which, theme, cx));
                for row in self.option_rows(which, theme, cx) {
                    card = card.child(row);
                }
            }
        }
        card.into_any_element()
    }

    /// Fixture readback: modes, draft, notices and the loaded catalog.
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_state(&self) -> serde_json::Value {
        serde_json::json!({
            "implementationMode": self.implementation_mode.key(),
            "reviewMode": self.review_mode.key(),
            "draft": {
                "implementation": self.draft().implementation,
                "review": self.draft().review,
            },
            "notices": self.notices,
            "options": match &self.options {
                OptionsState::Loading => serde_json::json!("loading"),
                OptionsState::Ready(options) => serde_json::to_value(options).unwrap_or_default(),
                OptionsState::Error(error) => serde_json::json!({ "error": error }),
            },
            "supportedReasoning": {
                "implementation": self.supported_levels(Which::Implementation),
                "review": self.supported_levels(Which::Review),
            },
        })
    }
}

fn options_error(error: &BoardCallError) -> String {
    format!("The host's choices could not load: {}", error.message())
}

impl Render for ExecutionSettingsEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let mut root = div()
            .id(SharedString::from(format!("{}-settings", self.prefix)))
            .w_full()
            .flex()
            .flex_col();
        match &self.options {
            OptionsState::Loading => {
                self.probe
                    .text(format!("{}-options", self.prefix), "loading".to_owned());
                root = root.child(widgets::row_description(
                    &theme,
                    "Loading the host's models, reasoning and access choices…",
                ));
            }
            OptionsState::Error(error) => {
                self.probe
                    .text(format!("{}-options", self.prefix), error.clone());
                root = root
                    .child(widgets::error_strip(&theme, error.clone()).mt_0())
                    .child(
                        div().mt(ui_rems(8.0)).child(
                            button::button(
                                format!("{}-options-retry", self.prefix),
                                ButtonKind::Secondary,
                                ControlSize::Sm,
                                true,
                                &theme,
                            )
                            .aria_label("Retry loading the host's choices")
                            .child("Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                        ),
                    );
            }
            OptionsState::Ready(_) => {}
        }
        root = root
            .child(self.implementation_section(&theme, cx))
            .children(self.notices_block(&theme))
            .child(self.review_section(&theme, cx));
        root
    }
}
