//! Settings → Devices (feature-inventory §1.5): the device registry — name,
//! platform, last-seen, presence dot, a "This device" badge, a Copy id action,
//! and a Rename dialog (Mutate renameDevice).
//!
//! One section card of list rows: the platform tile carrying the presence
//! dot, the device name (wrapping) over a quiet meta line, then the status
//! badges and the always-visible Copy id and Rename kit actions.

use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, SharedString, Subscription, Task, Window, div,
    prelude::*,
};
use std::time::Duration;

use luna_proto::WorkspaceScope;
use luna_rpc::methods;

use crate::button::{ButtonKind, ControlSize};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::popover;
use crate::settings::widgets;
use crate::state::{AppState, ConnectionStatus};
use crate::theme::Theme;
use crate::typography::ui_rems;

/// A device that pinged within this window shows a presence dot (engines
/// heartbeat every 15s; 70s tolerates a couple of missed beats).
pub const DEVICE_ONLINE_WINDOW_SECS: i64 = 70;

/// Presence: last-seen within the online window (future timestamps count). Pure.
pub fn device_online(last_seen: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last_seen
        .is_some_and(|at| now.signed_duration_since(at).num_seconds() <= DEVICE_ONLINE_WINDOW_SECS)
}

/// Compact last-seen line. Pure.
pub fn format_last_seen(last_seen: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(at) = last_seen else {
        return "never seen".to_string();
    };
    let secs = now.signed_duration_since(at).num_seconds();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Scope-aware copy: a local registry describes only the active local
/// workspace and must not imply that account device metadata is already live.
pub fn devices_subtitle(scope: Option<WorkspaceScope>) -> &'static str {
    match scope {
        Some(WorkspaceScope::Local) => "Manage device details stored in this local workspace.",
        Some(WorkspaceScope::Synced) => "Manage device names and inspect synced device metadata.",
        Some(WorkspaceScope::Development) | None => "Manage device names for this workspace.",
    }
}

struct RenameDialog {
    device_id: String,
    input: Entity<ComposerInput>,
    _events: Subscription,
}

pub struct DevicesPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    rename: Option<RenameDialog>,
    /// Device id whose id-chip shows "Copied" right now.
    copied: Option<String>,
    error: Option<SharedString>,
    copy_task: Option<Task<()>>,
    _observe: Subscription,
}

