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
    /// The scope a newly opened Board starts with: the sidebar's explicit
    /// project filter, else the last Board scope, else All projects. A
    /// project that no longer exists falls back to All projects.
    fn initial_board_scope(&self, cx: &App) -> luna_proto::BoardScope {
        let state = self.state.read(cx);
        self.settings
            .space_filter
            .clone()
            .or_else(|| self.settings.board.project_id.clone())
            .filter(|project_id| state.space_row(project_id).is_some())
            .map(|project_id| luna_proto::BoardScope::Project { project_id })
            .unwrap_or_default()
    }

    /// The Board page entity, created on first open. An explicit change of
    /// the sidebar's project filter carries over to an existing page (the
    /// Board opened from that project); otherwise it keeps its own scope.
    pub(super) fn board_page(&mut self, cx: &mut Context<Self>) -> Entity<BoardPage> {
        let filter = self.settings.space_filter.clone();
        if let Some(page) = &self.board_page {
            let page = page.clone();
            page.update(cx, |page, cx| page.follow_sidebar_filter(filter, cx));
            return page;
        }
        let scope = self.initial_board_scope(cx);
        let state = self.state.clone();
        let page = cx.new(|cx| {
            let mut page = BoardPage::new(state, scope, cx);
            page.note_sidebar_filter(filter);
            page
        });
        // Lives as long as the page, which the Shell keeps for its lifetime.
        cx.subscribe(&page, |shell, _, event: &BoardPageEvent, cx| {
            shell.on_board_page_event(event, cx)
        })
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
            BoardPageEvent::OpenChat(target) => self.open_board_chat(target, cx),
        }
    }

    /// Go to chat from a ticket: reopen the view, never relaunch or reparent.
    /// A side worker opens in its main parent's workbench.
    pub(super) fn open_board_chat(
        &mut self,
        target: &luna_proto::BoardNavigationTarget,
        cx: &mut Context<Self>,
    ) {
        use luna_proto::BoardNavigationTarget as Target;
        match target {
            Target::MainChat { task_id } | Target::Direct { task_id, .. } => {
                self.open_chat(task_id.clone(), cx)
            }
            Target::SideChat {
                main_task_id,
                side_task_id,
            } => {
                self.open_chat(main_task_id.clone(), cx);
                self.open_side_chat_in_pane(side_task_id.clone(), cx);
            }
            Target::NoChat { .. } => {}
        }
    }

    /// Open the Board prefiltered to a project (a project shortcut), or to
    /// All projects with `None`.
    #[allow(dead_code)]
    pub(crate) fn open_board_for_project(
        &mut self,
        project_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let page = self.board_page(cx);
        let scope = project_id
            .map(|project_id| luna_proto::BoardScope::Project { project_id })
            .unwrap_or_default();
        page.update(cx, |page, cx| page.set_scope(scope, cx));
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
