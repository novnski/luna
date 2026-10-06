//! Saved provider defaults, shared with agent-created fresh chats.
use gpui::{
    AnyElement, Context, Entity, SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use luna_proto::{
    HarnessId, Model, ProviderDefaultsSnapshot, ProviderExecutionDefault, ReasoningLevel,
};
use luna_rpc::methods;

use crate::{settings::widgets, state::AppState, theme::Theme};

#[derive(Clone)]
enum Action {
    Provider(HarnessId),
    Models,
    Model(Option<String>),
    Reasoning(Option<ReasoningLevel>),
    Option(String, String),
    Reset,
    Reload,
}

pub struct ProviderDefaultsPanel {
    state: Entity<AppState>,
    provider: HarnessId,
    snapshot: Option<ProviderDefaultsSnapshot>,
    models: Vec<Model>,
    models_open: bool,
    busy: bool,
    error: Option<String>,
    generation: u64,
    task: Option<Task<()>>,
    _observe: Subscription,
}

impl ProviderDefaultsPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let generation = state.read(cx).engine_generation();
        let observe = cx.observe(&state, |this: &mut Self, _, cx| {
            if this.generation != this.state.read(cx).engine_generation() {
                this.task = None;
                this.snapshot = None;
                this.models.clear();
                this.models_open = false;
                this.load(cx);
            }
            cx.notify();
        });
        let mut this = Self {
            state,
            provider: HarnessId::Codex,
            snapshot: None,
            models: Vec::new(),
            models_open: false,
            busy: false,
            error: None,
            generation,
            task: None,
            _observe: observe,
        };
        this.load(cx);
        this
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.generation = self.state.read(cx).engine_generation();
        let generation = self.generation;
        let provider = self.provider;
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.busy = false;
            self.error = Some("Connect to this device to load agent defaults.".into());
            return;
        };
        self.busy = true;
        self.error = None;
        self.models.clear();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let defaults = engine
                    .client()
                    .call(methods::GET_PROVIDER_DEFAULTS, serde_json::json!({}))
                    .await
                    .map_err(|e| e.to_string())?;
                let models = engine
                    .client()
                    .call(
                        methods::LIST_MODELS,
                        serde_json::json!({"harness":provider}),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>((
                    serde_json::from_value::<ProviderDefaultsSnapshot>(defaults)
                        .map_err(|e| e.to_string())?,
                    serde_json::from_value::<Vec<Model>>(models).map_err(|e| e.to_string())?,
                ))
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.state.read(cx).engine_generation() != generation
                    || this.provider != provider
                {
                    return;
                }
                this.busy = false;
                match result {
                    Ok((snapshot, models)) => {
                        this.state.update(cx, |state, cx| {
                            if snapshot.revision >= state.provider_defaults.revision {
                                state.provider_defaults = snapshot.clone();
                            }
                            state.provider_defaults_loaded = true;
                            state.provider_defaults_error = None;
                            cx.notify();
                        });
                        this.snapshot = Some(this.state.read(cx).provider_defaults.clone());
                        this.models = models;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn selection(&self) -> ProviderExecutionDefault {
        self.snapshot
            .as_ref()
            .and_then(|s| s.providers.get(&self.provider))
            .cloned()
            .unwrap_or_default()
    }

    fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match action {
            Action::Provider(provider) => {
                self.provider = provider;
                self.models_open = false;
                self.load(cx);
                return;
            }
            Action::Reload => {
                self.load(cx);
                return;
            }
            Action::Models => {
                self.models_open = !self.models_open;
                cx.notify();
                return;
            }
            _ => {}
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let revision = snapshot.revision;
        let mut defaults = self.selection();
        let defaults = match action {
            Action::Model(model) => {
                defaults.model = model;
                defaults.reasoning = None;
                defaults.model_options.clear();
                self.models_open = false;
                Some(defaults)
            }
            Action::Reasoning(reasoning) => {
                defaults.reasoning = reasoning;
                Some(defaults)
            }
            Action::Option(key, value) => {
                defaults.model_options.insert(key, serde_json::json!(value));
                Some(defaults)
            }
            Action::Reset => None,
            _ => return,
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let provider = self.provider;
        let generation = self.state.read(cx).engine_generation();
        self.busy = true;
        self.error = None;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::SET_PROVIDER_DEFAULTS, serde_json::json!({"expectedRevision":revision,"provider":provider,"defaults":defaults})).await;
            let _ = this.update(cx, |this, cx| {
                if this.state.read(cx).engine_generation() != generation || this.provider != provider { return; }
                this.busy = false;
                match result.map_err(|e|e.to_string()).and_then(|value|serde_json::from_value::<ProviderDefaultsSnapshot>(value).map_err(|e|e.to_string())) {
                    Ok(snapshot) => {
                        this.state.update(cx, |state, cx| {
                            if snapshot.revision >= state.provider_defaults.revision { state.provider_defaults = snapshot.clone(); }
                            state.provider_defaults_loaded = true;
                            state.provider_defaults_error = None;
                            cx.notify();
                        });
                        this.snapshot = Some(this.state.read(cx).provider_defaults.clone());
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn choice(
        &self,
        id: String,
        label: String,
        selected: bool,
        action: Action,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let generation = self.state.read(cx).engine_generation();
        let provider = self.provider;
        crate::button::chip(
            id,
            selected,
            crate::button::ControlSize::Md,
            !self.busy,
            theme,
        )
        .aria_label(label.clone())
        .child(SharedString::from(label))
        .on_click(cx.listener(move |this, _, _, cx| {
            if this.state.read(cx).engine_generation() == generation && this.provider == provider {
                this.act(action.clone(), cx);
            }
        }))
        .into_any_element()
    }
}

impl Render for ProviderDefaultsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let defaults = self.selection();
        let selected_model = defaults
            .model
            .as_deref()
            .and_then(|id| self.models.iter().find(|model| model.id == id))
            .or_else(|| {
                if defaults.model.is_none() {
                    self.models.first()
                } else {
                    None
                }
            });
        let mut providers = div().flex().flex_wrap().gap(px(7.0));
        for (provider, label) in [
            (HarnessId::Codex, "Codex"),
            (HarnessId::ClaudeCode, "Claude Code"),
            (HarnessId::Opencode, "OpenCode"),
        ] {
            providers = providers.child(self.choice(
                format!("defaults-provider-{provider:?}"),
                label.into(),
                self.provider == provider,
                Action::Provider(provider),
                &theme,
                cx,
            ));
        }
        let label = selected_model
            .map(|model| model.label.clone())
            .or_else(|| defaults.model.clone())
            .unwrap_or_else(|| "Provider default model".into());
        let mut content = div().flex().flex_col().gap(px(12.0))
            .child(div().text_color(theme.text).child("Defaults for new chats"))
            .child(div().text_color(theme.text_muted).text_size(crate::typography::ui_rems(12.0)).child("Used by you and your agents when creating fresh chats. Explicit choices and project model overrides take precedence. Existing chats and forks keep their settings."))
            .child(providers)
            .child(self.choice("defaults-model-picker".into(),format!("Model: {label}"),self.models_open,Action::Models,&theme,cx));
        if self.models_open {
            let mut choices = div().flex().flex_wrap().gap(px(7.0)).child(self.choice(
                "defaults-model-provider".into(),
                "Provider default model".into(),
                defaults.model.is_none(),
                Action::Model(None),
                &theme,
                cx,
            ));
            for model in &self.models {
                choices = choices.child(self.choice(
                    format!("defaults-model-{}", model.id),
                    model.label.clone(),
                    defaults.model.as_deref() == Some(&model.id),
                    Action::Model(Some(model.id.clone())),
                    &theme,
                    cx,
                ));
            }
            content = content.child(choices);
        }
        if let Some(model) = selected_model {
            if !model.reasoning_levels.is_empty() {
                let mut choices = div().flex().flex_wrap().gap(px(7.0)).child(self.choice(
                    "defaults-reasoning-default".into(),
                    format!(
                            "Reasoning: Default ({})",
                            luna_proto::default_reasoning_level(&model.reasoning_levels)
                                .map(crate::pickers::reasoning_label)
                                .unwrap_or("none")
                        ),
                    defaults.reasoning.is_none(),
                    Action::Reasoning(None),
                    &theme,
                    cx,
                ));
                for level in &model.reasoning_levels {
                    choices = choices.child(self.choice(
                        format!("defaults-reasoning-{level:?}"),
                        crate::pickers::reasoning_label(*level).into(),
                        defaults.reasoning == Some(*level),
                        Action::Reasoning(Some(*level)),
                        &theme,
                        cx,
                    ));
                }
                content = content.child(choices);
            }
            for option in &model.options {
                let selected = defaults
                    .model_options
                    .get(&option.id)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&option.default_choice);
                let mut choices = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(7.0))
                    .child(SharedString::from(format!("{}:", option.label)));
                for choice in &option.choices {
                    choices = choices.child(self.choice(
                        format!("defaults-option-{}-{}", option.id, choice.id),
                        choice.label.clone(),
                        selected == choice.id,
                        Action::Option(option.id.clone(), choice.id.clone()),
                        &theme,
                        cx,
                    ));
                }
                content = content.child(choices);
            }
        }
        let actions = div()
            .flex()
            .flex_wrap()
            .gap(px(7.0))
            .child(self.choice(
                "defaults-reset".into(),
                "Use provider defaults".into(),
                false,
                Action::Reset,
                &theme,
                cx,
            ))
            .child(self.choice(
                "defaults-reload".into(),
                if self.busy { "Loading…" } else { "Reload" }.into(),
                false,
                Action::Reload,
                &theme,
                cx,
            ));
        widgets::section_card(&theme)
            .mt(px(20.0))
            .p(px(16.0))
            .child(content)
            .child(actions.mt(px(12.0)))
            .when_some(self.error.clone(), |element, error| {
                element.child(widgets::error_strip(&theme, error))
            })
    }
}
