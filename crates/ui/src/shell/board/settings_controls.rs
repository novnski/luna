//! Implementation/review settings controls (A9): harness/account/model,
//! supported reasoning/options, access, checkout and the reviewer profile,
//! fed by the host's real catalog (`BoardApi::execution_options`).
//!
//! The controls are the chat composer's own pickers (a detached
//! [`Pickers`] per profile: the model pill with its harness rail, model list
//! and reasoning/options tray, the access lock, the checkout and a new
//! worktree's base branch), so choosing a ticket's agent looks and works like
//! choosing a chat's. Each profile row adds one mode menu (project defaults,
//! choose, decide later). An unset field shows what it inherits, resolved by
//! the engine; picking anything makes that one field the ticket's own.
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
    SharedString, Styled, Task, Window, div, prelude::*, px,
};
use luna_proto::{
    BoardCheckoutChoice, BoardExecutionOptions, BoardExecutionOptionsRequest,
    BoardExecutionProfile, BoardHarnessOptions, BoardReviewProfile, BoardSettingOrigin, HarnessId,
    Model, PermissionMode, ReasoningLevel, ResolvedExecution,
};

use super::api::BoardCallError;
use super::detail::BoardProbe;
use super::model::BoardModel;
use crate::button::{self, ButtonKind, ControlSize};
use crate::pickers::{CheckoutKind, DraftConfig, Pickers};
use crate::popover;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;
use crate::typography::ui_rems;

