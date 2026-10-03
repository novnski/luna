//! Shared chrome for the right pane's related-chat lists (Child chats, Side
//! chats): a header with Back to the surface menu, and sidebar-style sections
//! (Pinned, the active rows, Settled, Archived) built from the sidebar's own
//! task row ([`Shell::render_chat_row`]).

use super::*;

/// Sections of a related-chat list, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RelatedSection {
    Pinned,
    Active,
    Settled,
    Archived,
}

impl RelatedSection {
    fn label(self) -> &'static str {
        match self {
            Self::Pinned => "Pinned",
            Self::Active => "Active",
            Self::Settled => "Settled",
            Self::Archived => "Archived",
        }
    }

    /// Settled and Archived start folded, like the sidebar's shelves.
    fn collapsible(self) -> bool {
        matches!(self, Self::Settled | Self::Archived)
    }
}

impl Shell {
    /// Show the surface menu over this chat's open tabs. The tabs keep their
    /// state (a parked side chat keeps its lease); clicking one returns.
    pub(super) fn show_surface_picker(&mut self, cx: &mut Context<Self>) {
        self.suspend_subagent_watches(Some(RightSurface::Picker), cx);
        self.suspend_file_images(cx);
        let key = self.panel_key(cx);
        self.panels.update(&key, |panel| panel.picker_open = true);
        cx.notify();
    }

    /// Toolbar of a related-chat list: Back to the surface menu, the title,
    /// then optional trailing controls.
    pub(super) fn render_related_list_header(
        &self,
        id: &'static str,
        title: SharedString,
        trailing: Vec<AnyElement>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let muted = theme.text_muted;
        let border_strong = theme.border_strong;
        let hover = crate::theme::ink(0.06);
        let back = div()
            .id(SharedString::from(format!("{id}-back")))
            .role(gpui::Role::Button)
            .aria_label("Back to panels")
            .tab_index(0)
            .size(px(crate::surface_chrome::CONTROL_SIZE))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
            .border_1()
            .border_color(gpui::transparent_black())
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.border_color(border_strong))
            .tooltip(|_, cx| crate::pickers::ControlTooltip::view("Back to panels", None, cx))
            .tooltip_show_delay(crate::pickers::CONTROL_TOOLTIP_DELAY)
            .on_click(cx.listener(|this, _, _, cx| this.show_surface_picker(cx)))
            .on_key_down(crate::swallow_activation_key)
            .child(
                icon(icons::ALT_ARROW_LEFT)
                    .size(px(crate::surface_chrome::ICON_SIZE))
                    .text_color(muted),
            );
        crate::surface_chrome::toolbar(theme)
            .child(back)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(12.0))
                    .child(title),
            )
            .children(trailing)
    }

    /// A small text button in a related-list toolbar.
    pub(super) fn related_header_button(
        id: impl Into<SharedString>,
        glyph: &'static str,
        label: &'static str,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let muted = theme.text_muted;
        let border_strong = theme.border_strong;
        let hover = crate::theme::ink(0.06);
        div()
            .id(id.into())
            .role(gpui::Role::Button)
            .aria_label(label)
            .tab_index(0)
            .h(px(crate::surface_chrome::CONTROL_SIZE))
            .px(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.0))
            .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
            .border_1()
            .border_color(gpui::transparent_black())
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text_muted)
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.border_color(border_strong))
            .on_key_down(crate::swallow_activation_key)
            .child(
                icon(glyph)
                    .size(px(crate::surface_chrome::ICON_SIZE))
                    .text_color(muted),
            )
            .child(label)
    }

    /// Sidebar-style sections. `Active` has no header unless pins precede it;
    /// Settled and Archived fold behind a disclosure header with a count.
    pub(super) fn render_related_sections(
        &mut self,
        list: &'static str,
        sections: Vec<(RelatedSection, Vec<(String, AnyElement)>)>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        #[cfg(feature = "appshots-fixture")]
        self.fixture_related_rows.insert(
            list,
            sections
                .iter()
                .flat_map(|(section, rows)| {
                    rows.iter()
                        .map(move |(id, _)| (section.label().to_string(), id.clone()))
                })
                .collect(),
        );
        let has_pins = sections
            .iter()
            .any(|(section, rows)| *section == RelatedSection::Pinned && !rows.is_empty());
        let mut out = Vec::new();
        for (section, rows) in sections {
            if rows.is_empty() {
                continue;
            }
            let count = rows.len();
            let key = format!("{list}:{}", section.label());
            let open = !section.collapsible() || self.related_expanded.contains(&key);
            let show_header = section != RelatedSection::Active || has_pins;
            if show_header {
                let label: SharedString = if open {
                    section.label().into()
                } else {
                    format!("{} ({count})", section.label()).into()
                };
                let chevron = if section.collapsible() {
                    self.sidebar_disclosure_chevron(&format!("related:{key}"), open, theme)
                } else {
                    div().into_any_element()
                };
                let mut header = spaces::sidebar_disclosure_header(theme, label, chevron)
                    .id(SharedString::from(format!("related-section-{key}")));
                if section.collapsible() {
                    header = header.on_click(cx.listener(move |this, _, _, cx| {
                        if !this.related_expanded.remove(&key) {
                            this.related_expanded.insert(key.clone());
                        }
                        cx.notify();
                    }));
                } else {
                    header = header.cursor(gpui::CursorStyle::Arrow);
                }
                out.push(
                    div()
                        .w_full()
                        .pt(px(if out.is_empty() { 4.0 } else { 12.0 }))
                        .child(header)
                        .into_any_element(),
                );
            }
            if open {
                out.push(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .pt(px(if show_header { 4.0 } else { 0.0 }))
                        .children(rows.into_iter().map(|(_, row)| row))
                        .into_any_element(),
                );
            }
        }
        out
    }
}

