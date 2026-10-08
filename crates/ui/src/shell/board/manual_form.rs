//! Write manually (A9): title, description and the implementation/review
//! settings controls; Save to Backlog calls `BoardApi::create` (no chat).
//! Also hosts the New issue chooser (Write manually / Discuss with AI /
//! From GitHub issue). Stub until A9 lands.
// Entry points are wired by the Board detail slice.
#![allow(dead_code)]
use gpui::{Context, Entity, IntoElement, Render, Window, div};

use super::model::BoardModel;

/// The manual ticket form for one project.
pub struct ManualTicketForm {
    pub(crate) model: Entity<BoardModel>,
    pub(crate) project_id: Option<String>,
}

impl ManualTicketForm {
    pub fn new(
        model: Entity<BoardModel>,
        project_id: Option<String>,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self { model, project_id }
    }
}

impl Render for ManualTicketForm {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
