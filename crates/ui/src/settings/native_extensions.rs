//! "Claude extensions in effect": what a Claude launch for the selected task
//! or project loads — settings sources, output style, hooks, commands and
//! skills — with scope and precedence, configured versus effective, and the
//! mode-dependent launch policy. Rows marked verified match Claude's own
//! startup answer for the same launch (`GetNativeExtensions`).

use gpui::{
    AnyElement, Context, Entity, IntoElement, Render, SharedString, Subscription, Task, Window,
    div, prelude::*,
};
use luna_proto::{
    HarnessId, NativeCommandKind, NativeExtensionsView, NativeSettingsScope, PermissionMode,
};
use luna_rpc::methods;

use crate::button::{self, ButtonKind, ControlSize};
use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;
use crate::typography::ui_rems;

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

/// One fact line in a row's list: the value (a path, a hook, a command)
/// with its badges, wrapping as a unit at narrow widths.
fn line(theme: &Theme, title: impl Into<SharedString>, badges: Vec<gpui::Div>) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_x(ui_rems(8.0))
        .gap_y(ui_rems(4.0))
        .child(
            div()
                .min_w_0()
                .text_size(ui_rems(12.0))
                .line_height(ui_rems(16.0))
                .text_color(theme.text)
                .child(title.into()),
        )
        .children(badges)
}

/// A row titled `title` with optional trailing badges, whose facts list
/// sits under its text.
fn list_row(
    theme: &Theme,
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    trailing: Vec<gpui::Div>,
    items: Vec<AnyElement>,
) -> gpui::Div {
    widgets::card_row(theme, false)
        .child(widgets::row_text(theme, title, description))
        .when(!trailing.is_empty(), |row| {
            row.child(widgets::row_controls().children(trailing))
        })
        .when(!items.is_empty(), |row| {
            row.child(
                widgets::row_block()
                    .flex_col()
                    .items_stretch()
                    .gap(ui_rems(6.0))
                    .children(items),
            )
        })
}

fn note(theme: &Theme, text: impl Into<SharedString>) -> AnyElement {
    widgets::row_description(theme, text).into_any_element()
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

    /// The launch-mode preview: one segmented control (audit DS-16).
    fn render_mode_row(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let busy = self.is_loading();
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
        widgets::card_row(theme, false)
            .child(widgets::row_text(
                theme,
                "Launch mode",
                Some(
                    "Preview a launch in another permission mode. Task mode follows the selected \
                     task's own mode."
                        .into(),
                ),
            ))
            .child(
                widgets::row_controls().child(button::segmented(
                    "native-ext-mode",
                    modes
                        .iter()
                        .map(|(key, label, _)| button::Segment::new(*key, *label))
                        .collect(),
                    selected,
                    328.0,
                    ControlSize::Md,
                    theme,
                    cx.listener(move |panel, ix: &usize, _, cx| {
                        if !busy && let Some((_, _, mode)) = modes.get(*ix) {
                            panel.set_mode(*mode, cx);
                        }
                    }),
                )),
            )
    }

    /// The loaded view as rows of the card: the launch, then each kind of
    /// extension with its facts listed under the row's text.
    fn render_rows(&self, theme: &Theme, view: &NativeExtensionsView) -> Vec<AnyElement> {
        let mut rows: Vec<AnyElement> = Vec::new();
        let verified = view.native_verified;
        rows.push(
            widgets::card_row(theme, false)
                .child(widgets::row_text(
                    theme,
                    format!("{} launch", mode_label(view.permission_mode)),
                    Some(view.launch_policy.clone().into()),
                ))
                .child(widgets::row_controls().child(if verified {
                    widgets::badge_active(theme, "Verified with Claude")
                } else {
                    widgets::badge(theme, "Not verified")
                }))
                .when_some(view.native_error.clone(), |row, error| {
                    row.child(
                        widgets::row_block().child(
                            widgets::warning_strip(
                                theme,
                                format!(
                                    "Claude's own readback failed: {error}. Rows below are \
                                     configured, not verified."
                                ),
                            )
                            .mt_0()
                            .w_full(),
                        ),
                    )
                })
                .into_any_element(),
        );

        let sources = view
            .sources
            .iter()
            .map(|source| {
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
                line(theme, source.display_path.clone(), badges).into_any_element()
            })
            .collect();
        rows.push(
            list_row(
                theme,
                "Settings sources",
                Some("Highest precedence first.".into()),
                Vec::new(),
                sources,
            )
            .into_any_element(),
        );

        let style = &view.output_style;
        let mut style_badges = vec![widgets::badge(
            theme,
            style
                .configured_scope
                .map(NativeSettingsScope::label)
                .unwrap_or("Built-in"),
        )];
        match &style.native {
            Some(native) if *native == style.configured_effective => {
                style_badges.push(widgets::badge_active(theme, "Claude agrees"))
            }
            Some(native) => {
                style_badges.push(widgets::badge(theme, format!("Claude reports {native}")))
            }
            None => style_badges.push(widgets::badge(theme, "unverified")),
        }
        let mut style_notes: Vec<AnyElement> = style
            .configured
            .iter()
            .filter(|value| !value.applied)
            .map(|value| {
                note(
                    theme,
                    format!(
                        "{}: {} — {}",
                        value.scope.label(),
                        value.value,
                        value.reason.clone().unwrap_or_else(|| "not applied".into())
                    ),
                )
            })
            .collect();
        let available = style
            .available
            .iter()
            .map(|option| match option.scope {
                NativeSettingsScope::BuiltIn => option.name.clone(),
                scope => format!("{} ({})", option.name, scope.label()),
            })
            .collect::<Vec<_>>()
            .join(", ");
        style_notes.push(note(theme, format!("Available: {available}")));
        rows.push(
            list_row(
                theme,
                "Output style",
                Some(
                    format!(
                        "In effect: {}",
                        style
                            .native
                            .as_deref()
                            .unwrap_or(&style.configured_effective)
                    )
                    .into(),
                ),
                style_badges,
                style_notes,
            )
            .into_any_element(),
        );

        let effective_hooks = view.hooks.iter().filter(|hook| hook.effective).count();
        let hooks: Vec<AnyElement> = if view.hooks.is_empty() {
            vec![note(theme, "No hooks configured in the scanned settings.")]
        } else {
            view.hooks
                .iter()
                .map(|hook| {
                    let title = match &hook.matcher {
                        Some(matcher) => format!("{} ({matcher}) · {}", hook.event, hook.handler),
                        None => format!("{} · {}", hook.event, hook.handler),
                    };
                    line(
                        theme,
                        title,
                        vec![
                            widgets::badge(theme, hook.scope.label()),
                            if hook.effective {
                                widgets::badge_active(theme, "runs")
                            } else {
                                widgets::badge(
                                    theme,
                                    hook.reason.clone().unwrap_or_else(|| "off".into()),
                                )
                            },
                        ],
                    )
                    .into_any_element()
                })
                .collect()
        };
        rows.push(
            list_row(
                theme,
                "Hooks",
                Some(format!("{effective_hooks} of {} run", view.hooks.len()).into()),
                Vec::new(),
                hooks,
            )
            .into_any_element(),
        );

        let in_effect = view
            .commands
            .iter()
            .filter(|command| command.native.unwrap_or(command.configured_effective))
            .count();
        let commands: Vec<AnyElement> = if view.commands.is_empty() {
            vec![note(theme, "No custom commands or skills found.")]
        } else {
            view.commands
                .iter()
                .map(|command| {
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
                    line(
                        theme,
                        format!("/{}", command.name),
                        vec![
                            widgets::badge(theme, format!("{} {kind}", command.scope.label())),
                            state,
                        ],
                    )
                    .into_any_element()
                })
                .collect()
        };
        rows.push(
            list_row(
                theme,
                "Commands and skills",
                Some(format!("{in_effect} in effect").into()),
                Vec::new(),
                commands,
            )
            .into_any_element(),
        );
        rows
    }
}

