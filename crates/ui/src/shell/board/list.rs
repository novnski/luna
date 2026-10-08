//! List view (A8): the narrow-window alternative to columns with the same
//! actions, ordering and selection. Stub until A8 lands.
// Entry points are wired by the Board page slice.
#![allow(dead_code)]
use gpui::{AnyElement, IntoElement, div};

use super::model::BoardModel;
use crate::theme::Theme;

/// Render the List view for the model's current items.
pub(crate) fn render_list(model: &BoardModel, theme: &Theme) -> AnyElement {
    let _ = (model, theme);
    div().into_any_element()
}
