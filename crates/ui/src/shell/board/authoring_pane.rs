//! Discuss with AI (A6): the temporary authoring conversation — composer and
//! message stream only (no title/description fields), an optional read-only
//! proposed-ticket summary, close → resumable Board draft, Discard. Driven by
//! `BoardApi::authoring_*`. Stub until A6 lands.
// Entry points are wired by the authoring slice.
#![allow(dead_code)]
use gpui::{Context, Entity, IntoElement, Render, Window, div};

use super::model::BoardModel;

/// The authoring pane for one temporary session.
pub struct AuthoringPane {
    pub(crate) model: Entity<BoardModel>,
    pub(crate) project_id: String,
    pub(crate) session_id: Option<String>,
}

impl AuthoringPane {
    pub fn new(model: Entity<BoardModel>, project_id: String, _cx: &mut Context<Self>) -> Self {
        Self {
            model,
            project_id,
            session_id: None,
        }
    }
}

impl Render for AuthoringPane {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
