//! "Claude extensions in effect": what a Claude launch for the selected task
//! or project loads — settings sources, output style, hooks, commands and
//! skills — with scope and precedence, configured versus effective, and the
//! mode-dependent launch policy. Rows marked verified match Claude's own
//! startup answer for the same launch (`GetNativeExtensions`).

use gpui::{
    AnyElement, Context, Entity, IntoElement, Render, SharedString, Subscription, Task, Window,
    div, prelude::*, px,
};
use luna_proto::{
    HarnessId, NativeCommandKind, NativeExtensionsView, NativeSettingsScope, PermissionMode,
};
use luna_rpc::methods;

use crate::popover::Loadable;
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

pub struct NativeExtensionsPanel {
    state: Entity<AppState>,
    view: Loadable<NativeExtensionsView>,
    /// `None` previews the selected task's own effective mode.
    mode: Option<PermissionMode>,
    scope: (Option<String>, Option<String>),
    request: u64,
    task: Option<Task<()>>,
    _observer: Subscription,
}

fn selected_scope(state: &AppState) -> (Option<String>, Option<String>) {
    (
        state.selected_chat_row().map(|chat| chat.id.clone()),
        state.selected_space_row().map(|space| space.id.clone()),
    )
}

fn mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "Ask",
        PermissionMode::Auto => "Auto",
        PermissionMode::Full => "Full access",
    }
}

fn action(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    widgets::ghost_action(id, enabled, theme).aria_label(label.into())
}

fn heading(theme: &Theme, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .mt(px(16.0))
        .text_size(crate::typography::ui_rems(11.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.text_muted)
        .child(label.into())
}

fn line(theme: &Theme, title: impl Into<SharedString>, badges: Vec<gpui::Div>) -> gpui::Div {
    let mut row = div()
        .mt(px(6.0))
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .min_w_0()
                .text_size(crate::typography::ui_rems(12.5))
                .text_color(theme.text)
                .child(title.into()),
        );
    for badge in badges {
        row = row.child(badge);
    }
    row
}

fn note(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    widgets::meta_line(theme, vec![div().child(text.into()).into_any_element()])
}