impl DevicesPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            state,
            scroll: widgets::PageScroll::default(),
            rename: None,
            copied: None,
            error: None,
            copy_task: None,
            _observe: observe,
        }
    }

    fn open_rename(&mut self, device_id: String, current: String, cx: &mut Context<Self>) {
        let input = cx.new(|cx| ComposerInput::new("Device name", cx));
        input.update(cx, |input, cx| input.set_text(current, cx));
        let events = cx.subscribe(&input, |this: &mut Self, _, event, cx| {
            if matches!(event, ComposerInputEvent::Submitted) {
                this.submit_rename(cx);
            }
        });
        self.rename = Some(RenameDialog {
            device_id,
            input,
            _events: events,
        });
        cx.notify();
    }

    fn submit_rename(&mut self, cx: &mut Context<Self>) {
        // An empty name keeps the dialog open instead of silently closing it.
        let Some(name) = self
            .rename
            .as_ref()
            .map(|dialog| dialog.input.read(cx).text().trim().to_string())
        else {
            return;
        };
        if name.is_empty() {
            return;
        }
        // Taken once: Enter and a click on Rename can't submit twice.
        let Some(dialog) = self.rename.take() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.error = Some("Rename failed: the engine is unavailable.".into());
            cx.notify();
            return;
        };
        let params = serde_json::json!({
            "op": "renameDevice",
            "deviceId": dialog.device_id,
            "name": name,
        });
        // Detached: a second rename started while this one is in flight must
        // not cancel it (a single task slot dropped the earlier request).
        cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::MUTATE, params).await;
            this.update(cx, |page, cx| {
                if let Err(err) = result {
                    page.error = Some(format!("Rename failed: {err}").into());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn copy_id(&mut self, device_id: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(device_id.clone()));
        self.copied = Some(device_id);
        self.copy_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            this.update(cx, |page, cx| {
                page.copied = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn render_rename_dialog(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let dialog = self.rename.as_ref()?;
        let input = dialog.input.clone();
        let card = popover::dialog_card(&theme)
            .child(popover::dialog_title(&theme, "Rename device"))
            .child(
                div()
                    .mt(crate::typography::ui_rems(12.0))
                    .child(popover::dialog_field(input.into_any_element())),
            )
            .child(
                div()
                    .mt(crate::typography::ui_rems(16.0))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(crate::typography::ui_rems(8.0))
                    .child(
                        popover::btn_ghost(&theme, "Cancel", "rename-cancel")
                            .id("rename-cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.rename = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        popover::btn_primary(&theme, "Rename")
                            .id("rename-save")
                            .on_click(cx.listener(|this, _, _, cx| this.submit_rename(cx))),
                    ),
            )
            .into_any_element();
        Some(popover::modal("rename-device-dialog", viewport, card))
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    /// One device row: platform tile with the presence dot · name over the
    /// meta line · status badges · Copy id and Rename.
    fn device_row(
        &self,
        theme: &Theme,
        device: &luna_proto::Device,
        is_local: bool,
        local_only: bool,
        now: DateTime<Utc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let online = device_online(device.last_seen_at, now);
        let id_copied = self.copied.as_deref() == Some(device.id.as_str());
        let platform_icon = match device.platform.as_str() {
            "macos" | "darwin" => crate::icons::LAPTOP,
            "web" => crate::icons::GLOBAL,
            "ios" | "android" => crate::icons::SMARTPHONE,
            _ => crate::icons::MONITOR,
        };
        // Presence lives ON the identity tile: a corner dot (success with a
        // soft glow online, a quiet neutral offline), ringed by the card tone
        // so it "cuts" the tile. Sized in rems with the tile; the ring is a
        // hairline-class border and stays in pixels.
        let success = theme.success;
        let tile = widgets::row_tile(theme, platform_icon).relative().child(
            div()
                .absolute()
                .bottom(ui_rems(-3.0))
                .right(ui_rems(-3.0))
                .size(ui_rems(9.0))
                .rounded_full()
                .border_2()
                .border_color(theme.surface)
                .when(online, |el| {
                    el.bg(success).shadow(vec![gpui::BoxShadow {
                        color: success.opacity(0.55),
                        offset: gpui::point(gpui::px(0.0), gpui::px(0.0)),
                        blur_radius: gpui::px(6.0),
                        spread_radius: gpui::px(0.0),
                        inset: false,
                    }])
                })
                .when(!online, |el| el.bg(theme.decorative_muted(0.35))),
        );

        // One quiet meta line: (offline: last seen) · platform · version ·
        // added · id. Presence comes first when it is the news.
        let mut meta: Vec<AnyElement> = Vec::new();
        if !online {
            meta.push(
                div()
                    .child(SharedString::from(format!(
                        "Last seen {}",
                        format_last_seen(device.last_seen_at, now)
                    )))
                    .into_any_element(),
            );
        }
        meta.push(
            div()
                .child(SharedString::from(
                    platform_label(&device.platform).to_string(),
                ))
                .into_any_element(),
        );
        if let Some(version) = device.version.as_deref().filter(|v| !v.is_empty()) {
            meta.push(
                div()
                    .child(SharedString::from(format!("v{version}")))
                    .into_any_element(),
            );
        }
        // "Added {time ago}" (luna settings.devices.tsx).
        if let Some(created) = device.created_at {
            meta.push(
                div()
                    .child(SharedString::from(format!(
                        "Added {}",
                        format_last_seen(Some(created), now)
                    )))
                    .into_any_element(),
            );
        }
        // The id is user data: shortened, with the full id in a tooltip. It
        // reads "Copied" for a moment after Copy id.
        meta.push(
            div()
                .id(SharedString::from(format!("device-id-{}", device.id)))
                .font_family(theme.font_mono.clone())
                .text_size(ui_rems(11.0))
                .text_color(if id_copied {
                    theme.readable_tone(theme.success_muted, 0.9)
                } else {
                    theme.readable_muted(0.8)
                })
                .tooltip(popover::text_tooltip(device.id.clone()))
                .child(SharedString::from(if id_copied {
                    "Copied".to_string()
                } else {
                    short_id(&device.id)
                }))
                .into_any_element(),
        );

        let copy_id = device.id.clone();
        let rename_id = device.id.clone();
        let rename_name = device.name.clone();
        let controls = widgets::row_controls()
            .when(is_local, |controls| {
                controls.child(widgets::badge(
                    theme,
                    if local_only {
                        "Local only"
                    } else {
                        "This device"
                    },
                ))
            })
            // This device is online by definition; the badge marks the others.
            .when(online && !is_local, |controls| {
                controls.child(widgets::badge_active(theme, "Online"))
            })
            .child(
                crate::button::icon_button(
                    format!("device-copy-{}", device.id),
                    if id_copied {
                        crate::icons::COPY_CHECK
                    } else {
                        crate::icons::COPY
                    },
                    ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label(if id_copied {
                    "Device id copied"
                } else {
                    "Copy device id"
                })
                .tooltip(popover::text_tooltip(if id_copied {
                    "Copied"
                } else {
                    "Copy device id"
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.copy_id(copy_id.clone(), cx);
                })),
            )
            .child(
                crate::button::button(
                    format!("device-rename-{}", device.id),
                    ButtonKind::Ghost,
                    ControlSize::Sm,
                    true,
                    theme,
                )
                .aria_label(SharedString::from(format!("Rename {}", device.name)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_rename(rename_id.clone(), rename_name.clone(), cx);
                }))
                .child(crate::button::glyph(crate::icons::PEN, ControlSize::Sm))
                .child(SharedString::from("Rename")),
            );

        // Below the section header, so every row carries its hairline.
        widgets::card_row(theme, false)
            .child(tile)
            .child(
                widgets::row_text(theme, device.name.clone(), None)
                    .child(widgets::meta_line(theme, meta)),
            )
            .child(controls)
            .into_any_element()
    }
}

impl popover::ScrollRailHost for DevicesPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

/// Human platform label (luna settings.devices.tsx `platformLabel`).
pub fn platform_label(platform: &str) -> &str {
    match platform {
        "macos" | "darwin" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        "web" => "Web",
        "ios" => "iOS",
        "android" => "Android",
        other => other,
    }
}

/// Short device id for the meta line (`abcd1234…wxyz`). Counts characters, so
/// a non-ASCII id never splits inside a code point.
pub fn short_id(id: &str) -> String {
    let chars: Vec<char> = id.chars().collect();
    if chars.len() > 12 {
        let head: String = chars[..8].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}…{tail}")
    } else {
        id.to_string()
    }
}

impl Render for DevicesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let now = Utc::now();
        let (devices, local_id, workspace_scope, connection) = {
            let state = self.state.read(cx);
            (
                state.devices.clone(),
                state.local_device_id.clone(),
                state.workspace_scope,
                state.connection.clone(),
            )
        };
        let dialog = self.render_rename_dialog(window.viewport_size(), cx);
        let count = devices.len();
        let local_only = workspace_scope == Some(WorkspaceScope::Local);

        let mut card = widgets::section_card(&theme);
        if !devices.is_empty() {
            card = card.child(widgets::section_header(
                &theme,
                "Registered devices",
                Some("A device is online when it has checked in within about a minute.".into()),
                Vec::new(),
            ));
        }
        // A failed rename belongs to this list: the strip sits inside the
        // card, dismissed by its close button or a click on the strip.
        if let Some(message) = self.error.clone() {
            card = card.child(
                widgets::card_row(&theme, devices.is_empty()).child(
                    widgets::error_strip(&theme, message)
                        .mt(ui_rems(0.0))
                        .w_full()
                        .id("devices-error")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_error(cx)))
                        .child(
                            crate::button::icon_button(
                                "devices-error-dismiss",
                                crate::icons::CLOSE,
                                ControlSize::Sm,
                                true,
                                &theme,
                            )
                            .ml_auto()
                            .aria_label("Dismiss error")
                            .tooltip(popover::text_tooltip("Dismiss"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.dismiss_error(cx);
                            })),
                        ),
                ),
            );
        }
        let first_row = devices.is_empty() && self.error.is_none();
        if devices.is_empty() {
            // §7 states: skeleton while the engine connects, its error when
            // it failed, otherwise one quiet empty row.
            card = match &connection {
                ConnectionStatus::Connecting => card.child(
                    widgets::card_row(&theme, first_row).child(div().w_full().child(
                        popover::skeleton_rows("devices-loading", &theme, 2, cx.entity_id(), cx),
                    )),
                ),
                ConnectionStatus::Failed(message) => card.child(
                    widgets::card_row(&theme, first_row).child(
                        widgets::error_strip(
                            &theme,
                            SharedString::from(format!("Devices can't load: {message}")),
                        )
                        .mt(ui_rems(0.0))
                        .w_full(),
                    ),
                ),
                ConnectionStatus::Ready => card.child(
                    widgets::card_row(&theme, first_row)
                        .child(widgets::row_tile(&theme, crate::icons::MONITOR))
                        .child(widgets::row_text(
                            &theme,
                            "No devices registered",
                            Some("A device appears here once LUNA runs on it.".into()),
                        )),
                ),
            };
        } else {
            for device in &devices {
                let is_local = local_id.as_deref() == Some(device.id.as_str());
                card = card.child(self.device_row(&theme, device, is_local, local_only, now, cx));
            }
        }

        let scrollbar = popover::rail(self, "devices-page-scrollbar", &theme, cx);
        div()
            .id("devices-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("devices-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_title_bar(
                                &theme,
                                "Devices",
                                (count > 0).then_some(count),
                                Vec::new(),
                            ))
                            .child(widgets::page_subtitle(
                                &theme,
                                devices_subtitle(workspace_scope),
                            ))
                            .child(card),
                    ),
            )
            .children(scrollbar)
            .when_some(dialog, |el, dialog| el.child(dialog))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    #[test]
    fn presence_window() {
        let now = Utc::now();
        assert!(device_online(Some(now - TimeDelta::seconds(10)), now));
        assert!(device_online(Some(now - TimeDelta::seconds(70)), now));
        assert!(!device_online(Some(now - TimeDelta::seconds(71)), now));
        assert!(!device_online(None, now));
        // Clock skew (future) counts as online.
        assert!(device_online(Some(now + TimeDelta::seconds(30)), now));
    }

    #[test]
    fn last_seen_formatting() {
        let now = Utc::now();
        assert_eq!(format_last_seen(None, now), "never seen");
        assert_eq!(
            format_last_seen(Some(now - TimeDelta::seconds(30)), now),
            "just now"
        );
        assert_eq!(
            format_last_seen(Some(now - TimeDelta::minutes(5)), now),
            "5m ago"
        );
        assert_eq!(
            format_last_seen(Some(now - TimeDelta::hours(3)), now),
            "3h ago"
        );
        assert_eq!(
            format_last_seen(Some(now - TimeDelta::days(2)), now),
            "2d ago"
        );
    }

    #[test]
    fn local_subtitle_does_not_claim_synced_metadata() {
        let copy = devices_subtitle(Some(WorkspaceScope::Local));
        assert!(copy.contains("local workspace"));
        assert!(!copy.contains("synced"));
    }
}
