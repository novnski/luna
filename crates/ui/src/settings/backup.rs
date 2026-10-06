//! Settings → Backups. The engine is authoritative; this page only displays
//! redacted status and requests explicitly marked copy-isolated operations.
//!
//! Layout: the title bar carries Refresh and the page's main action, Back up
//! now. Every other action sits in the trailing column of the row it acts on
//! (Inspect on the last archive, Export on the backup folder, Stage on the
//! restore checkpoint), so each button lives beside what it changes.
//! Results and paths are rows; errors sit inside the section they belong to.

use gpui::{AnyElement, Context, Entity, Render, SharedString, Task, Window, div, prelude::*};
use luna_proto::{
    BackupArchiveRequest, BackupAvailability, BackupPreview, BackupStageStatus, BackupStatus,
    BackupSummary,
};
use luna_rpc::methods;

use super::widgets;
use crate::button::{ButtonKind, ControlSize};
use crate::icons;
use crate::popover::{self, Loadable};
use crate::state::AppState;
use crate::theme::Theme;
use crate::typography::ui_rems;

/// The page's engine operations. One runs at a time; the one in flight
/// names its button's working label and where its error is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackupAction {
    Backup,
    Export,
    Inspect,
    Stage,
}

pub struct BackupsPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    status: Loadable<BackupStatus>,
    /// A status reload is in flight. The last good status stays on screen
    /// meanwhile, so the page doesn't collapse to a skeleton after every
    /// action; actions stay disabled until the fresh status lands.
    refreshing: bool,
    preview: Loadable<BackupPreview>,
    last_result: Option<BackupSummary>,
    stage_result: Option<BackupStageStatus>,
    /// The last failed action and its message.
    error: Option<(BackupAction, String)>,
    /// The action in flight, if any: every action is disabled meanwhile.
    running: Option<BackupAction>,
    load_task: Option<Task<()>>,
    action_task: Option<Task<()>>,
}

