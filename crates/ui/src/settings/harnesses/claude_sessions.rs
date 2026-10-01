//! Settings → Providers advanced section: the Claude Code advanced-feature
//! ledger and Claude's own sessions outside LUNA (read-only).

use gpui::{AnyElement, Context, div, prelude::*, px};

use luna_proto::ClaudeExternalSessionsView;
use luna_rpc::methods;

use super::HarnessesPage;
use crate::claude_advanced::{self, ExternalSessionsState};
use crate::theme::Theme;

impl HarnessesPage {
    /// `claude agents --json` through the engine on this device. Listing has
    /// no side effects (verified: it does not start Claude's supervisor).
    pub(super) fn load_claude_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.claude_sessions = ExternalSessionsState::Loading;
        self.claude_sessions_task = Some(cx.spawn(async move |page, cx| {
            let result = engine
                .client()
                .call(
                    methods::LIST_CLAUDE_EXTERNAL_SESSIONS,
                    serde_json::json!({}),
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<ClaudeExternalSessionsView>(value)
                        .map_err(|error| error.to_string())
                });
            page.update(cx, |page, cx| {
                page.claude_sessions = match result {
                    Ok(view) => ExternalSessionsState::Ready(view),
                    Err(error) => ExternalSessionsState::Error(error),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn render_claude_advanced(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let loading = matches!(self.claude_sessions, ExternalSessionsState::Loading);
        let accent = theme.accent;
        let wash = theme.surface_raised;
        let refresh = gpui_base::Button::new("refresh-claude-sessions")
            .role(gpui::Role::Button)
            .accessibility_id("refresh-claude-sessions")
            .accessibility_label("Refresh Claude sessions outside LUNA")
            .disabled(loading)
            .tab_index(0)
            .tab_stop(!loading)
            .flex_none()
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(8.0))
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text_muted)
            .when(!loading, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(wash))
                    .focus_visible(move |style| style.bg(accent.opacity(0.18)).text_color(accent))
            })
            .child(if loading { "Checking…" } else { "Refresh" })
            .on_click(cx.listener(|page, _, _, cx| page.load_claude_sessions(cx)))
            .into_any_element();
        div()
            .flex()
            .flex_col()
            .child(claude_advanced::ledger_card(theme))
            .child(claude_advanced::external_sessions_card(
                theme,
                &self.claude_sessions,
                refresh,
            ))
            .into_any_element()
    }
}
