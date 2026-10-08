//! Toolbar and filters (A8): project selector (including All projects),
//! Board/List switch, New issue, From GitHub issue, search, attention /
//! provider / source filters. Filter state lives in `BoardListFilters` and
//! `UiSettings::board`; the engine applies it. Stub until A8 lands.
// Entry points are wired by the Board page slice.
#![allow(dead_code)]
use gpui::{AnyElement, IntoElement, div};

use super::model::BoardModel;
use crate::theme::Theme;

/// Render the Board toolbar for the model's scope and filters.
pub(crate) fn render_toolbar(model: &BoardModel, theme: &Theme) -> AnyElement {
    let _ = (model, theme);
    div().into_any_element()
}