impl BackupsPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut page = Self {
            state,
            scroll: widgets::PageScroll::default(),
            status: Loadable::Idle,
            refreshing: false,
            preview: Loadable::Idle,
            last_result: None,
            stage_result: None,
            error: None,
            running: None,
            load_task: None,
            action_task: None,
        };
        page.load(cx);
        page
    }

    /// No action is running and no reload is pending.
    fn idle(&self) -> bool {
        self.running.is_none() && !self.refreshing
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.status = Loadable::Error("Engine unavailable".into());
            self.refreshing = false;
            cx.notify();
            return;
        };
        if self.status.ready().is_none() {
            self.status = Loadable::Loading;
        }
        self.refreshing = true;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::GET_BACKUP_STATUS, serde_json::json!({}))
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<BackupStatus>(value).map_err(|error| error.to_string())
                });
            this.update(cx, |page, cx| {
                page.refreshing = false;
                page.status = match result {
                    Ok(status) => Loadable::Ready(status),
                    Err(error) => Loadable::Error(error),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn run_backup(&mut self, export: bool, cx: &mut Context<Self>) {
        if !self.idle() {
            return;
        }
        let Some(status) = self.status.ready() else {
            return;
        };
        if status.availability != BackupAvailability::Ready {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let (method, params) = if export {
            let Some(destination) = status.destination_dir.as_ref() else {
                return;
            };
            let filename = format!("luna-export-{}.tar.gz", uuid::Uuid::new_v4());
            let archive_path = std::path::Path::new(destination).join(filename);
            (
                methods::EXPORT_BACKUP,
                serde_json::to_value(BackupArchiveRequest {
                    archive_path: archive_path.display().to_string(),
                })
                .unwrap(),
            )
        } else {
            (methods::BACKUP_NOW, serde_json::json!({}))
        };
        let action = if export {
            BackupAction::Export
        } else {
            BackupAction::Backup
        };
        self.running = Some(action);
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(method, params)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<BackupSummary>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |page, cx| {
                page.running = None;
                match result {
                    Ok(summary) => {
                        page.last_result = Some(summary);
                        page.load(cx);
                    }
                    Err(error) => page.error = Some((action, error)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn inspect_latest(&mut self, cx: &mut Context<Self>) {
        if !self.idle() {
            return;
        }
        let Some(status) = self.status.ready() else {
            return;
        };
        let Some(last) = status.last_success.as_ref() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = serde_json::to_value(BackupArchiveRequest {
            archive_path: last.archive_path.clone(),
        })
        .unwrap();
        self.preview = Loadable::Loading;
        self.running = Some(BackupAction::Inspect);
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::INSPECT_BACKUP, params)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<BackupPreview>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |page, cx| {
                page.running = None;
                page.preview = match result {
                    Ok(preview) => Loadable::Ready(preview),
                    Err(error) => Loadable::Error(error),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn stage_latest(&mut self, cx: &mut Context<Self>) {
        if !self.idle() {
            return;
        }
        let Some(status) = self.status.ready() else {
            return;
        };
        if status.availability != BackupAvailability::Ready {
            return;
        }
        let Some(archive_path) = status
            .staged_restore
            .as_ref()
            .map(|stage| stage.archive_path.clone())
            .or_else(|| {
                status
                    .last_success
                    .as_ref()
                    .map(|last| last.archive_path.clone())
            })
        else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let params = serde_json::to_value(BackupArchiveRequest { archive_path }).unwrap();
        self.running = Some(BackupAction::Stage);
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::STAGE_BACKUP_RESTORE, params)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    serde_json::from_value::<BackupStageStatus>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |page, cx| {
                page.running = None;
                match result {
                    Ok(stage) => {
                        page.stage_result = Some(stage);
                        page.load(cx);
                    }
                    Err(error) => {
                        page.error = Some((BackupAction::Stage, error));
                        page.stage_result = None;
                        page.load(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }

    /// The failed action's message when it was one of `actions`.
    fn error_for(&self, actions: &[BackupAction]) -> Option<SharedString> {
        self.error
            .as_ref()
            .filter(|(action, _)| actions.contains(action))
            .map(|(_, message)| SharedString::from(message.clone()))
    }
}

impl popover::ScrollRailHost for BackupsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }
    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

/// Byte count in the largest whole unit (`512 B`, `3.4 MB`). Pure.
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A row whose only content is a status strip (error or warning), inside
/// the section it belongs to.
fn strip_row(theme: &Theme, strip: gpui::Div) -> gpui::Div {
    widgets::card_row(theme, false).child(strip.mt(ui_rems(0.0)).w_full())
}

/// A row titled `title` whose description is a path: user data, so it may
/// truncate (in the middle, keeping the file name) and shows the full path in
/// a tooltip. `path: None` shows `missing` as ordinary wrapping copy.
fn path_row(
    theme: &Theme,
    id: &'static str,
    title: &'static str,
    path: Option<String>,
    missing: &'static str,
) -> gpui::Div {
    let text = widgets::row_text(theme, title, None);
    let text = match path {
        Some(path) => text.child(
            div()
                .id(id)
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_middle()
                .text_size(ui_rems(widgets::ROW_DESCRIPTION_SIZE))
                .line_height(ui_rems(16.0))
                .text_color(theme.readable_muted(0.85))
                .tooltip(popover::text_tooltip(path.clone()))
                .child(SharedString::from(path)),
        ),
        None => text.child(widgets::row_description(theme, missing)),
    };
    widgets::card_row(theme, false).child(text)
}

/// A labelled action button for a row or the title bar, with its working
/// label while it runs.
#[allow(clippy::too_many_arguments)]
fn action_button(
    theme: &Theme,
    id: &'static str,
    glyph: &'static str,
    label: &'static str,
    working: &'static str,
    aria: &'static str,
    running: bool,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    crate::button::button(id, ButtonKind::Secondary, ControlSize::Md, enabled, theme)
        .aria_label(aria)
        .child(crate::button::glyph(glyph, ControlSize::Md))
        .child(if running { working } else { label })
}

impl Render for BackupsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let status = self.status.ready().cloned();
        let available = status
            .as_ref()
            .is_some_and(|value| value.availability == BackupAvailability::Ready);
        let ready = available && self.idle();
        let offline_automatic = status.as_ref().is_some_and(|value| {
            value.automatic_enabled && value.availability == BackupAvailability::Unavailable
        });
        let can_inspect = ready
            && status
                .as_ref()
                .is_some_and(|value| value.last_success.is_some());
        let can_stage = ready
            && status.as_ref().is_some_and(|value| {
                value.staged_restore.is_some() || value.last_success.is_some()
            });
        let can_export = ready
            && status
                .as_ref()
                .is_some_and(|value| value.destination_dir.is_some());
        let has_stage = status
            .as_ref()
            .is_some_and(|value| value.staged_restore.is_some());
        let running = self.running;
        let privacy_line = "Archives are unencrypted. Keep exported files private.";
        let status_label = match status.as_ref().map(|value| value.availability) {
            Some(BackupAvailability::Ready) => "Ready for isolated copies",
            Some(BackupAvailability::Unconfigured) => "Configuration needed",
            Some(BackupAvailability::Error) => "Verification failed",
            Some(BackupAvailability::Unavailable)
                if status.as_ref().is_some_and(|value| value.automatic_enabled) =>
            {
                "Automatic after shutdown"
            }
            Some(BackupAvailability::Unavailable) => "Unavailable for live data",
            None => {
                if self.status.is_loading() {
                    "Checking…"
                } else {
                    "Unavailable"
                }
            }
        };
        let status_badge: Option<AnyElement> = status.as_ref().map(|_| {
            if available {
                widgets::badge_active(&theme, "Isolated").into_any_element()
            } else if offline_automatic {
                widgets::badge(&theme, "Offline").into_any_element()
            } else {
                widgets::badge(&theme, "Paused").into_any_element()
            }
        });
        let last_line = status
            .as_ref()
            .and_then(|value| value.last_success.as_ref())
            .map(|receipt| {
                let date = chrono::DateTime::from_timestamp_millis(receipt.created_at_ms)
                    .map(|date| date.format("%Y-%m-%d %H:%M UTC").to_string())
                    .unwrap_or_else(|| "Unknown date".into());
                format!(
                    "{} · {} files · {}",
                    date,
                    receipt.entry_count,
                    format_bytes(receipt.total_bytes)
                )
            })
            .unwrap_or_else(|| "No verified backup recorded".into());
        let reason = status.as_ref().and_then(|s| s.reason.as_ref()).cloned();
        let restore_reason = status
            .as_ref()
            .map(|s| s.restore_reason.clone())
            .unwrap_or_else(|| "Restore activation is unavailable.".into());
        let staged = status
            .as_ref()
            .and_then(|value| value.staged_restore.clone())
            .or_else(|| self.stage_result.clone());

        // ── Title bar: Refresh, then the page's main action at the right.
        let refresh = crate::button::icon_button(
            "backups-refresh",
            icons::REFRESH,
            ControlSize::Md,
            self.idle(),
            &theme,
        )
        .aria_label("Refresh backup status")
        .tooltip(popover::text_tooltip("Refresh backup status"))
        .when(self.idle(), |button| {
            button.on_click(cx.listener(|page, _, _, cx| page.load(cx)))
        });
        let back_up_now = action_button(
            &theme,
            "backup-now",
            icons::ARCHIVE_MINIMALISTIC,
            "Back up now",
            "Backing up…",
            "Back up now",
            running == Some(BackupAction::Backup),
            ready,
        )
        .when(ready, |button| {
            button.on_click(cx.listener(|page, _, _, cx| page.run_backup(false, cx)))
        });

        // ── Status: availability, the last archive (Inspect), the latest save.
        let mut status_card = widgets::section_card(&theme).child(widgets::section_header(
            &theme,
            "Backup status",
            Some(status_label.into()),
            status_badge.into_iter().collect(),
        ));
        if let Some(error) = self.status.error() {
            status_card = status_card.child(strip_row(
                &theme,
                widgets::error_strip(&theme, SharedString::from(error.to_string())),
            ));
        }
        if status.is_none() && self.status.is_loading() {
            status_card =
                status_card.child(widgets::card_row(&theme, false).child(div().w_full().child(
                    popover::skeleton_rows("backups-loading", &theme, 3, cx.entity_id(), cx),
                )));
        }
        if let Some(reason) = reason {
            status_card = status_card.child(strip_row(
                &theme,
                widgets::warning_strip(&theme, SharedString::from(reason)),
            ));
        }
        if let Some(error) = self.error_for(&[BackupAction::Backup, BackupAction::Export]) {
            status_card = status_card.child(strip_row(&theme, widgets::error_strip(&theme, error)));
        }
        if status.is_some() {
            let inspect = action_button(
                &theme,
                "backup-inspect",
                icons::MAGNIFER,
                "Inspect",
                "Inspecting…",
                "Inspect last backup",
                running == Some(BackupAction::Inspect),
                can_inspect,
            )
            .when(can_inspect, |button| {
                button.on_click(cx.listener(|page, _, _, cx| page.inspect_latest(cx)))
            });
            status_card = status_card.child(
                widgets::card_row(&theme, false)
                    .child(widgets::row_text(
                        &theme,
                        "Last verified archive",
                        Some(last_line.into()),
                    ))
                    .child(widgets::row_controls().child(inspect)),
            );
        }
        if let Some(path) = self.last_result.as_ref().map(|r| r.archive_path.clone()) {
            status_card = status_card.child(path_row(
                &theme,
                "backup-saved-path",
                "Saved archive",
                Some(path),
                "",
            ));
        }

        // ── Archive contents: the inspection result, while it loads or once
        // it lands.
        let contents_card: Option<gpui::Div> = match &self.preview {
            Loadable::Idle => None,
            Loadable::Loading => Some(
                widgets::section_card(&theme)
                    .child(widgets::section_header(
                        &theme,
                        "Last archive contents",
                        Some("Inspecting the last verified archive…".into()),
                        Vec::new(),
                    ))
                    .child(widgets::card_row(&theme, false).child(div().w_full().child(
                        popover::skeleton_rows("backups-inspect", &theme, 2, cx.entity_id(), cx),
                    ))),
            ),
            Loadable::Error(error) => Some(
                widgets::section_card(&theme)
                    .child(widgets::section_header(
                        &theme,
                        "Last archive contents",
                        None,
                        Vec::new(),
                    ))
                    .child(strip_row(
                        &theme,
                        widgets::error_strip(&theme, SharedString::from(error.clone())),
                    )),
            ),
            Loadable::Ready(preview) => Some(
                widgets::section_card(&theme)
                    .child(widgets::section_header(
                        &theme,
                        "Last archive contents",
                        Some(
                            format!(
                                "{} documents · {} task events · {} attachments · {}",
                                preview.document_count,
                                preview.task_event_count,
                                preview.attachment_count,
                                format_bytes(preview.total_bytes)
                            )
                            .into(),
                        ),
                        Vec::new(),
                    ))
                    .child(widgets::card_row(&theme, false).child(widgets::row_text(
                        &theme,
                        "Excluded",
                        Some(if preview.excluded_categories.is_empty() {
                            "Nothing excluded".into()
                        } else {
                            preview.excluded_categories.join(", ").into()
                        }),
                    )))
                    .when(!preview.unsupported.is_empty(), |card| {
                        card.child(widgets::card_row(&theme, false).child(widgets::row_text(
                            &theme,
                            "Recovery limits",
                            Some(preview.unsupported.join("; ").into()),
                        )))
                    }),
            ),
        };

        // ── Locations: where archives come from and go (Export), privacy.
        let locations_card = status.as_ref().map(|status| {
            let export = action_button(
                &theme,
                "backup-export",
                icons::ARROW_UP_RIGHT,
                "Export archive",
                "Exporting…",
                "Export to backup folder",
                running == Some(BackupAction::Export),
                can_export,
            )
            .when(can_export, |button| {
                button.on_click(cx.listener(|page, _, _, cx| page.run_backup(true, cx)))
            });
            widgets::section_card(&theme)
                .child(widgets::section_header(
                    &theme,
                    "Locations",
                    None,
                    Vec::new(),
                ))
                .child(path_row(
                    &theme,
                    "backup-source-path",
                    "Source data",
                    status.source_root.clone(),
                    "No backup source configured",
                ))
                .child(
                    path_row(
                        &theme,
                        "backup-folder-path",
                        "Backup folder",
                        status.destination_dir.clone(),
                        "No backup destination configured",
                    )
                    .child(widgets::row_controls().child(export)),
                )
                .child(widgets::card_row(&theme, false).child(widgets::row_text(
                    &theme,
                    "Archive privacy",
                    Some(privacy_line.into()),
                )))
        });

        // ── Restore: the staged checkpoint (Stage / Verify) and what
        // activation needs.
        let restore_card = status.as_ref().map(|_| {
            let stage = action_button(
                &theme,
                "backup-stage",
                icons::RESTART,
                if has_stage {
                    "Verify staged copy"
                } else {
                    "Stage restore"
                },
                "Verifying…",
                if has_stage {
                    "Verify staged copy"
                } else {
                    "Stage restore"
                },
                running == Some(BackupAction::Stage),
                can_stage,
            )
            .when(can_stage, |button| {
                button.on_click(cx.listener(|page, _, _, cx| page.stage_latest(cx)))
            });
            let checkpoint = match staged.as_ref() {
                Some(staged) => path_row(
                    &theme,
                    "backup-staging-path",
                    "Staged restore checkpoint",
                    Some(staged.staging_path.clone()),
                    "",
                ),
                None => widgets::card_row(&theme, false).child(widgets::row_text(
                    &theme,
                    "Staged restore checkpoint",
                    Some(
                        "Nothing staged. Staging unpacks the last verified archive into an \
                         isolated copy and verifies it; live data is not changed."
                            .into(),
                    ),
                )),
            };
            widgets::section_card(&theme)
                .child(widgets::section_header(&theme, "Restore", None, Vec::new()))
                .child(checkpoint.child(widgets::row_controls().child(stage)))
                .when_some(self.error_for(&[BackupAction::Stage]), |card, error| {
                    card.child(strip_row(&theme, widgets::error_strip(&theme, error)))
                })
                .when_some(staged.as_ref(), |card, staged| {
                    card.child(widgets::card_row(&theme, false).child(widgets::row_text(
                        &theme,
                        "Activation unavailable",
                        Some(SharedString::from(staged.reason.clone())),
                    )))
                })
                .child(strip_row(
                    &theme,
                    widgets::warning_strip(&theme, SharedString::from(restore_reason.clone())),
                ))
        });

        let scroll_bar = popover::rail(self, "backups-page-scrollbar", &theme, cx);
        div()
            .id("backups-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                div()
                    .id("backups-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll.scroll)
                    .child(
                        widgets::page_column()
                            .child(widgets::page_title_bar(
                                &theme,
                                "Backups",
                                None,
                                vec![refresh.into_any_element(), back_up_now.into_any_element()],
                            ))
                            .child(widgets::page_subtitle(
                                &theme,
                                "Private BB-style archives after clean shutdown; manual restore runs while LUNA is stopped.",
                            ))
                            .child(status_card)
                            .children(contents_card)
                            .children(locations_card)
                            .children(restore_card),
                    ),
            )
            .children(scroll_bar)
    }
}
