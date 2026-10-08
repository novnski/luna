//! The Board page (owned by the Board page UI slice, A8). This foundation
//! version shows the scope, the engine status and the three columns so the
//! route is never blank; A8 adds the toolbar, filters, Board/List switch,
//! drag/keyboard moves and persisted presentation.
use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, ScrollHandle, SharedString,
    Styled, Subscription, Window, div, prelude::*,
};
use luna_proto::BoardScope;

use super::model::{BoardModel, BoardStatus};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

pub struct BoardPage {
    state: Entity<AppState>,
    model: Entity<BoardModel>,
    scroll: ScrollHandle,
    _model: Subscription,
    /// Discuss with AI (A6): the open temporary discussion, if any.
    pub(super) authoring: Option<super::authoring_pane::AuthoringMount>,
    /// Resumable Board drafts and the Discuss with AI entry (A6).
    pub(super) drafts: Entity<super::authoring_pane::AuthoringDrafts>,
}

impl BoardPage {
    pub fn new(state: Entity<AppState>, scope: BoardScope, cx: &mut Context<Self>) -> Self {
        let model = cx.new(|cx| BoardModel::new(state.clone(), scope, cx));
        let observer = cx.observe(&model, |_, _, cx| cx.notify());
        let page = cx.entity().downgrade();
        let drafts =
            cx.new(|cx| super::authoring_pane::AuthoringDrafts::new(model.clone(), page, cx));
        Self {
            state,
            model,
            scroll: ScrollHandle::new(),
            _model: observer,
            authoring: None,
            drafts,
        }
    }

    /// The data layer shared by every Board surface on this page.
    pub fn model(&self) -> &Entity<BoardModel> {
        &self.model
    }

    fn scope_label(&self, cx: &Context<Self>) -> String {
        match self.model.read(cx).scope() {
            BoardScope::AllProjects => "All projects".into(),
            BoardScope::Project { project_id } => self
                .state
                .read(cx)
                .space_row(project_id)
                .map(|space| space.display_name().to_owned())
                .unwrap_or_else(|| "Project".into()),
        }
    }

    fn status_notice(&self, theme: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let text = match self.model.read(cx).status() {
            BoardStatus::Live => return None,
            BoardStatus::Loading => "Loading tickets…".to_owned(),
            BoardStatus::Disconnected(reason) | BoardStatus::Error(reason) => reason.clone(),
            BoardStatus::Unsupported => "This host does not support the Board yet.".into(),
        };
        Some(
            widgets::row_description(theme, text)
                .id("board-status")
                .into_any_element(),
        )
    }
}

impl Render for BoardPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(mount) = &self.authoring {
            // Discuss with AI replaces the board body while it is open.
            return div()
                .id("board-page")
                .size_full()
                .child(mount.pane.clone())
                .into_any_element();
        }
        let theme = Theme::of(cx).clone();
        let scope = self.scope_label(cx);
        let notice = self.status_notice(&theme, cx);
        let model = self.model.read(cx);
        let count = model.items().len();
        let body = if count == 0 && matches!(model.status(), BoardStatus::Live) {
            widgets::page_subtitle(
                &theme,
                "No tickets yet. Save a ticket to plan work without starting an agent; \
                 moving it to Ready starts it.",
            )
            .id("board-empty")
            .into_any_element()
        } else {
            super::columns::render_columns(model, &theme)
        };
        div()
            .id("board-page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .w_full()
                    .px(crate::typography::ui_rems(24.0))
                    .pt(crate::typography::ui_rems(28.0))
                    .pb(crate::typography::ui_rems(48.0))
                    .flex()
                    .flex_col()
                    .gap(crate::typography::ui_rems(16.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(widgets::page_title_bar(
                                &theme,
                                "Board",
                                Some(count),
                                vec![],
                            ))
                            .child(widgets::page_subtitle(&theme, SharedString::from(scope))),
                    )
                    .children(notice)
                    .child(self.drafts.clone())
                    .child(body),
            )
            .into_any_element()
    }
}
