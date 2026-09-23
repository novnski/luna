//! Settings → Backups. The engine is authoritative; this page only displays
//! redacted status and requests explicitly marked copy-isolated operations.

use gpui::{Context, Entity, Render, SharedString, Task, Window, div, prelude::*, px};
use zeron_proto::{
    BackupArchiveRequest, BackupAvailability, BackupPreview, BackupStageStatus, BackupStatus,
    BackupSummary,
};
use zeron_rpc::methods;

use super::widgets;
use crate::popover::{self, Loadable};
use crate::state::AppState;
use crate::theme::Theme;
use crate::{icons, theme};

pub struct BackupsPage {
    state: Entity<AppState>,
    scroll: widgets::PageScroll,
    status: Loadable<BackupStatus>,
    preview: Loadable<BackupPreview>,
    last_result: Option<BackupSummary>,
    stage_result: Option<BackupStageStatus>,
    error: Option<String>,
    busy: bool,
    load_task: Option<Task<()>>,
    action_task: Option<Task<()>>,
}

impl BackupsPage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut page = Self {
            state,
            scroll: widgets::PageScroll::default(),
            status: Loadable::Idle,
            preview: Loadable::Idle,
            last_result: None,
            stage_result: None,
            error: None,
            busy: false,
            load_task: None,
            action_task: None,
        };
        page.load(cx);
        page
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.status = Loadable::Error("Engine unavailable".into());
            cx.notify();
            return;
        };
        self.status = Loadable::Loading;
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
                page.status = match result {
                    Ok(status) => Loadable::Ready(status),
                    Err(error) => Loadable::Error(error),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn run_backup(&mut self, export: bool, cx: &mut Context<Self>) {
        if self.busy {
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
            let filename = format!("luna-export-{}.lunabk", uuid::Uuid::new_v4());
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
        self.busy = true;
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
                page.busy = false;
                match result {
                    Ok(summary) => {
                        page.last_result = Some(summary);
                        page.load(cx);
                    }
                    Err(error) => page.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn inspect_latest(&mut self, cx: &mut Context<Self>) {
        if self.busy {
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
        self.busy = true;
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
                page.busy = false;
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
        if self.busy {
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
        self.busy = true;
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
                page.busy = false;
                match result {
                    Ok(stage) => {
                        page.stage_result = Some(stage);
                        page.load(cx);
                    }
                    Err(error) => {
                        page.error = Some(error);
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
}

impl popover::ScrollRailHost for BackupsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }
    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for BackupsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let status = self.status.ready().cloned();
        let ready = status
            .as_ref()
            .is_some_and(|value| value.availability == BackupAvailability::Ready)
            && !self.busy;
        let can_inspect = ready
            && status
                .as_ref()
                .is_some_and(|value| value.last_success.is_some());
        let can_stage = ready
            && status.as_ref().is_some_and(|value| {
                value.staged_restore.is_some() || value.last_success.is_some()
            });
        let has_stage = status
            .as_ref()
            .is_some_and(|value| value.staged_restore.is_some());
        let key_line = match status.as_ref() {
            Some(value) if value.key_configured => "Owner recovery key configured",
            Some(_) => "Owner recovery key needed. This build cannot generate or reveal one.",
            None => "Recovery key status unavailable",
        };
        let status_label = match status.as_ref().map(|value| value.availability) {
            Some(BackupAvailability::Ready) => "Ready for isolated copies",
            Some(BackupAvailability::Unconfigured) => "Configuration needed",
            Some(BackupAvailability::Error) => "Verification failed",
            Some(BackupAvailability::Unavailable) => "Unavailable for live data",
            None => {
                if self.status.is_loading() {
                    "Checking…"
                } else {
                    "Unavailable"
                }
            }
        };
        let last_line = status
            .as_ref()
            .and_then(|value| value.last_success.as_ref())
            .map(|receipt| {
                let date = chrono::DateTime::from_timestamp_millis(receipt.created_at_ms)
                    .map(|date| date.format("%Y-%m-%d %H:%M UTC").to_string())
                    .unwrap_or_else(|| "Unknown date".into());
                format!(
                    "{} · {} files · {} bytes",
                    date, receipt.entry_count, receipt.total_bytes
                )
            })
            .unwrap_or_else(|| "No verified backup recorded".into());
        let source = status
            .as_ref()
            .and_then(|s| s.source_root.as_ref())
            .cloned()
            .unwrap_or_else(|| "No copy-isolated source configured".into());
        let destination = status
            .as_ref()
            .and_then(|s| s.destination_dir.as_ref())
            .cloned()
            .unwrap_or_else(|| "No marked destination configured".into());
        let reason = status.as_ref().and_then(|s| s.reason.as_ref()).cloned();
        let restore_reason = status
            .as_ref()
            .map(|s| s.restore_reason.clone())
            .unwrap_or_else(|| "Restore activation is unavailable.".into());
        let preview = self.preview.ready().cloned();
        let staged = status
            .as_ref()
            .and_then(|value| value.staged_restore.clone())
            .or_else(|| self.stage_result.clone());
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
                            .child(widgets::page_header(&theme, "Backups", None))
                            .child(widgets::page_subtitle(&theme,
                                "Encrypted exports and backups for explicitly isolated LUNA copies."))
                            .when_some(self.status.error().map(str::to_owned), |page, error| page.child(widgets::error_strip(&theme, SharedString::from(error))))
                            .when_some(reason, |page, reason| page.child(widgets::warning_strip(&theme, SharedString::from(reason))))
                            .when_some(self.error.clone(), |page, error| page.child(widgets::error_strip(&theme, SharedString::from(error))))
                            .when_some(self.preview.error().map(str::to_owned), |page, error| page.child(widgets::error_strip(&theme, SharedString::from(error))))
                            .child(
                                widgets::section_card(&theme)
                                    .child(widgets::card_row(&theme, true)
                                        .child(widgets::row_tile(&theme, icons::ARCHIVE_MINIMALISTIC))
                                        .child(div().flex_1().min_w_0()
                                            .child(widgets::row_title(&theme, "Backup status"))
                                            .child(widgets::meta_line(&theme, vec![div().child(SharedString::from(status_label)).into_any_element()])))
                                        .child(widgets::badge(&theme, if ready { "Isolated" } else { "Paused" })))
                                    .child(widgets::card_row(&theme, false)
                                        .child(div().flex_1().min_w_0()
                                            .child(widgets::row_title(&theme, "Source copy"))
                                            .child(widgets::meta_line(&theme, vec![div().child(SharedString::from(source)).into_any_element()]))))
                                    .child(widgets::card_row(&theme, false)
                                        .child(div().flex_1().min_w_0()
                                            .child(widgets::row_title(&theme, "Backup folder"))
                                            .child(widgets::meta_line(&theme, vec![div().child(SharedString::from(destination)).into_any_element()]))))
                                    .child(widgets::card_row(&theme, false)
                                        .child(div().flex_1().min_w_0()
                                            .child(widgets::row_title(&theme, "Recovery key"))
                                            .child(widgets::meta_line(&theme, vec![div().child(key_line).into_any_element()]))))
                                    .child(widgets::card_row(&theme, false)
                                        .child(div().flex_1().min_w_0()
                                            .child(widgets::row_title(&theme, "Last verified archive"))
                                            .child(widgets::meta_line(&theme, vec![div().child(SharedString::from(last_line)).into_any_element()]))))
                            )
                            .child(div().mt(px(16.0)).flex().flex_row().flex_wrap().gap(px(8.0))
                                .child(widgets::ghost_action(&theme)
                                    .id("backup-now")
                                    .opacity(if ready { 1.0 } else { 0.45 })
                                    .hover(|s| s.bg(theme::wash(0.06)))
                                    .child("Back up now")
                                    .when(ready, |button| button.on_click(cx.listener(|page, _, _, cx| page.run_backup(false, cx)))))
                                .child(widgets::ghost_action(&theme)
                                    .id("backup-export")
                                    .opacity(if ready { 1.0 } else { 0.45 })
                                    .hover(|s| s.bg(theme::wash(0.06)))
                                    .child("Export to backup folder")
                                    .when(ready, |button| button.on_click(cx.listener(|page, _, _, cx| page.run_backup(true, cx)))))
                                .child(widgets::ghost_action(&theme)
                                    .id("backup-inspect")
                                    .opacity(if can_inspect { 1.0 } else { 0.45 })
                                    .hover(|s| s.bg(theme::wash(0.06)))
                                    .child("Inspect last backup")
                                    .when(can_inspect, |button| button.on_click(cx.listener(|page, _, _, cx| page.inspect_latest(cx)))))
                                .child(widgets::ghost_action(&theme)
                                    .id("backup-stage")
                                    .opacity(if can_stage { 1.0 } else { 0.45 })
                                    .hover(|s| s.bg(theme::wash(0.06)))
                                    .child(if has_stage { "Verify staged copy" } else { "Stage restore" })
                                    .when(can_stage, |button| button.on_click(cx.listener(|page, _, _, cx| page.stage_latest(cx)))))
                            )
                            .when_some(self.last_result.as_ref().map(|r| r.archive_path.clone()), |page, path| page.child(widgets::meta_line(&theme, vec![div().child(SharedString::from(format!("Saved: {path}"))).into_any_element()])))
                            .when_some(preview, |page, preview| page.child(
                                widgets::section_card(&theme)
                                    .child(widgets::card_row(&theme, true).child(div().flex_1()
                                        .child(widgets::row_title(&theme, "Archive inspection"))
                                        .child(widgets::meta_line(&theme, vec![div().child(SharedString::from(format!("{} documents · {} task events · {} attachments · {} bytes", preview.document_count, preview.task_event_count, preview.attachment_count, preview.total_bytes))).into_any_element()]))))
                                    .child(widgets::card_row(&theme, false).child(div().flex_1()
                                        .child(widgets::row_title(&theme, "Excluded"))
                                        .child(widgets::meta_line(&theme, vec![div().child(SharedString::from(preview.excluded_categories.join(", "))).into_any_element()]))))
                                    .when(!preview.unsupported.is_empty(), |card| card.child(widgets::card_row(&theme, false).child(div().child(widgets::row_title(&theme, "Recovery limits")).child(widgets::meta_line(&theme, vec![div().child(SharedString::from(preview.unsupported.join("; "))).into_any_element()])))))
                            ))
                            .when_some(staged, |page, staged| page.child(
                                widgets::section_card(&theme)
                                    .child(widgets::card_row(&theme, true).child(div().flex_1()
                                        .child(widgets::row_title(&theme, "Staged restore checkpoint"))
                                        .child(widgets::meta_line(&theme, vec![div().child(SharedString::from(staged.staging_path)).into_any_element()]))))
                                    .child(widgets::card_row(&theme, false).child(div()
                                        .child(widgets::row_title(&theme, "Activation unavailable"))
                                        .child(widgets::meta_line(&theme, vec![div().child(SharedString::from(staged.reason)).into_any_element()]))))
                            ))
                            .child(widgets::warning_strip(&theme, SharedString::from(restore_reason)))
                    ),
            )
            .children(scroll_bar)
    }
}