impl NativeExtensionsPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observer = cx.observe(&state, |panel, state, cx| {
            let scope = selected_scope(state.read(cx));
            if scope != panel.scope {
                panel.load(false, cx);
            }
        });
        let mut panel = Self {
            scope: selected_scope(state.read(cx)),
            state,
            view: Loadable::Idle,
            mode: None,
            request: 0,
            task: None,
            _observer: observer,
        };
        panel.load(false, cx);
        panel
    }

    pub fn load(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let scope = selected_scope(self.state.read(cx));
        self.scope = scope.clone();
        self.request = self.request.wrapping_add(1);
        let request = self.request;
        let mut params = serde_json::json!({
            "harness": HarnessId::ClaudeCode,
            "refresh": refresh,
        });
        if let Some(chat) = &scope.0 {
            params["chatId"] = chat.clone().into();
        } else if let Some(space) = &scope.1 {
            params["spaceId"] = space.clone().into();
        }
        if let Some(mode) = self.mode {
            params["permissionMode"] = serde_json::to_value(mode).unwrap_or_default();
        }
        self.view = Loadable::Loading;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::GET_NATIVE_EXTENSIONS, params)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<NativeExtensionsView>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |panel, cx| {
                if panel.request != request {
                    return;
                }
                panel.view = match result {
                    Ok(view) => Loadable::Ready(view),
                    Err(error) => Loadable::Error(error),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn set_mode(&mut self, mode: Option<PermissionMode>, cx: &mut Context<Self>) {
        self.mode = mode;
        self.load(false, cx);
    }

    /// Fixture readback: the loaded view, if any.
    pub fn loaded_view(&self) -> Option<&NativeExtensionsView> {
        match &self.view {
            Loadable::Ready(view) => Some(view),
            _ => None,
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.view, Loadable::Loading)
    }

    fn render_controls(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let busy = self.is_loading();
        let mut row = div()
            .mt(px(10.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(6.0));
        let modes = [
            ("task", "Task mode", None),
            ("ask", "Ask", Some(PermissionMode::Ask)),
            ("auto", "Auto", Some(PermissionMode::Auto)),
            ("full", "Full access", Some(PermissionMode::Full)),
        ];
        let selected = modes
            .iter()
            .position(|(_, _, mode)| *mode == self.mode)
            .unwrap_or(0);
        // One segmented control for the launch mode (audit DS-16).
        row = row.child(crate::button::segmented(
            "native-ext-mode",
            modes
                .iter()
                .map(|(key, label, _)| crate::button::Segment::new(*key, *label))
                .collect(),
            selected,
            360.0,
            crate::button::ControlSize::Md,
            theme,
            cx.listener(move |panel, ix: &usize, _, cx| {
                if !busy && let Some((_, _, mode)) = modes.get(*ix) {
                    panel.set_mode(*mode, cx);
                }
            }),
        ));
        row.child(
            action(
                theme,
                "native-ext-refresh",
                "Re-check extensions with Claude",
                !busy,
            )
            .when(!busy, |el| {
                el.on_click(cx.listener(|panel, _, _, cx| panel.load(true, cx)))
            })
            .child(if busy {
                "Checking…"
            } else {
                "Re-check with Claude"
            }),
        )
    }

    fn render_view(&self, theme: &Theme, view: &NativeExtensionsView) -> AnyElement {
        let mut body = div().flex().flex_col();
        let verified = view.native_verified;
        body = body.child(
            line(
                theme,
                format!("{} launch", mode_label(view.permission_mode)),
                vec![if verified {
                    widgets::badge_active(theme, "Verified with Claude")
                } else {
                    widgets::badge(theme, "Not verified")
                }],
            )
            .mt(px(12.0)),
        );
        body = body.child(note(theme, view.launch_policy.clone()));
        if let Some(error) = &view.native_error {
            body = body.child(widgets::warning_strip(
                theme,
                format!("Claude's own readback failed: {error}. Rows below are configured, not verified."),
            ));
        }

        body = body.child(heading(
            theme,
            "Settings sources (highest precedence first)",
        ));
        for source in &view.sources {
            let state = if source.loaded {
                widgets::badge_active(theme, "loaded")
            } else {
                widgets::badge(
                    theme,
                    source.reason.clone().unwrap_or_else(|| "not loaded".into()),
                )
            };
            let mut badges = vec![widgets::badge(theme, source.scope.label()), state];
            if source.loaded
                && let Some(reason) = &source.reason
            {
                badges.push(widgets::badge(theme, reason.clone()));
            }
            body = body.child(line(theme, source.display_path.clone(), badges));
        }

        let style = &view.output_style;
        body = body.child(heading(theme, "Output style"));
        let mut badges = vec![widgets::badge(
            theme,
            style
                .configured_scope
                .map(NativeSettingsScope::label)
                .unwrap_or("Built-in"),
        )];
        match &style.native {
            Some(native) if *native == style.configured_effective => {
                badges.push(widgets::badge_active(theme, "Claude agrees"))
            }
            Some(native) => badges.push(widgets::badge(theme, format!("Claude reports {native}"))),
            None => badges.push(widgets::badge(theme, "unverified")),
        }
        body = body.child(line(
            theme,
            format!(
                "In effect: {}",
                style
                    .native
                    .as_deref()
                    .unwrap_or(&style.configured_effective)
            ),
            badges,
        ));
        for value in style.configured.iter().filter(|value| !value.applied) {
            body = body.child(note(
                theme,
                format!(
                    "{}: {} — {}",
                    value.scope.label(),
                    value.value,
                    value.reason.clone().unwrap_or_else(|| "not applied".into())
                ),
            ));
        }
        let available = style
            .available
            .iter()
            .map(|option| match option.scope {
                NativeSettingsScope::BuiltIn => option.name.clone(),
                scope => format!("{} ({})", option.name, scope.label()),
            })
            .collect::<Vec<_>>()
            .join(", ");
        body = body.child(note(theme, format!("Available: {available}")));

        let effective_hooks = view.hooks.iter().filter(|hook| hook.effective).count();
        body = body.child(heading(
            theme,
            format!("Hooks · {effective_hooks} of {} run", view.hooks.len()),
        ));
        if view.hooks.is_empty() {
            body = body.child(note(theme, "No hooks configured in the scanned settings."));
        }
        for hook in &view.hooks {
            let title = match &hook.matcher {
                Some(matcher) => format!("{} ({matcher}) · {}", hook.event, hook.handler),
                None => format!("{} · {}", hook.event, hook.handler),
            };
            body = body.child(line(
                theme,
                title,
                vec![
                    widgets::badge(theme, hook.scope.label()),
                    if hook.effective {
                        widgets::badge_active(theme, "runs")
                    } else {
                        widgets::badge(theme, hook.reason.clone().unwrap_or_else(|| "off".into()))
                    },
                ],
            ));
        }

        let in_effect = view
            .commands
            .iter()
            .filter(|command| command.native.unwrap_or(command.configured_effective))
            .count();
        body = body.child(heading(
            theme,
            format!("Commands and skills · {in_effect} in effect"),
        ));
        if view.commands.is_empty() {
            body = body.child(note(theme, "No custom commands or skills found."));
        }
        for command in &view.commands {
            let kind = match command.kind {
                NativeCommandKind::Command => "command",
                NativeCommandKind::Skill => "skill",
            };
            let effective = command.native.unwrap_or(command.configured_effective);
            let state = if effective {
                widgets::badge_active(
                    theme,
                    if command.native == Some(true) {
                        "in effect · Claude lists it"
                    } else {
                        "in effect (configured)"
                    },
                )
            } else {
                widgets::badge(
                    theme,
                    command
                        .reason
                        .clone()
                        .unwrap_or_else(|| "not loaded".into()),
                )
            };
            body = body.child(line(
                theme,
                format!("/{}", command.name),
                vec![
                    widgets::badge(theme, format!("{} {kind}", command.scope.label())),
                    state,
                ],
            ));
        }
        for limitation in &view.limitations {
            body = body.child(note(theme, limitation.clone()).mt(px(10.0)));
        }
        body.into_any_element()
    }
}

impl Render for NativeExtensionsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let mut card = widgets::section_card(&theme)
            .mt(px(20.0))
            .p(px(16.0))
            .id("native-extensions")
            .role(gpui::Role::Group)
            .aria_label("Claude extensions in effect")
            .child(widgets::row_title(&theme, "Claude extensions in effect"))
            .child(widgets::page_subtitle(
                &theme,
                "What a Claude launch for the selected task or project loads: settings sources, output style, hooks, commands and skills, with scope and precedence. Ask and Auto launch in restricted mode.",
            ))
            .child(self.render_controls(&theme, cx));
        card = match &self.view {
            Loadable::Ready(view) => card.child(self.render_view(&theme, view)),
            Loadable::Error(error) => card.child(widgets::error_strip(&theme, error.clone())),
            _ => {
                card.child(note(&theme, "Reading Claude's settings for this launch…").mt(px(12.0)))
            }
        };
        card
    }
}