/// Shown wherever the review profile inherits and no reviewer is configured.
pub(crate) const REVIEW_FALLBACK_TEXT: &str =
    "No reviewer is configured: the implementation model reviews in a separate reviewer chat.";

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
pub enum Which {
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
        BoardSettingOrigin::ExistingWorker => "existing chat",
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
    load_task: Option<Task<()>>,
    /// The chat composer's pickers, detached: the implementation profile's
    /// (model, access, checkout) and the reviewer's (model only).
    impl_pickers: Entity<Pickers>,
    review_pickers: Entity<Pickers>,
    /// The open mode menu.
    mode_menu: popover::Popup<Which>,
    _pickers: [gpui::Subscription; 2],
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
        let impl_pickers =
            cx.new(|cx| Pickers::new_detached(state.clone(), project_id.clone(), true, true, cx));
        let review_pickers =
            cx.new(|cx| Pickers::new_detached(state.clone(), project_id.clone(), false, false, cx));
        let watch = |which: Which| {
            move |editor: &mut Self, _: Entity<Pickers>, cx: &mut Context<Self>| {
                editor.on_pickers_changed(which, cx)
            }
        };
        let subscriptions = [
            cx.observe(&impl_pickers, watch(Which::Implementation)),
            cx.observe(&review_pickers, watch(Which::Review)),
        ];
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
            load_task: None,
            impl_pickers,
            review_pickers,
            mode_menu: popover::Popup::default(),
            _pickers: subscriptions,
        };
        editor.sync_pickers(Which::Implementation, cx);
        editor.sync_pickers(Which::Review, cx);
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
        self.sync_pickers(Which::Implementation, cx);
        self.sync_pickers(Which::Review, cx);
        self.sync_inherited(cx);
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
                editor.sync_inherited(cx);
                cx.emit(SettingsEditorEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
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
                account: &mut self.implementation.provider_account_id,
                model: &mut self.implementation.model,
                reasoning: &mut self.implementation.reasoning,
                options: &mut self.implementation.model_options,
            },
            Which::Review => Fields {
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

    // ---- choices ----

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(SettingsEditorEvent::Changed);
        cx.notify();
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
        // Only a complete catalog can say a model is not offered (the pickers
        // list the host's full catalog; this one may be a first page).
        if harness_changed
            && entry.next_offset.is_none()
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
        // What the model supports is only known for a catalogued model.
        let known_model = self.effective_model_info(which).is_some();
        let levels = self.supported_levels(which);
        if known_model
            && let Some(level) = self.values(which).reasoning
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
            if known_model && !supported {
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

    // ---- the chat's pickers ----

    fn pickers(&self, which: Which) -> &Entity<Pickers> {
        match which {
            Which::Implementation => &self.impl_pickers,
            Which::Review => &self.review_pickers,
        }
    }

    /// What a profile's pickers show as picks (unset fields inherit; only
    /// Custom has picks), and whether its checkout is a pick.
    fn picker_draft(&self, which: Which) -> (DraftConfig, bool) {
        match which {
            Which::Implementation => {
                if self.implementation_mode != ImplementationMode::Custom {
                    return (DraftConfig::default(), false);
                }
                let profile = &self.implementation;
                let (checkout, branch) = match &profile.checkout {
                    Some(BoardCheckoutChoice::NewWorktree { base, .. }) => {
                        (CheckoutKind::NewWorktree, base.clone())
                    }
                    _ => (CheckoutKind::Local, None),
                };
                (
                    DraftConfig {
                        harness: profile.harness,
                        provider_account_id: profile.provider_account_id.clone(),
                        model: profile.model.clone(),
                        reasoning: profile.reasoning,
                        branch,
                        checkout,
                        permission_mode: profile.permission_mode,
                        model_options: profile.model_options.clone(),
                    },
                    profile.checkout.is_some(),
                )
            }
            Which::Review => {
                if self.review_mode != ReviewMode::Custom {
                    return (DraftConfig::default(), false);
                }
                let profile = &self.review;
                (
                    DraftConfig {
                        harness: profile.harness,
                        provider_account_id: profile.provider_account_id.clone(),
                        model: profile.model.clone(),
                        reasoning: profile.reasoning,
                        model_options: profile.model_options.clone(),
                        ..DraftConfig::default()
                    },
                    false,
                )
            }
        }
    }

    /// Show the profile's picks in its pickers.
    fn sync_pickers(&mut self, which: Which, cx: &mut Context<Self>) {
        let (draft, checkout_explicit) = self.picker_draft(which);
        self.pickers(which).clone().update(cx, |pickers, cx| {
            pickers.set_detached_draft(draft, checkout_explicit, cx);
        });
    }

    /// What each profile's unset fields show: the engine's resolution (the
    /// reviewer, with no reviewer configured, follows the implementation).
    fn sync_inherited(&mut self, cx: &mut Context<Self>) {
        if self.options().is_none() {
            return;
        }
        let label = |this: &Self, harness: HarnessId, model: String| {
            let label = model_label(this.options(), harness, &model);
            (model, label)
        };
        let implementation = self.baseline(Which::Implementation).map(|resolved| {
            (
                resolved.harness,
                resolved
                    .model
                    .clone()
                    .map(|model| label(self, resolved.harness, model)),
                resolved.reasoning,
                Some(resolved.permission_mode),
            )
        });
        let checkout = self
            .options()
            .and_then(|options| options.project_defaults.checkout);
        let review = if self.review_inherits_implementation(Which::Review) {
            self.effective_harness(Which::Review).map(|harness| {
                let model = self
                    .effective_model(Which::Review)
                    .map(|model| label(self, harness, model));
                let reasoning = self.implementation_in_effect().reasoning.or_else(|| {
                    self.inherited_reasoning(Which::Review)
                        .map(|(level, _)| level)
                });
                (harness, model, reasoning, None)
            })
        } else {
            self.baseline(Which::Review).map(|resolved| {
                (
                    resolved.harness,
                    resolved
                        .model
                        .clone()
                        .map(|model| label(self, resolved.harness, model)),
                    resolved.reasoning,
                    None,
                )
            })
        };
        for (pickers, inherited, checkout) in [
            (self.impl_pickers.clone(), implementation, checkout),
            (self.review_pickers.clone(), review, None),
        ] {
            let (harness, model, reasoning, permission) = match inherited {
                Some((harness, model, reasoning, permission)) => {
                    (Some(harness), model, reasoning, permission)
                }
                None => (None, None, None, None),
            };
            pickers.update(cx, |pickers, cx| {
                pickers.set_detached_inherited(harness, model, reasoning, permission, checkout, cx)
            });
        }
    }

    /// A pick in a profile's pickers: that field becomes the ticket's own
    /// (Custom), the rest keep inheriting; unsupported leftovers are reset
    /// with a notice and the pickers show the result.
    fn on_pickers_changed(&mut self, which: Which, cx: &mut Context<Self>) {
        let (draft, checkout_explicit) = {
            let pickers = self.pickers(which).read(cx);
            (pickers.draft().clone(), pickers.checkout_explicit())
        };
        let changed = match which {
            Which::Implementation => {
                let current = match self.implementation_mode {
                    ImplementationMode::Custom => self.implementation.clone(),
                    _ => BoardExecutionProfile::default(),
                };
                let kept_branch = match &self.implementation.checkout {
                    Some(BoardCheckoutChoice::NewWorktree { branch, .. }) => branch.clone(),
                    _ => None,
                };
                let next = BoardExecutionProfile {
                    harness: draft.harness,
                    provider_account_id: draft.provider_account_id.clone(),
                    model: draft.model.clone(),
                    reasoning: draft.reasoning,
                    model_options: draft.model_options.clone(),
                    permission_mode: draft.permission_mode,
                    checkout: checkout_explicit.then(|| match draft.checkout {
                        CheckoutKind::Local => BoardCheckoutChoice::Current,
                        CheckoutKind::NewWorktree => BoardCheckoutChoice::NewWorktree {
                            branch: kept_branch,
                            base: draft.branch.clone(),
                        },
                    }),
                    deferred: false,
                };
                if next == current {
                    false
                } else {
                    let harness_changed = next.harness != current.harness;
                    self.implementation = next;
                    self.implementation_mode = ImplementationMode::Custom;
                    self.reconcile(which, harness_changed);
                    true
                }
            }
            Which::Review => {
                let current = match self.review_mode {
                    ReviewMode::Custom => self.review.clone(),
                    ReviewMode::ProjectDefault => BoardReviewProfile::default(),
                };
                let next = BoardReviewProfile {
                    harness: draft.harness,
                    provider_account_id: draft.provider_account_id.clone(),
                    model: draft.model.clone(),
                    reasoning: draft.reasoning,
                    model_options: draft.model_options.clone(),
                    disabled: current.disabled,
                };
                if next == current {
                    false
                } else {
                    let harness_changed = next.harness != current.harness;
                    self.review = next;
                    self.review_mode = ReviewMode::Custom;
                    self.reconcile(which, harness_changed);
                    true
                }
            }
        };
        if changed {
            self.sync_pickers(which, cx);
            // The reviewer may follow the implementation's picks.
            self.sync_inherited(cx);
            self.changed(cx);
        }
    }

    // ---- rendering ----

    fn id(&self, which: Which, rest: &str) -> String {
        format!("{}-{}-{rest}", self.prefix, which.key())
    }

    fn mode_label(&self, which: Which) -> &'static str {
        match which {
            Which::Implementation => match self.implementation_mode {
                ImplementationMode::ProjectDefaults => "Project defaults",
                ImplementationMode::Custom => "Custom",
                ImplementationMode::Deferred => "Decide later",
            },
            Which::Review => match self.review_mode {
                ReviewMode::ProjectDefault => "Project default",
                ReviewMode::Custom => "Custom reviewer",
            },
        }
    }

    fn toggle_mode_menu(&mut self, which: Which, cx: &mut Context<Self>) {
        if self.mode_menu.get() == Some(&which) && !self.mode_menu.is_closing() {
            self.close_mode_menu(cx);
        } else {
            self.mode_menu.open(which);
            cx.notify();
        }
    }

    fn close_mode_menu(&mut self, cx: &mut Context<Self>) {
        if self.mode_menu.begin_close() {
            popover::reap_popup(cx, |editor: &mut Self| &mut editor.mode_menu);
        }
        cx.notify();
    }

    fn pick_mode(&mut self, which: Which, index: usize, cx: &mut Context<Self>) {
        match which {
            Which::Implementation => {
                self.implementation_mode = match index {
                    0 => ImplementationMode::ProjectDefaults,
                    1 => ImplementationMode::Custom,
                    _ => ImplementationMode::Deferred,
                };
            }
            Which::Review => {
                self.review_mode = if index == 0 {
                    ReviewMode::ProjectDefault
                } else {
                    ReviewMode::Custom
                };
            }
        }
        self.close_mode_menu(cx);
        self.sync_pickers(which, cx);
        self.sync_inherited(cx);
        self.changed(cx);
    }

    /// The mode menu's rows: (label, detail, selected).
    fn mode_rows(&self, which: Which) -> Vec<(&'static str, &'static str, bool)> {
        match which {
            Which::Implementation => vec![
                (
                    "Project defaults",
                    "Use the project's agent settings",
                    self.implementation_mode == ImplementationMode::ProjectDefaults,
                ),
                (
                    "Custom",
                    "Pick this ticket's own agent",
                    self.implementation_mode == ImplementationMode::Custom,
                ),
                (
                    "Decide later",
                    "Choose before it moves to Ready",
                    self.implementation_mode == ImplementationMode::Deferred,
                ),
            ],
            Which::Review => vec![
                (
                    "Project default",
                    "The project's reviewer",
                    self.review_mode == ReviewMode::ProjectDefault,
                ),
                (
                    "Custom reviewer",
                    "Pick this ticket's reviewer",
                    self.review_mode == ReviewMode::Custom,
                ),
            ],
        }
    }

    /// The mode pop-up button with its menu.
    fn render_mode_menu(&self, which: Which, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let open = self.mode_menu.get() == Some(&which);
        let closing = self.mode_menu.closing_since();
        let element = self.id(which, "mode");
        self.probe.chip(&element, self.mode_label(which), true);
        let trigger =
            crate::pickers::ghost_pill(element.clone(), self.mode_label(which), true, open, theme)
                .aria_label(SharedString::from(format!(
                    "{} settings: {}",
                    match which {
                        Which::Implementation => "Implementation",
                        Which::Review => "Review",
                    },
                    self.mode_label(which)
                )))
                .tab_index(0)
                .on_key_down(crate::swallow_activation_key)
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_mode_menu(which, cx)))
                .children(self.probe.bounds_el(element.clone()));
        let mut trigger = trigger.relative();
        if open {
            let popup = theme.for_popup();
            let rows: Vec<AnyElement> = self
                .mode_rows(which)
                .into_iter()
                .enumerate()
                .map(|(index, (label, detail, selected))| {
                    let row_id = self.id(which, &format!("mode-{index}"));
                    popover::menu_row_nav(&popup, selected, false)
                        .id(SharedString::from(row_id.clone()))
                        .role(gpui::Role::MenuItem)
                        .aria_label(SharedString::from(format!("{label}: {detail}")))
                        .hover(|style| style.bg(popover::menu_hover_bg()))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.pick_mode(which, index, cx)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(div().child(label))
                                .child(
                                    div()
                                        .text_size(ui_rems(11.5))
                                        .text_color(popup.readable_muted(0.75))
                                        .child(detail),
                                ),
                        )
                        .children(self.probe.bounds_el(row_id))
                        .into_any_element()
                })
                .collect();
            let menu = popover::popover_card(&popup)
                .w(ui_rems(popover::MENU_W_M))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_mode_menu(cx)))
                .flex()
                .flex_col()
                .gap(px(popover::MENU_GAP))
                .children(rows)
                .into_any_element();
            trigger = trigger.child(popover::anchored_menu_below_leading(
                SharedString::from(format!("{element}-menu")),
                menu,
                closing,
            ));
        }
        trigger.into_any_element()
    }

    /// The muted line under a profile row: what it inherits, or why it has
    /// nothing to pick.
    fn caption(&self, which: Which, cx: &App) -> Option<String> {
        match which {
            Which::Implementation => match self.implementation_mode {
                ImplementationMode::Deferred => Some(
                    "Settings stay unresolved until you choose them; Ready asks first and LUNA \
                     never picks greater access on its own."
                        .into(),
                ),
                ImplementationMode::ProjectDefaults => Some(match self.host_line(cx) {
                    Some(host) => format!("Inherits the project's settings. {host}"),
                    None => "Inherits the project's settings.".into(),
                }),
                ImplementationMode::Custom => self.host_line(cx),
            },
            Which::Review => match self.review_mode {
                ReviewMode::ProjectDefault => Some(match self.options() {
                    Some(options) if review_falls_back(options) => REVIEW_FALLBACK_TEXT.to_owned(),
                    _ => "A temporary reviewer chat checks each result.".into(),
                }),
                ReviewMode::Custom => None,
            },
        }
    }

    /// One profile: its label, mode menu and the chat's pickers on one line,
    /// with a muted caption. `label`: show the row's own label (the form's
    /// implementation row sits in the description card, which names it).
    pub fn render_row(
        &mut self,
        which: Which,
        label: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let deferred = which == Which::Implementation
            && self.implementation_mode == ImplementationMode::Deferred;
        let caption = self.caption(which, cx);
        if let Some(caption) = &caption {
            self.probe.text(self.id(which, "caption"), caption.clone());
        }
        let pickers = self.pickers(which).clone();
        let pickers_id = self.id(which, "pickers");
        let line = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(ui_rems(2.0))
            .min_w_0()
            .when(label, |line| {
                line.child(
                    div()
                        .w(ui_rems(92.0))
                        .flex_none()
                        .text_size(ui_rems(12.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.readable_muted(0.85))
                        .child(match which {
                            Which::Implementation => "Agent",
                            Which::Review => "Reviewer",
                        }),
                )
            })
            .child(self.render_mode_menu(which, theme, cx))
            .when(!deferred, |line| {
                line.child(
                    div()
                        .flex_none()
                        .w(px(1.0))
                        .h(ui_rems(14.0))
                        .mx(ui_rems(4.0))
                        .bg(theme.separator(false)),
                )
                .child(
                    div()
                        .id(SharedString::from(pickers_id.clone()))
                        .min_w_0()
                        .child(pickers)
                        .children(self.probe.bounds_el(pickers_id)),
                )
            });
        div()
            .id(SharedString::from(self.id(which, "row")))
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(2.0))
            .child(line)
            .when_some(caption, |row, caption| {
                row.child(
                    div()
                        .when(label, |el| el.pl(ui_rems(92.0 + 10.0)))
                        .when(!label, |el| el.pl(ui_rems(10.0)))
                        .text_size(ui_rems(11.5))
                        .line_height(ui_rems(15.0))
                        .text_color(theme.readable_muted(0.7))
                        .child(caption),
                )
            })
            .into_any_element()
    }

    fn host_line(&self, cx: &App) -> Option<String> {
        let options = self.options()?;
        let state = self.state.read(cx);
        let name = state
            .device_name(&options.host_device_id)
            .map(str::to_owned)
            .unwrap_or_else(|| "the project's host".into());
        Some(format!("Runs on {name}, the project's host."))
    }

    /// What a pick reset, as compact warning lines.
    pub fn render_notices(&self, theme: &Theme) -> Option<AnyElement> {
        if self.notices.is_empty() {
            return None;
        }
        let mut block = div().w_full().flex().flex_col().gap(ui_rems(4.0));
        for (index, notice) in self.notices.iter().enumerate() {
            self.probe
                .text(format!("{}-notice-{index}", self.prefix), notice.clone());
            block = block.child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(ui_rems(6.0))
                    .text_size(ui_rems(11.5))
                    .line_height(ui_rems(15.0))
                    .text_color(theme.readable_tone(theme.warning, 0.85))
                    .child(
                        crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                            .size(ui_rems(12.0))
                            .mt(ui_rems(1.5))
                            .flex_none(),
                    )
                    .child(div().min_w_0().child(notice.clone())),
            );
        }
        Some(block.into_any_element())
    }

    /// Loading or error state of the host's catalog, if any.
    pub fn render_status(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        match &self.options {
            OptionsState::Loading => {
                self.probe
                    .text(format!("{}-options", self.prefix), "loading".to_owned());
                None
            }
            OptionsState::Error(error) => {
                self.probe
                    .text(format!("{}-options", self.prefix), error.clone());
                Some(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(ui_rems(8.0))
                        .child(widgets::error_strip(theme, error.clone()).mt_0().flex_1())
                        .child(
                            button::button(
                                format!("{}-options-retry", self.prefix),
                                ButtonKind::Secondary,
                                ControlSize::Sm,
                                true,
                                theme,
                            )
                            .aria_label("Retry loading the host's choices")
                            .child("Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                        )
                        .into_any_element(),
                )
            }
            OptionsState::Ready(_) => None,
        }
    }

    /// Fixture readback: modes, draft, notices and the loaded catalog.
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_state(&self, cx: &App) -> serde_json::Value {
        let pickers = |pickers: &Entity<Pickers>| {
            let pickers = pickers.read(cx);
            serde_json::json!({
                "draft": serde_json::to_value(pickers.draft()).unwrap_or_default(),
                "checkoutExplicit": pickers.checkout_explicit(),
                "isOpen": pickers.is_open(),
                "ui": pickers.fixture_detached_readback(cx),
            })
        };
        serde_json::json!({
            "pickers": {
                "implementation": pickers(&self.impl_pickers),
                "review": pickers(&self.review_pickers),
            },
            "modeMenu": self.mode_menu.get().map(|which| which.key()),
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
        let status = self.render_status(&theme, cx);
        let implementation = self.render_row(Which::Implementation, true, &theme, cx);
        let review = self.render_row(Which::Review, true, &theme, cx);
        div()
            .id(SharedString::from(format!(
                "{}-settings-editor",
                self.prefix
            )))
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_rems(8.0))
            .children(status)
            .child(implementation)
            .children(self.render_notices(&theme))
            .child(review)
    }
}

/// One profile row of an editor as its own view, so a host can place the
/// implementation row inside its description card and the reviewer below.
pub struct SettingsRow {
    editor: Entity<ExecutionSettingsEditor>,
    which: Which,
    label: bool,
    _editor: gpui::Subscription,
}

impl SettingsRow {
    pub fn new(
        editor: Entity<ExecutionSettingsEditor>,
        which: Which,
        label: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let observer = cx.observe(&editor, |_, _, cx| cx.notify());
        Self {
            editor,
            which,
            label,
            _editor: observer,
        }
    }
}

impl Render for SettingsRow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let (which, label) = (self.which, self.label);
        self.editor.update(cx, |editor, cx| {
            let status = (which == Which::Implementation)
                .then(|| editor.render_status(&theme, cx))
                .flatten();
            let row = editor.render_row(which, label, &theme, cx);
            let notices = (which == Which::Implementation)
                .then(|| editor.render_notices(&theme))
                .flatten();
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(ui_rems(6.0))
                .children(status)
                .child(row)
                .children(notices)
        })
    }
}
