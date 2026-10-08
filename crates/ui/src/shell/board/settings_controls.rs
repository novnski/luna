//! Implementation/review settings controls (A9): harness/account/model,
//! supported reasoning/options, access and reviewer profile, fed by
//! `BoardApi::execution_options`; inherited values shown explicitly.
//! Stub until A9 lands.
// Entry points are wired by the Board detail slice.
#![allow(dead_code)]
use gpui::{AnyElement, IntoElement, div};
use luna_proto::{BoardExecutionOptions, BoardExecutionProfile, BoardReviewProfile};

use crate::theme::Theme;

/// The current choices in a settings editor.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExecutionSettingsDraft {
    pub implementation: BoardExecutionProfile,
    pub review: BoardReviewProfile,
}

/// Render the controls for a draft against the host's real options.
pub(crate) fn render_settings_controls(
    draft: &ExecutionSettingsDraft,
    options: Option<&BoardExecutionOptions>,
    theme: &Theme,
) -> AnyElement {
    let _ = (draft, options, theme);
    div().into_any_element()
}