impl Render for NativeExtensionsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let busy = self.is_loading();
        let recheck = button::button(
            "native-ext-refresh",
            ButtonKind::Secondary,
            ControlSize::Md,
            !busy,
            &theme,
        )
        .aria_label("Re-check extensions with Claude")
        .child(button::glyph(crate::icons::REFRESH, ControlSize::Md))
        .child(if busy {
            "Checking…"
        } else {
            "Re-check with Claude"
        })
        .when(!busy, |el| {
            el.on_click(cx.listener(|panel, _, _, cx| panel.load(true, cx)))
        });
        let mut card = widgets::section_card(&theme)
            .id("native-extensions")
            .role(gpui::Role::Group)
            .aria_label("Claude extensions in effect")
            .child(widgets::section_header(
                &theme,
                "Claude extensions in effect",
                Some(
                    "What a Claude launch for the selected task or project loads: settings \
                     sources, output style, hooks, commands and skills, with scope and \
                     precedence. Ask and Auto launch in restricted mode."
                        .into(),
                ),
                vec![recheck.into_any_element()],
            ))
            .child(self.render_mode_row(&theme, cx));
        let mut limitations = Vec::new();
        match &self.view {
            Loadable::Ready(view) => {
                card = card.children(self.render_rows(&theme, view));
                limitations = view.limitations.clone();
            }
            Loadable::Error(error) => {
                card = card.child(
                    widgets::card_row(&theme, false).child(
                        div()
                            .w_full()
                            .min_w_0()
                            .child(widgets::error_strip(&theme, error.clone()).mt_0()),
                    ),
                );
            }
            _ => {
                card = card.child(widgets::card_row(&theme, false).child(
                    div().w_full().min_w_0().child(popover::skeleton_rows(
                        "native-extensions-skeleton",
                        &theme,
                        4,
                        cx.entity_id(),
                        cx,
                    )),
                ));
            }
        }
        // Limitations are fine print under the card (spec §2).
        div().flex().flex_col().child(card).children(
            limitations
                .into_iter()
                .map(|limitation| widgets::section_footer(&theme, limitation)),
        )
    }
}
