//! Settings → Archived (feature-inventory §1.5): archived chats across
//! devices, with Unarchive (Mutate setChatArchived false).
//!
//! One section card per device, in sidebar (recency) order of each device's
//! newest archived session. Each row is the list-row anatomy: an archive tile,
//! the session title (wrapping) over a quiet meta line, and an always-visible
//! Unarchive action, so the action is discoverable by pointer and keyboard and
//! never shifts the row when it appears.

use std::collections::HashMap;

use gpui::{
    AnyElement, Context, Entity, SharedString, Subscription, Task, Window, div, prelude::*,
};

use crate::button::{ButtonKind, ControlSize};
use crate::popover;
use crate::settings::widgets;
use crate::state::{AppState, ConnectionStatus};
use crate::theme::Theme;
use luna_proto::Chat;

/// Archived rows in sidebar (recency) order. Pure.
pub fn archived_chats(chats: &[Chat]) -> Vec<&Chat> {
    chats.iter().filter(|c| c.archived).collect()
}

/// Archived rows grouped by device, keeping recency order inside each group
/// and ordering groups by their newest session. Pure.
fn group_by_device(rows: Vec<Chat>) -> Vec<(String, Vec<Chat>)> {
    let mut groups: Vec<(String, Vec<Chat>)> = Vec::new();
    for chat in rows {
        match groups
            .iter_mut()
            .find(|(device, _)| *device == chat.device_id)
        {
            Some((_, chats)) => chats.push(chat),
            None => groups.push((chat.device_id.clone(), vec![chat])),
        }
    }
    groups
}

/// "Active 3h ago" from the compact sidebar stamp ("now", "3h", "2w"). Pure.
fn activity_label(time_ago: &str) -> String {
    if time_ago == "now" {
        "Active just now".to_string()
    } else {
        format!("Active {time_ago} ago")
    }
}

pub struct ArchivedPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    /// The last failed unarchive: the chat it belongs to and the message,
    /// shown in that chat's card right under its row.
    error: Option<(String, SharedString)>,
    /// In-flight unarchives by chat id. Each keeps its own task, so starting
    /// a second row never cancels the first one's request.
    pending: HashMap<String, Task<()>>,
    _observe: Subscription,
}

impl ArchivedPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            scroll: widgets::PageScroll::default(),
            error: None,
            pending: HashMap::new(),
            _observe: observe,
        }
    }

    fn unarchive(&mut self, chat_id: String, cx: &mut Context<Self>) {
        // A double click (or Enter then click) on the same row is one request.
        if self.pending.contains_key(&chat_id) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.error = Some((
                chat_id,
                "Unarchive failed: the engine is unavailable.".into(),
            ));
            cx.notify();
            return;
        };
        if self.error.as_ref().is_some_and(|(id, _)| *id == chat_id) {
            self.error = None;
        }
        let intent = serde_json::json!({ "kind": "setArchived", "archived": false });
        let task_id = chat_id.clone();
        let key = chat_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = crate::task_commands::execute(&engine, &task_id, intent).await;
            this.update(cx, |page, cx| {
                // Detached, not dropped: this task is the one running.
                if let Some(task) = page.pending.remove(&key) {
                    task.detach();
                }
                if let Err(err) = result {
                    page.error = Some((key, format!("Unarchive failed: {err}").into()));
                }
                cx.notify();
            })
            .ok();
        });
        self.pending.insert(chat_id, task);
        cx.notify();
    }

    fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    /// The failed-unarchive strip, inside the card under its row: dismissed
    /// by its close button (keyboard reachable) or a click on the strip.
    fn error_row(
        &self,
        theme: &Theme,
        message: SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        widgets::card_row(theme, false)
            .child(
                widgets::error_strip(theme, message)
                    .mt(crate::typography::ui_rems(0.0))
                    .w_full()
                    .id("archived-error")
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss_error(cx)))
                    .child(
                        crate::button::icon_button(
                            "archived-error-dismiss",
                            crate::icons::CLOSE,
                            ControlSize::Sm,
                            true,
                            theme,
                        )
                        .ml_auto()
                        .aria_label("Dismiss error")
                        .tooltip(popover::text_tooltip("Dismiss"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.dismiss_error(cx);
                        })),
                    ),
            )
            .into_any_element()
    }
}

