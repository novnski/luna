//! CC-10 provider-aware rewind copy and Claude's native file checkpoints in
//! the shared conversation-rewind dialog.
//!
//! Conversation rewind always keeps files. Claude's own file checkpoints are
//! a separate, explicitly previewed restore that covers only Claude's file
//! tools; LUNA's Git checkpoints stay a third, separate operation.

use super::*;
use luna_proto::{HarnessId, NativeFileRewindView};

#[derive(Default)]
pub(super) enum NativeFiles {
    #[default]
    Idle,
    Loading,
    Preview(NativeFileRewindView),
    Restoring(NativeFileRewindView),
    Restored(NativeFileRewindView),
    Failed(String),
}

impl NativeFiles {
    #[cfg(feature = "appshots-fixture")]
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::Preview(_) => "preview",
            Self::Restoring(_) => "restoring",
            Self::Restored(_) => "restored",
            Self::Failed(_) => "failed",
        }
    }
}

/// Body copy for the shared rewind dialog, named for the task's provider.
pub(super) fn rewind_copy(provider: Option<HarnessId>, pending: bool) -> &'static str {
    match (pending, provider) {
        (true, _) => {
            "Read the saved provider history to finish or dismiss the unresolved rewind. Revert will not be sent again. Later LUNA rows stay visible until the retained prefix is verified."
        }
        (false, Some(HarnessId::ClaudeCode)) => {
            "Remove this message and every later turn from this same Claude Code session. Claude keeps the removed turns only as an unreachable branch; an audit snapshot is kept in LUNA. Your draft, files and external effects are kept."
        }
        (false, _) => {
            "Remove this message and every later turn from this same Codex conversation. The provider's removed history cannot be restored by LUNA; an audit snapshot is kept. Your draft, files and external effects are kept."
        }
    }
}

fn accessible_button(
    button: gpui::Div,
    id: &'static str,
    label: &'static str,
    on_activate: impl Fn(&mut Shell, &mut Context<Shell>) + Clone + 'static,
    cx: &mut Context<Shell>,
) -> gpui::Stateful<gpui::Div> {
    let keyboard = on_activate.clone();
    button
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label)
        .tab_index(0)
        .on_click(cx.listener(move |shell, _, _, cx| on_activate(shell, cx)))
        .on_key_down(
            cx.listener(move |shell, event: &gpui::KeyDownEvent, _, cx| {
                if !event.is_held && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    keyboard(shell, cx);
                    cx.stop_propagation();
                }
            }),
        )
}

