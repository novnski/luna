//! Settings → Providers advanced section: the Claude Code advanced-feature
//! ledger and Claude's own sessions outside LUNA (read-only).

use gpui::{AnyElement, Context, div, prelude::*};

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
        let refresh = super::section_action(
            theme,
            "refresh-claude-sessions",
            crate::icons::REFRESH,
            "Refresh Claude sessions outside LUNA",
            if loading { "Checking…" } else { "Refresh" },
            !loading,
        )
        .when(!loading, |button| {
            button.on_click(cx.listener(|page, _, _, cx| page.load_claude_sessions(cx)))
        })
        .into_any_element();
        let skeleton = loading.then(|| {
            crate::popover::skeleton_rows("claude-sessions-skeleton", theme, 2, cx.entity_id(), cx)
        });
        div()
            .flex()
            .flex_col()
            .child(claude_advanced::ledger_card(theme))
            .child(claude_advanced::external_sessions_section(
                theme,
                &self.claude_sessions,
                refresh,
                skeleton,
            ))
            .into_any_element()
    }
}
