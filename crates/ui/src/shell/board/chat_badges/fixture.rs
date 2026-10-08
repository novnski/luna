//! Headless QA hooks for `board-chat-ui-fixture`: a probe of what the
//! board surfaces painted (bounds for real pointer input, their words) and
//! a readback of the slice's state. Nothing here changes board state.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{App, Bounds, Context, Pixels};
use serde_json::{Value, json};

use crate::shell::Shell;

/// What the board surfaces drew last frame (window coordinates).
#[derive(Default)]
pub(crate) struct BoardChatProbe {
    pub(crate) bounds: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    pub(crate) marks: Rc<RefCell<HashMap<String, Vec<String>>>>,
}

fn bounds_json(bounds: &Bounds<Pixels>) -> Value {
    json!({
        "x": f32::from(bounds.origin.x),
        "y": f32::from(bounds.origin.y),
        "w": f32::from(bounds.size.width),
        "h": f32::from(bounds.size.height),
    })
}

impl Shell {
    /// Start (or stop) recording the board surfaces' painted bounds.
    pub fn fixture_board_chat_probe(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.board_chat.probe = enabled.then(BoardChatProbe::default);
        cx.notify();
    }

    /// Forget last frame's records (a surface that stopped drawing must not
    /// keep its old bounds).
    pub fn fixture_board_chat_probe_clear(&mut self) {
        if let Some(probe) = &self.board_chat.probe {
            probe.bounds.borrow_mut().clear();
            probe.marks.borrow_mut().clear();
        }
    }

    /// Go to chat through the production entry point (the same call Board
    /// cards, receipts, Activity and the palette make).
    pub fn fixture_board_go_to_chat(
        &mut self,
        work_item_id: Option<String>,
        task_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.board_go_to_chat(
            luna_proto::BoardNavigationRequest {
                work_item_id,
                task_id,
            },
            cx,
        );
    }

    /// Re-read the badges now (a fixture waits for engine-side changes).
    pub fn fixture_board_chat_refresh(&mut self, cx: &mut Context<Self>) {
        self.board_chat_refresh(cx);
    }

    /// The slice's state as the user sees it.
    pub fn fixture_board_chat_readback(&self, cx: &App) -> Value {
        let ui = &self.board_chat;
        let (bounds, marks) = match &ui.probe {
            Some(probe) => (
                probe
                    .bounds
                    .borrow()
                    .iter()
                    .map(|(label, bounds)| (label.clone(), bounds_json(bounds)))
                    .collect::<serde_json::Map<_, _>>(),
                probe
                    .marks
                    .borrow()
                    .iter()
                    .map(|(key, values)| (key.clone(), json!(values)))
                    .collect::<serde_json::Map<_, _>>(),
            ),
            None => Default::default(),
        };
        let badge_json = |badge: &luna_proto::ChatTicketBadge| {
            json!({
                "taskId": badge.task_id,
                "workItemId": badge.work_item_id,
                "key": badge.key,
                "title": badge.title,
                "role": badge.role.as_str(),
                "column": badge.column.as_str(),
                "phase": badge.phase.label(),
                "text": super::render::badge_text(badge),
                "compact": super::render::compact_text(badge),
                "accessible": super::render::badge_accessible(badge),
            })
        };
        let state = self.state.read(cx);
        json!({
            "bounds": bounds,
            "marks": marks,
            "badgesUnavailable": ui.badges_unavailable,
            "badges": ui.badges.values().map(badge_json).collect::<Vec<_>>(),
            "related": ui.related.iter().map(|(chat, list)| json!({
                "taskId": chat,
                "tickets": list.iter().map(badge_json).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "receipts": ui.receipts.iter().map(|(chat, list)| json!({
                "taskId": chat,
                "receipts": list.iter().map(|receipt| json!({
                    "kind": format!("{:?}", receipt.kind),
                    "workItemId": receipt.work_item_id,
                    "key": receipt.key,
                    "title": receipt.title,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "identities": ui.identities.iter().map(|(id, identity)| (id.clone(), json!({
                "role": identity.role.as_str(),
                "mainParentId": identity.main_parent_id,
                "permitted": identity.permitted,
                "parentLocked": identity.parent_locked,
            }))).collect::<serde_json::Map<_, _>>(),
            "dialog": ui.dialog.as_ref().map(|dialog| json!({
                "kind": format!("{:?}", dialog.kind),
                "chatId": dialog.chat_id,
                "title": dialog.title.read(cx).text(),
                "body": dialog.body.read(cx).text(),
                "busy": dialog.busy,
                "lost": dialog.lost,
                "error": dialog.error.as_ref().map(|error| error.to_string()),
            })),
            "sideCard": ui.side_card.as_ref().map(|card| json!({
                "sideId": card.side_id,
                "mainId": card.main_id,
            })),
            "statusMenu": ui.status_menu.as_ref().map(|menu| json!({
                "taskId": menu.task_id,
                "workItemId": menu.work_item_id,
            })),
            "revealPending": ui.reveal.as_ref().map(|reveal| json!({
                "main": reveal.main,
                "side": reveal.side,
            })),
            "activity": ui.activity.iter().map(|event| json!({
                "id": event.id,
                "workItemId": event.work_item_id,
                "kind": event.kind.name(),
            })).collect::<Vec<_>>(),
            "route": match self.route {
                crate::shell::Route::Chat => "chat".to_string(),
                crate::shell::Route::Page(page) => format!("page:{}", page.key()),
                crate::shell::Route::Settings(_) => "settings".to_string(),
            },
            "selectedChat": state.selected_chat,
            "boardSelectedItem": self.settings.board.selected_item,
            "notice": self.sidebar_notice.as_ref().map(|notice| notice.message.to_string()),
            "chatCount": state.chats.len(),
        })
    }
}
