//! From GitHub issue (A7): repository/account selection, search with
//! freshness, preview with selected comments and truncation notes, Add to
//! board / Start session, Open card for existing links. Driven by
//! `BoardApi::issue_*`. Stub until A7 lands.
// Entry points are wired by the GitHub slice.
#![allow(dead_code)]
use gpui::{Context, Entity, IntoElement, Render, Window, div};

use super::model::BoardModel;

/// The issue picker for one project.
pub struct GitHubIssuePicker {
    pub(crate) model: Entity<BoardModel>,
    pub(crate) project_id: String,
}

impl GitHubIssuePicker {
    pub fn new(model: Entity<BoardModel>, project_id: String, _cx: &mut Context<Self>) -> Self {
        Self { model, project_id }
    }
}

impl Render for GitHubIssuePicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
