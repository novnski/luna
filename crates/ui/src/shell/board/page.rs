//! The Board page (owned by the Board page UI slice, A8). This foundation
//! version shows the scope, the engine status and the three columns so the
//! route is never blank; A8 adds the toolbar, filters, Board/List switch,
//! drag/keyboard moves and persisted presentation.
use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, ScrollHandle, SharedString,
    Styled, Subscription, Window, div, prelude::*,
};
use luna_proto::BoardScope;

use super::detail::BoardSurfaces;
use super::model::{BoardModel, BoardStatus};
use crate::button::{self, ButtonKind, ControlSize};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

pub struct BoardPage {
    state: Entity<AppState>,
    model: Entity<BoardModel>,
    scroll: ScrollHandle,
    /// The detail drawer / New ticket overlay (Board detail slice, A9).
    surfaces: Entity<BoardSurfaces>,
    _model: Subscription,
}

impl BoardPage {
    pub fn new(state: Entity<AppState>, scope: BoardScope, cx: &mut Context<Self>) -> Self {
        let model = cx.new(|cx| BoardModel::new(state.clone(), scope, cx));
        let observer = cx.observe(&model, |_, _, cx| cx.notify());
        let surfaces = cx.new(|cx| BoardSurfaces::new(state.clone(), model.clone(), cx));
        Self {
            state,
            model,
            scroll: ScrollHandle::new(),
            surfaces,
            _model: observer,
        }
    }

    /// The overlay host (detail drawer, New ticket chooser and forms).
    pub fn surfaces(&self) -> &Entity<BoardSurfaces> {
        &self.surfaces
    }

    /// Open one ticket's detail drawer (card click / Enter on a card).
    // Called by the Board page slice's cards (A8).
    #[allow(dead_code)]
    pub fn open_detail(
        &mut self,
        work_item_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.surfaces.update(cx, |surfaces, cx| {
            surfaces.open_detail(work_item_id, window, cx)
        });
    }

    /// Open the New ticket chooser for the current project scope (All
    /// projects asks for the project explicitly).
    pub fn open_new_ticket(
        &mut self,
        project_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.surfaces.update(cx, |surfaces, cx| {
            surfaces.open_new_ticket(project_id, window, cx)
        });
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
        let theme = Theme::of(cx).clone();
        let scope = self.scope_label(cx);
        let notice = self.status_notice(&theme, cx);
        let scope_project = self.model.read(cx).scope().project_id().map(str::to_owned);
        let new_ticket = button::button(
            "board-new-ticket",
            ButtonKind::Primary,
            ControlSize::Md,
            true,
            &theme,
        )
        .aria_label("New ticket")
        .child(button::glyph(crate::icons::PLUS, ControlSize::Md).text_color(theme.on_solid))
        .child("New ticket")
        .on_click(cx.listener(move |page, _, window, cx| {
            page.open_new_ticket(scope_project.clone(), window, cx)
        }))
        .into_any_element();
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
        let page = div()
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
                                vec![new_ticket],
                            ))
                            .child(widgets::page_subtitle(&theme, SharedString::from(scope))),
                    )
                    .children(notice)
                    .child(body),
            );
        // The overlay sits outside the scroller so the drawer never scrolls
        // with the board.
        div()
            .id("board-page-frame")
            .relative()
            .size_full()
            .child(page)
            .child(self.surfaces.clone())
    }
}
