//! Settings -> Setup: the minimal local L03 Setup Doctor. It renders the
//! engine-owned snapshot also consumed by the new-session first-run hint and
//! routes explicit recovery to the existing Agents / Accounts surfaces.

use gpui::{
    AnyElement, Context, Entity, EventEmitter, IntoElement, Render, SharedString, Task, Window,
    div, prelude::*, px,
};
use zeron_engine::setup_health::{
    EvidenceFreshness, ProviderReadiness, ProviderReadinessState, SetupCapabilityKind,
    SetupCapabilityState, SetupHealthSnapshot, SetupOverall, SetupRemediationKind,
    WorkspaceReadinessState,
};
use zeron_rpc::methods;

use crate::icons::{self, icon};
use crate::pickers::harness_brand_icon;
use crate::popover::{self, Loadable};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

#[derive(Debug, Clone, Copy)]
pub enum SetupEvent {
    Continue,
    OpenAgents,
    OpenAccounts,
}

pub struct SetupPage {
    state: Entity<AppState>,
    snapshot: Loadable<SetupHealthSnapshot>,
    load_task: Option<Task<()>>,
    scroll: widgets::PageScroll,
}

impl EventEmitter<SetupEvent> for SetupPage {}

impl SetupPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut page = Self {
            state,
            snapshot: Loadable::Idle,
            load_task: None,
            scroll: widgets::PageScroll::default(),
        };
        page.load(cx);
        page
    }

    fn local_workspace_path(&self, cx: &Context<Self>) -> Option<String> {
        let state = self.state.read(cx);
        let local = state
            .engine()
            .map(|engine| engine.engine_info().device_id.as_str())
            .or(state.local_device_id.as_deref());
        state
            .selected_space_row()
            .filter(|space| Some(space.device_id.as_str()) == local)
            .map(|space| space.path.clone())
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.snapshot = Loadable::Error("The local engine is not connected.".into());
            cx.notify();
            return;
        };
        let workspace_path = self.local_workspace_path(cx);
        self.snapshot = Loadable::Loading;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::SETUP_HEALTH,
                    serde_json::json!({ "workspacePath": workspace_path }),
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<SetupHealthSnapshot>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |page, cx| {
                page.snapshot = match result {
                    Ok(snapshot) => Loadable::Ready(snapshot),
                    Err(error) => Loadable::Error(error),
                };
                page.load_task = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn emit_remediation(&mut self, kind: SetupRemediationKind, cx: &mut Context<Self>) {
        match kind {
            SetupRemediationKind::Refresh => self.load(cx),
            SetupRemediationKind::OpenAgents => cx.emit(SetupEvent::OpenAgents),
            SetupRemediationKind::OpenAccounts => cx.emit(SetupEvent::OpenAccounts),
            SetupRemediationKind::SelectProject => cx.emit(SetupEvent::Continue),
        }
    }

    fn action(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        widgets::ghost_action(theme)
            .id(id)
            .role(gpui::Role::Button)
            .tab_index(0)
            .focus_visible({
                let accent = theme.accent;
                move |style| style.border_2().border_color(accent)
            })
            .hover(|style| widgets::ghost_hover(theme, style))
            .child(label.into())
    }

    fn render_workspace(
        &self,
        snapshot: &SetupHealthSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ready = snapshot.workspace.state == WorkspaceReadinessState::Available;
        let label = if ready { "Available" } else { "Needs action" };
        let path = snapshot
            .workspace
            .canonical_path
            .as_deref()
            .unwrap_or("No local project selected");
        let remediation = snapshot.workspace.remediation.clone();
        widgets::section_card(theme)
            .child(
                widgets::card_row(theme, true)
                    .child(widgets::row_tile(theme, icons::FOLDER))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(theme, "Local project"))
                            .child(widgets::meta_line(
                                theme,
                                vec![
                                    div().child(path.to_string()).into_any_element(),
                                    div()
                                        .child(snapshot.workspace.detail.clone())
                                        .into_any_element(),
                                ],
                            )),
                    )
                    .child(if ready {
                        widgets::badge_active(theme, label)
                    } else {
                        widgets::badge(theme, label)
                    })
                    .when_some(remediation, |row, remediation| {
                        let label = remediation.label.clone();
                        let kind = remediation.kind;
                        row.child(
                            self.action("setup-workspace-action", label, theme)
                                .on_click(cx.listener(move |page, _, _, cx| {
                                    page.emit_remediation(kind, cx)
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn capability_summary(provider: &ProviderReadiness) -> String {
        let label = |kind: SetupCapabilityKind| {
            provider
                .capabilities
                .iter()
                .find(|capability| capability.kind == kind)
                .map(|capability| match capability.state {
                    SetupCapabilityState::Supported => "supported",
                    SetupCapabilityState::Unsupported => "unsupported",
                    SetupCapabilityState::Unknown => "unknown",
                })
                .unwrap_or("unknown")
        };
        format!(
            "{} models · reasoning {} · steering {} · resume {} · fork {}",
            provider.model_count,
            label(SetupCapabilityKind::Reasoning),
            label(SetupCapabilityKind::Steering),
            label(SetupCapabilityKind::Resume),
            label(SetupCapabilityKind::Fork),
        )
    }

    fn render_provider_row(
        &self,
        provider: &ProviderReadiness,
        index: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let available = provider.is_fresh_available();
        let (icon_path, tint) = harness_brand_icon(provider.harness);
        let badge = match (provider.state, provider.freshness) {
            (_, EvidenceFreshness::Stale) => "Stale",
            (ProviderReadinessState::Available, _) => "Available",
            (ProviderReadinessState::Authenticated, _) => "Authenticated",
            (ProviderReadinessState::Configured, _) => "Configured",
            (ProviderReadinessState::Installed, _) => "Installed",
            (ProviderReadinessState::NotInstalled, _) => "Not installed",
            (ProviderReadinessState::Unsupported, _) => "Unsupported",
            (ProviderReadinessState::Unknown, _) => "Unknown",
        };
        let tile = div()
            .flex_none()
            .size(px(36.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(theme.border)
            .bg(crate::theme::ink(0.03))
            .flex()
            .items_center()
            .justify_center()
            .child(
                icon(icon_path)
                    .size(px(16.0))
                    .text_color(tint.unwrap_or(theme.text_muted)),
            );
        let remediation = provider.remediation.clone();
        widgets::card_row(theme, index == 0)
            .id(("setup-provider-row", index))
            .when(
                !provider.enabled || provider.state == ProviderReadinessState::NotInstalled,
                |row| row.opacity(0.65),
            )
            .child(tile)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(widgets::row_title(theme, provider.name.clone()))
                    .child(widgets::meta_line(
                        theme,
                        vec![
                            div().child(provider.detail.clone()).into_any_element(),
                            div()
                                .child(Self::capability_summary(provider))
                                .into_any_element(),
                        ],
                    )),
            )
            .child(if available {
                widgets::badge_active(theme, badge)
            } else {
                widgets::badge(theme, badge)
            })
            .when_some(remediation, |row, remediation| {
                let label = remediation.label.clone();
                let kind = remediation.kind;
                row.child(
                    self.action(("setup-provider-action", index), label, theme)
                        .on_click(
                            cx.listener(move |page, _, _, cx| page.emit_remediation(kind, cx)),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_ready(
        &self,
        snapshot: &SetupHealthSnapshot,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let summary = match snapshot.overall {
            SetupOverall::Ready => "Ready to run locally",
            SetupOverall::NeedsAction => "Setup needs action",
            SetupOverall::Degraded => "Using stale readiness evidence",
            SetupOverall::Unavailable => "Readiness could not be verified",
        };
        let workspace = self.render_workspace(snapshot, theme, cx);
        let providers = snapshot
            .providers
            .iter()
            .enumerate()
            .map(|(index, provider)| self.render_provider_row(provider, index, theme, cx))
            .collect::<Vec<_>>();
        let provider_card = if providers.is_empty() {
            widgets::warning_strip(
                theme,
                "No provider evidence is available. Check again before changing any setup.",
            )
            .into_any_element()
        } else {
            widgets::section_card(theme)
                .children(providers)
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(if snapshot.overall == SetupOverall::Ready {
                        widgets::badge_active(theme, summary)
                    } else {
                        widgets::badge(theme, summary)
                    })
                    .child(
                        self.action("setup-refresh", "Check again", theme)
                            .on_click(cx.listener(|page, _, _, cx| page.load(cx))),
                    ),
            )
            .child(
                div()
                    .child(widgets::field_label(theme, "Workspace"))
                    .child(workspace),
            )
            .child(
                div()
                    .child(widgets::field_label(theme, "Providers"))
                    .child(provider_card),
            )
            .child(
                div().flex().justify_end().child(
                    self.action("setup-continue", "Continue in composer", theme)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SetupEvent::Continue))),
                ),
            )
            .into_any_element()
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl popover::ScrollRailHost for SetupPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for SetupPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let body = match &self.snapshot {
            Loadable::Idle | Loadable::Loading => widgets::section_card(&theme)
                .p(px(16.0))
                .child(popover::skeleton_rows(
                    "setup-skeleton",
                    &theme,
                    4,
                    cx.entity_id(),
                    cx,
                ))
                .into_any_element(),
            Loadable::Error(message) => div()
                .child(widgets::error_strip(&theme, message.clone()))
                .child(
                    self.action("setup-retry", "Retry", &theme)
                        .on_click(cx.listener(|page, _, _, cx| page.load(cx))),
                )
                .into_any_element(),
            Loadable::Ready(snapshot) => self.render_ready(snapshot, &theme, cx),
        };
        let scrollbar = popover::rail(self, "setup-page-scrollbar", &theme, cx);
        div()
            .id("setup-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("setup-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_header(&theme, "Setup", None))
                            .child(
                                widgets::page_subtitle(
                                    &theme,
                                    "Local readiness from the same engine evidence used by the composer. Checks never install, sign in, or change provider settings automatically.",
                                )
                                .max_w(px(600.0))
                                .line_height(px(20.0)),
                            )
                            .child(body),
                    ),
            )
            .children(scrollbar)
    }
}
