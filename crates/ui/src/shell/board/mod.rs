//! The Board page (sidebar: New thread → Automations → Board) and the board
//! surfaces in chats. Contract: docs/design/project-board/pb0-contract.md §9.
//!
//! Foundation (A1): `api.rs` (typed calls for every `Board*` method),
//! `model.rs` (the data layer entity) and the minimal page/columns/card so
//! the route is never blank. Every other file is a compiling stub owned by a
//! UI slice: page/columns/card/list/filters (A8), detail/manual_form/
//! settings_controls/evidence (A9), authoring_pane (A6), github_picker (A7),
//! chat_badges (A10).
use super::*;

pub(crate) mod api;
pub(crate) mod authoring_pane;
pub(crate) mod card;
pub(crate) mod chat_badges;
pub(crate) mod columns;
pub(crate) mod detail;
pub(crate) mod evidence;
pub(crate) mod filters;
pub(crate) mod github_picker;
/// PB-4A feature groups panel.
pub(crate) mod groups;
pub(crate) mod list;
pub(crate) mod manual_form;
pub(crate) mod model;
pub(crate) mod page;
pub(crate) mod settings_controls;

// The data-layer surface the UI slices (A6-A10) build on.
#[allow(unused_imports)]
pub use api::{BoardApi, BoardCallError};
#[allow(unused_imports)]
pub use model::{BoardListFilters, BoardModel, BoardModelEvent, BoardStatus};
pub use page::{BoardPage, BoardPageEvent};

impl Shell {
    /// The Board page entity, created on first open. Its scope is always
    /// the sidebar's project filter (one project, or All projects): every
    /// fetch hands the current filter over, so a change made anywhere
    /// applies before the Board draws, and overlays for a project it no
    /// longer shows close.
    pub(super) fn board_page(&mut self, cx: &mut Context<Self>) -> Entity<BoardPage> {
        let filter = self.settings.space_filter.clone();
        if let Some(page) = &self.board_page {
            let page = page.clone();
            if page.update(cx, |page, cx| page.follow_sidebar_filter(filter, cx)) {
                let (scope, open_detail) = {
                    let page = page.read(cx);
                    (
                        page.model().read(cx).scope().clone(),
                        page.surfaces().read(cx).detail_work_item_id(cx),
                    )
                };
                self.board_github_follow_scope(&scope, open_detail, cx);
            }
            return page;
        }
        let scope = page::scope_for_filter(filter);
        let state = self.state.clone();
        let page = cx.new(|cx| BoardPage::new(state, scope, cx));
        // Both live as long as the page, which the Shell keeps for its
        // lifetime: the page's own events (prefs, Go to chat, the GitHub
        // picker) and its overlay host's (A9: Go to chat, Needs attention,
        // writeback).
        cx.subscribe(&page, |shell, _, event: &BoardPageEvent, cx| {
            shell.on_board_page_event(event, cx)
        })
        .detach();
        let surfaces = page.read(cx).surfaces().clone();
        cx.subscribe(
            &surfaces,
            |shell, _, event: &detail::BoardSurfacesEvent, cx| {
                shell.on_board_surface_event(event, cx)
            },
        )
        .detach();
        page.update(cx, |page, cx| page.sync_prefs(cx));
        self.board_page = Some(page.clone());
        page
    }

    fn on_board_page_event(&mut self, event: &BoardPageEvent, cx: &mut Context<Self>) {
        match event {
            BoardPageEvent::PrefsChanged(prefs) => {
                if self.settings.board != *prefs {
                    self.settings.board = prefs.clone();
                    self.schedule_save(cx);
                }
            }
            BoardPageEvent::GoToChat(request) => self.board_go_to_chat(request.clone(), cx),
            BoardPageEvent::OpenGitHubPicker { project_id } => {
                self.open_board_github_picker(project_id.clone(), cx)
            }
        }
    }

    /// Open the Board for a project (a project shortcut), or for All
    /// projects with `None`: the sidebar's filter changes with it, since the
    /// Board always shows the sidebar's project.
    #[allow(dead_code)]
    pub(crate) fn open_board_for_project(
        &mut self,
        project_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.settings.space_filter != project_id {
            self.set_space_filter(project_id, cx);
        }
        self.board_page(cx);
        self.open_sidebar_page(SidebarPage::Board, cx);
    }

    /// Keep the persisted sidebar page in step with the route, so a restart
    /// reopens the page that was open (and a chat route clears it).
    pub(super) fn sync_restored_page(&mut self, cx: &mut Context<Self>) {
        let page = match self.route {
            Route::Page(page) => Some(page.key()),
            Route::Chat | Route::Settings(_) => None,
        };
        if self.settings.last_sidebar_page.as_deref() != page {
            self.settings.last_sidebar_page = page.map(str::to_owned);
            self.schedule_save(cx);
        }
    }

    /// QA: the mounted Board page.
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_board_page(&self) -> Option<Entity<BoardPage>> {
        self.board_page.clone()
    }

    /// QA: the Board page's readback (`Value::Null` before it mounts).
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_board_readback(&self, window: &Window, cx: &App) -> serde_json::Value {
        self.board_page
            .as_ref()
            .map(|page| page.read(cx).fixture_readback(window, cx))
            .unwrap_or(serde_json::Value::Null)
    }
}