impl popover::ScrollRailHost for ArchivedPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for ArchivedPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let now = chrono::Utc::now();
        let (rows, device_names, local_id, connection) = {
            let state = self.state.read(cx);
            let rows: Vec<Chat> = archived_chats(&state.chats).into_iter().cloned().collect();
            let names: HashMap<String, String> = state
                .devices
                .iter()
                .map(|d| (d.id.clone(), d.name.clone()))
                .collect();
            (
                rows,
                names,
                state.local_device_id.clone(),
                state.connection.clone(),
            )
        };
        let count = rows.len();

        let mut sections: Vec<AnyElement> = Vec::new();
        if rows.is_empty() {
            // §7 states: skeleton while the engine connects, the connection
            // error when it failed, otherwise one quiet empty row.
            let card = widgets::section_card(&theme);
            let card = match &connection {
                ConnectionStatus::Connecting => {
                    card.child(widgets::card_row(&theme, true).child(div().w_full().child(
                        popover::skeleton_rows("archived-loading", &theme, 3, cx.entity_id(), cx),
                    )))
                }
                ConnectionStatus::Failed(message) => card.child(
                    widgets::card_row(&theme, true).child(
                        widgets::error_strip(
                            &theme,
                            SharedString::from(format!("Archived sessions can't load: {message}")),
                        )
                        .mt(crate::typography::ui_rems(0.0))
                        .w_full(),
                    ),
                ),
                ConnectionStatus::Ready => card.child(
                    widgets::card_row(&theme, true)
                        .child(widgets::row_tile(
                            &theme,
                            crate::icons::ARCHIVE_MINIMALISTIC,
                        ))
                        .child(widgets::row_text(
                            &theme,
                            "Nothing archived",
                            Some("Right-click a session in the sidebar to archive it.".into()),
                        )),
                ),
            };
            sections.push(card.into_any_element());
        }

        for (device_id, chats) in group_by_device(rows) {
            let is_local = local_id.as_deref() == Some(device_id.as_str());
            let device_name: SharedString = device_names
                .get(&device_id)
                .cloned()
                .unwrap_or_else(|| "Unknown device".to_string())
                .into();
            let sessions = match chats.len() {
                1 => "1 session".to_string(),
                n => format!("{n} sessions"),
            };
            let description = if is_local {
                format!("This device · {sessions}")
            } else {
                sessions
            };
            let mut card = widgets::section_card(&theme).child(widgets::section_header(
                &theme,
                device_name,
                Some(description.into()),
                Vec::new(),
            ));
            for chat in chats {
                let title: SharedString = chat
                    .title
                    .clone()
                    .unwrap_or_else(|| "Untitled session".into())
                    .into();
                let time_ago = crate::state::format_time_ago(
                    chat.last_message_at.unwrap_or(chat.created_at),
                    now,
                );
                let mut meta: Vec<AnyElement> = vec![
                    div()
                        .child(SharedString::from(activity_label(&time_ago)))
                        .into_any_element(),
                ];
                if let Some(location) = crate::state::chat_location(&chat) {
                    meta.push(div().child(SharedString::from(location)).into_any_element());
                }
                let is_busy = self.pending.contains_key(&chat.id);
                let chat_id = chat.id.clone();
                let unarchive = crate::button::button(
                    format!("unarchive-{}", chat.id),
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    !is_busy,
                    &theme,
                )
                .aria_label(SharedString::from(format!("Unarchive {title}")))
                .when(!is_busy, |button| {
                    button.on_click(cx.listener(move |this, _, _, cx| {
                        this.unarchive(chat_id.clone(), cx);
                    }))
                })
                .child(crate::button::glyph(
                    crate::icons::ARCHIVE_UP_MINIMALISTIC,
                    ControlSize::Sm,
                ))
                .child(SharedString::from(if is_busy {
                    "Unarchiving…"
                } else {
                    "Unarchive"
                }));
                card = card.child(
                    widgets::card_row(&theme, false)
                        .child(widgets::row_tile(
                            &theme,
                            crate::icons::ARCHIVE_MINIMALISTIC,
                        ))
                        .child(
                            widgets::row_text(&theme, title, None)
                                .child(widgets::meta_line(&theme, meta)),
                        )
                        .child(widgets::row_controls().child(unarchive)),
                );
                if let Some((_, message)) = self
                    .error
                    .as_ref()
                    .filter(|(id, _)| *id == chat.id)
                    .cloned()
                {
                    card = card.child(self.error_row(&theme, message, cx));
                }
            }
            sections.push(card.into_any_element());
        }

        let scrollbar = popover::rail(self, "archived-page-scrollbar", &theme, cx);
        div()
            .id("archived-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("archived-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_title_bar(
                                &theme,
                                "Archived sessions",
                                (count > 0).then_some(count),
                                Vec::new(),
                            ))
                            .child(widgets::page_subtitle(
                                &theme,
                                "Hidden from the sidebar, never deleted. Unarchiving puts a session back on its device.",
                            ))
                            .children(sections),
                    ),
            )
            .children(scrollbar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn chat(id: &str, archived: bool) -> Chat {
        Chat {
            child_thread: None,
            id: id.into(),
            device_id: "d".into(),
            title: None,
            archived,
            settlement_override: None,
            pinned_order: None,
            cwd: None,
            branch: None,
            checkout_id: None,
            source_context: None,
            config: None,
            last_message_preview: None,
            last_message_at: None,
            created_at: Utc::now(),
            harness_session_id: None,
            harness_session_cwd: None,
            space_id: None,
            last_seen_at: None,
            manually_unread: false,
            room_gen: None,
        }
    }

    #[test]
    fn only_archived_rows_show() {
        let chats = vec![chat("a", false), chat("b", true), chat("c", true)];
        let rows = archived_chats(&chats);
        let ids: Vec<&str> = rows.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["b", "c"]);
    }
}