/// Headless QA reads the related lists, the sidebar order and the pane's
/// navigation state through this hook.
#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_related_readback(&self, cx: &Context<Self>) -> serde_json::Value {
        let key = self.panel_key(cx);
        let state = self.state.read(cx);
        let mut pinned: Vec<&luna_proto::Chat> = state
            .chats
            .iter()
            .filter(|chat| !chat.archived && chat.pinned_order.is_some())
            .collect();
        pinned.sort_by(|left, right| {
            spaces::compare_sidebar_chats(SidebarSort::LastUpdated, left, right)
        });
        serde_json::json!({
            "sidebarOrder": self.sidebar_visible_order(cx),
            "sidebarSort": format!("{:?}", self.settings.sidebar_sort),
            "manualOrder": self.settings.sidebar_manual_order,
            "pinnedOrder": pinned.iter().map(|chat| chat.id.clone()).collect::<Vec<_>>(),
            "hover": self.chat_status_hover,
            "drag": self.sidebar_drag.as_ref().map(|drag| serde_json::json!({
                "chat": drag.chat_id,
                "target": drag.target.as_ref().map(|(anchor, place)| serde_json::json!([anchor, format!("{place:?}")])),
            })),
            "notice": self.sidebar_notice.as_ref().map(|notice| notice.message.to_string()),
            "rightActive": format!("{:?}", self.resolved_right_active(cx)),
            "pickerOpen": self.panels.get(&key).picker_open,
            "rightTabs": self
                .right_tabs
                .get(&key)
                .map(|tabs| tabs.iter().map(|tab| format!("{tab:?}")).collect::<Vec<_>>())
                .unwrap_or_default(),
            "related": self
                .fixture_related_rows
                .iter()
                .map(|(list, rows)| {
                    (
                        list.to_string(),
                        serde_json::json!(
                            rows.iter()
                                .map(|(section, id)| serde_json::json!([section, id]))
                                .collect::<Vec<_>>()
                        ),
                    )
                })
                .collect::<serde_json::Map<_, _>>(),
            "childOpen": self
                .child_chat_panel
                .as_ref()
                .and_then(|panel| panel.read(cx).open_chat_id().map(str::to_string)),
            "sideMarks": self.settings.side_chat_marks,
            "pendingActions": self.pending_task_actions.keys().cloned().collect::<Vec<_>>(),
        })
    }
}
