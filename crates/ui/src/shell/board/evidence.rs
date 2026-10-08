//! Evidence and results (A9 presentation over A5's records): diff/PR/check/
//! artifact outcomes, reviewer findings, launch snapshots and dispatch
//! history. Stub until A9 lands.
// Entry points are wired by the Board detail slice.
#![allow(dead_code)]
use gpui::{AnyElement, IntoElement, div};
use luna_proto::BoardItemDetail;

use crate::theme::Theme;

/// Render a ticket's evidence section.
pub(crate) fn render_evidence(detail: &BoardItemDetail, theme: &Theme) -> AnyElement {
    let _ = (detail, theme);
    div().into_any_element()
}
