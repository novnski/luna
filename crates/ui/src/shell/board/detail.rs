//! Ticket detail drawer (A9): editable brief, source issue, execution
//! history, outcome/evidence and actions, loaded with `BoardApi::read`.
//! Stub until A9 lands.
// Entry points are wired by the Board detail slice.
#![allow(dead_code)]
use gpui::{Context, Entity, IntoElement, Render, Window, div};

use super::model::BoardModel;

/// The drawer for one ticket.
pub struct BoardDetail {
    pub(crate) model: Entity<BoardModel>,
    pub(crate) work_item_id: String,
}

impl BoardDetail {
    pub fn new(model: Entity<BoardModel>, work_item_id: String, _cx: &mut Context<Self>) -> Self {
        Self {
            model,
            work_item_id,
        }
    }
}

impl Render for BoardDetail {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