/// The Claude file-checkpoint section of the rewind dialog.
pub(super) fn section(files: &NativeFiles, theme: &Theme, cx: &mut Context<Shell>) -> AnyElement {
    let muted = |copy: String| {
        div()
            .mt(px(6.0))
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted)
            .child(SharedString::from(copy))
    };
    let mut body = div()
        .id("history-rewind-native-files")
        .mt(px(12.0))
        .p(px(10.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(crate::theme::hairline(0.10))
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(crate::typography::ui_rems(12.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child("Claude file checkpoint"),
        );
    let view = match files {
        NativeFiles::Preview(view) | NativeFiles::Restoring(view) | NativeFiles::Restored(view) => {
            Some(view)
        }
        _ => None,
    };
    body = body.child(muted(match files {
        NativeFiles::Idle => "Optionally restore files Claude's own file tools changed since this message. Conversation rewind never changes files.".into(),
        NativeFiles::Loading => "Asking Claude which files it would restore…".into(),
        NativeFiles::Failed(error) => format!("Claude file checkpoint unavailable: {error}"),
        // Claude's applied response may describe the remaining diff (zero),
        // rather than enumerate the files it just restored.
        NativeFiles::Restored(_) => "Claude confirmed the checkpoint restore.".into(),
        NativeFiles::Preview(view) | NativeFiles::Restoring(view) if !view.can_rewind => format!(
            "Claude cannot restore files here: {}",
            view.error.clone().unwrap_or_else(|| "no checkpoint".into())
        ),
        NativeFiles::Preview(view) | NativeFiles::Restoring(view) if view.files_changed.is_empty() => {
            "Files already match Claude's checkpoint for this message.".into()
        }
        NativeFiles::Preview(view) | NativeFiles::Restoring(view) => format!(
            "{} file{} would change (+{} −{}):",
            view.files_changed.len(),
            if view.files_changed.len() == 1 { "" } else { "s" },
            view.insertions,
            view.deletions
        ),
    }));
    if let Some(view) = view.filter(|view| view.can_rewind && !view.files_changed.is_empty()) {
        let list = view.files_changed.iter().take(6).fold(
            div()
                .id("history-rewind-native-file-list")
                .mt(px(4.0))
                .max_h(px(96.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .font_family(theme.font_mono.clone())
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text),
            |list, path| {
                let name = std::path::Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                list.child(div().child(SharedString::from(name)))
            },
        );
        body = body.child(list);
        if view.files_changed.len() > 6 {
            body = body.child(muted(format!("and {} more", view.files_changed.len() - 6)));
        }
    }
    if let Some(view) = view {
        body = body.child(muted(view.limitations.clone()));
    }
    let buttons = div().mt(px(8.0)).flex().justify_end().gap(px(8.0));
    let buttons = match files {
        NativeFiles::Idle | NativeFiles::Failed(_) => buttons.child(accessible_button(
            popover::btn_ghost(
                theme,
                "Check Claude-edited files",
                "history-rewind-native-preview",
            ),
            "history-rewind-native-preview",
            "Check which files Claude's checkpoint would restore",
            |shell, cx| shell.preview_native_files(cx),
            cx,
        )),
        NativeFiles::Preview(view) if view.can_rewind && !view.files_changed.is_empty() => buttons
            .child(accessible_button(
                popover::btn_ghost(
                    theme,
                    "Restore these files",
                    "history-rewind-native-restore",
                ),
                "history-rewind-native-restore",
                "Restore these files from Claude's checkpoint",
                |shell, cx| shell.apply_native_files(cx),
                cx,
            )),
        NativeFiles::Restoring(_) => buttons.child(
            div()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted)
                .child("Restoring…"),
        ),
        _ => buttons,
    };
    body.child(buttons).into_any_element()
}

impl Shell {
    fn native_files_target(&self) -> Option<(String, String)> {
        self.conversation_rewind_dialog
            .as_ref()
            .filter(|dialog| dialog.provider == Some(HarnessId::ClaudeCode))
            .map(|dialog| (dialog.task_id.clone(), dialog.message_id.clone()))
    }

    fn set_native_files(&mut self, files: NativeFiles, cx: &mut Context<Self>) {
        if let Some(dialog) = self.conversation_rewind_dialog.as_mut() {
            dialog.native_files = files;
        }
        cx.notify();
    }

    pub(super) fn preview_native_files(&mut self, cx: &mut Context<Self>) {
        let Some((task_id, message_id)) = self.native_files_target() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.set_native_files(NativeFiles::Failed("Engine not connected".into()), cx);
            return;
        };
        self.set_native_files(NativeFiles::Loading, cx);
        let generation = self.history_flow_generation;
        let task = cx.spawn(async move |this, cx| {
            let preview = engine
                .client()
                .call_as::<NativeFileRewindView>(
                    methods::PREVIEW_NATIVE_FILE_REWIND,
                    serde_json::json!({ "taskId": task_id, "messageId": message_id }),
                )
                .await;
            this.update(cx, |shell, cx| {
                if shell.history_flow_generation != generation {
                    return;
                }
                let next = match preview {
                    Ok(view) => NativeFiles::Preview(view),
                    Err(error) => NativeFiles::Failed(error.to_string()),
                };
                shell.set_native_files(next, cx);
            })
            .ok();
        });
        self.keep_native_files_task(task);
    }

    /// The dialog owns the request; closing the dialog cancels it.
    fn keep_native_files_task(&mut self, task: Task<()>) {
        if let Some(dialog) = self.conversation_rewind_dialog.as_mut() {
            dialog.native_files_task = Some(task);
        }
    }

    pub(super) fn apply_native_files(&mut self, cx: &mut Context<Self>) {
        let Some((task_id, message_id)) = self.native_files_target() else {
            return;
        };
        let Some(NativeFiles::Preview(preview)) = self
            .conversation_rewind_dialog
            .as_ref()
            .map(|dialog| &dialog.native_files)
        else {
            return;
        };
        let preview = preview.clone();
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let digest = preview.digest.clone();
        self.set_native_files(NativeFiles::Restoring(preview), cx);
        let generation = self.history_flow_generation;
        let task = cx.spawn(async move |this, cx| {
            let applied = engine
                .client()
                .call_as::<NativeFileRewindView>(
                    methods::APPLY_NATIVE_FILE_REWIND,
                    serde_json::json!({
                        "taskId": task_id, "messageId": message_id, "digest": digest,
                    }),
                )
                .await;
            // The restore retires the parked Claude runtime, which advances
            // the task revision. Re-read it so the reviewed conversation
            // rewind stays available on the same binding.
            let current = engine
                .client()
                .call_as::<luna_proto::TaskCommandRead>(
                    methods::GET_TASK_COMMAND_SNAPSHOT,
                    serde_json::json!({ "taskId": task_id }),
                )
                .await
                .ok();
            this.update(cx, |shell, cx| {
                if shell.history_flow_generation != generation {
                    return;
                }
                if let (Some(read), Some(dialog)) =
                    (current, shell.conversation_rewind_dialog.as_mut())
                    && read.snapshot.binding_generation
                        == dialog.review["bindingGeneration"]
                            .as_u64()
                            .map(|g| g as u32)
                {
                    dialog.review["taskRevision"] = read.snapshot.task_revision.into();
                    dialog.snapshot = read.snapshot;
                }
                let next = match applied {
                    Ok(view) if view.can_rewind => NativeFiles::Restored(view),
                    Ok(view) => NativeFiles::Failed(
                        view.error
                            .unwrap_or_else(|| "Claude did not restore the files".into()),
                    ),
                    Err(error) => NativeFiles::Failed(error.to_string()),
                };
                shell.set_native_files(next, cx);
            })
            .ok();
        });
        self.keep_native_files_task(task);
    }
}

#[cfg(feature = "appshots-fixture")]
impl Shell {
    pub fn fixture_open_conversation_rewind(
        &mut self,
        task_id: String,
        message_id: String,
        text: String,
        cx: &mut Context<Self>,
    ) {
        self.open_conversation_rewind(task_id, message_id, text, cx);
    }

    /// `None` until the rewind dialog is open; then the native-files state.
    pub fn fixture_native_files_state(&self) -> Option<&'static str> {
        self.conversation_rewind_dialog
            .as_ref()
            .map(|dialog| dialog.native_files.label())
    }

    pub fn fixture_preview_native_files(&mut self, cx: &mut Context<Self>) {
        self.preview_native_files(cx);
    }

    pub fn fixture_apply_native_files(&mut self, cx: &mut Context<Self>) {
        self.apply_native_files(cx);
    }

    pub fn fixture_submit_conversation_rewind(&mut self, cx: &mut Context<Self>) {
        self.submit_conversation_rewind(cx);
    }

    pub fn fixture_rewind_dialog_open(&self) -> bool {
        self.conversation_rewind_dialog.is_some()
    }
}
