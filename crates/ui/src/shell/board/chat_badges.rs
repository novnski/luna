//! Ticket and reviewer badges for chat surfaces (A10): chat header, sidebar
//! rows, side-chat tabs/lists, from `BoardApi::chat_badges`. "Ticket LUN-42"
//! for working chats, "Reviewer · LUN-42" for reviewers; source discussions
//! get receipts, never a working badge. Stub until A10 lands.
// Entry points are wired by the chat UI slice.
#![allow(dead_code)]
use gpui::{AnyElement, IntoElement, div};
use luna_proto::ChatTicketBadge;

use crate::theme::Theme;

/// Render one compact badge.
pub(crate) fn render_badge(badge: &ChatTicketBadge, theme: &Theme) -> AnyElement {
    let _ = (badge, theme);
    div().into_any_element()
}
