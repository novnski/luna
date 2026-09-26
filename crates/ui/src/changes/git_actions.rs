//! Git actions card in the existing Changes surface.

use super::*;
use luna_proto::{
    GitActionKind, GitActionReceipt, GitActionRequest, GitCheckpointDiff, GitCheckpointList,
    GitCheckpointRestorePreview, GitCheckpointRestoreReceipt, GitCheckpointRestoreRequest,
    GitMessageRequest, GitPrStatus, GitPrStatusRequest,
};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct GitUiScope {
    engine_generation: u64,
    engine_device: String,
    selection: String,
    checkout_id: Option<String>,
    branch: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct GitWriteOwner {
    scope: GitUiScope,
    generation: u64,
}

impl Changes {
    pub(super) fn git_context(&self, cx: &App) -> Option<(String, Option<String>, String)> {
        let state = self.state.read(cx);
        let chat = state.selected_chat_row()?;
        let cwd = chat.cwd.clone()?;
        let target = (state.local_device_id.as_deref() != Some(chat.device_id.as_str()))
            .then(|| chat.device_id.clone());
        let key = format!("{}\0{}\0{}", chat.device_id, cwd, chat.id);
        Some((cwd, target, key))
    }

    pub(super) fn git_chat_id(&self, cx: &App) -> Option<String> {
        self.state
            .read(cx)
            .selected_chat_row()
            .map(|chat| chat.id.clone())
    }

    fn live_git_scope(&self, cx: &App) -> Option<GitUiScope> {
        let (_, _, selection) = self.git_context(cx)?;
        let state = self.state.read(cx);
        let engine = state.engine()?;
        let chat = state.selected_chat_row()?;
        Some(GitUiScope {
            engine_generation: state.engine_generation(),
            engine_device: engine.engine_info().device_id.clone(),
            selection,
            checkout_id: chat.checkout_id.clone(),
            branch: chat.branch.clone(),
        })
    }

    /// Retire view/read ownership, never the independently running write.
    pub(super) fn reconcile_git_context(&mut self, cx: &mut Context<Self>) {
        let scope = self.live_git_scope(cx);
        if self.git_scope == scope {
            return;
        }
        self.git_scope = scope.clone();
        self.git_target = scope.as_ref().map(|scope| scope.selection.clone());
        self.git_read_generation += 1;
        self.checkpoint_generation += 1;
        self.git_task = None;
        self.checkpoint_task = None;
        self.git_pr_task = None;
        self.git_write_owner = None;
        self.git_busy = false;
        self.git_needs_refresh = scope.is_some();
        self.git_preview = None;
        self.git_selected.clear();
        self.git_notice = None;
        self.git_pr_status = None;
        self.checkpoints = None;
        self.checkpoint_diff = None;
        self.checkpoint_restore = None;
        self.checkpoint_restore_scope = None;
        self.checkpoint_notice = None;
        self.checkpoint_receipt = None;
        self.reconcile_pr_context(cx);
        cx.notify();
    }

    fn git_scope_current(&self, scope: &GitUiScope, cx: &App) -> bool {
        self.git_scope.as_ref() == Some(scope) && self.live_git_scope(cx).as_ref() == Some(scope)
    }

    fn git_read_current(&self, scope: &GitUiScope, generation: u64, cx: &App) -> bool {
        self.git_scope_current(scope, cx)
            && self.git_read_generation == generation
            && self.git_write_owner.is_none()
    }

    fn checkpoint_read_current(&self, scope: &GitUiScope, generation: u64, cx: &App) -> bool {
        self.git_scope_current(scope, cx)
            && self.checkpoint_generation == generation
            && self.git_write_owner.is_none()
    }

    fn begin_git_write(&mut self, cx: &mut Context<Self>) -> Option<GitWriteOwner> {
        if self.git_busy || self.git_write_owner.is_some() {
            return None;
        }
        let scope = self.live_git_scope(cx)?;
        self.git_write_generation += 1;
        let owner = GitWriteOwner {
            scope,
            generation: self.git_write_generation,
        };
        self.git_write_owner = Some(owner.clone());
        self.git_read_generation += 1;
        self.checkpoint_generation += 1;
        self.git_task = None;
        self.checkpoint_task = None;
        self.checkpoint_restore = None;
        self.checkpoint_restore_scope = None;
        self.git_busy = true;
        cx.notify();
        Some(owner)
    }

    fn finish_git_write(&mut self, owner: &GitWriteOwner, cx: &App) -> bool {
        if self.git_write_owner.as_ref() != Some(owner) || !self.git_scope_current(&owner.scope, cx)
        {
            return false;
        }
        self.git_write_owner = None;
        self.git_busy = false;
        true
    }

    pub(super) fn load_git(&mut self, cx: &mut Context<Self>) {
        self.reconcile_git_context(cx);
        if self.git_write_owner.is_some() {
            return;
        }
        let Some(scope) = self.live_git_scope(cx) else {
            return;
        };
        let Some((cwd, target, key)) = self.git_context(cx) else {
            return;
        };
        let Some(chat_id) = self.git_chat_id(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.git_read_generation += 1;
        let generation = self.git_read_generation;
        self.git_needs_refresh = false;
        self.git_target = Some(key.clone());
        self.git_busy = true;
        self.git_pr_status = None;
        self.git_pr_task = None;
        self.git_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::json!({ "cwd": cwd, "chatId": chat_id });
            if let Some(checkout_id) = scope.checkout_id.as_deref() {
                params["expectedCheckoutId"] = checkout_id.into();
            }
            if let Some(target) = target {
                params["targetDeviceId"] = target.into();
            }
            let result = engine
                .client()
                .call(methods::GET_GIT_ACTION_PREVIEW, params)
                .await;
            this.update(cx, |pane, cx| {
                if !pane.git_read_current(&scope, generation, cx) {
                    return;
                }
                pane.git_busy = false;
                match result.and_then(|value| {
                    serde_json::from_value::<GitActionPreview>(value)
                        .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))
                }) {
                    Ok(preview) => {
                        pane.git_selected = preview
                            .files
                            .iter()
                            .filter(|file| file.staged)
                            .map(|file| file.path.clone())
                            .collect();
                        pane.git_preview = Some(preview);
                        pane.reconcile_pr_context(cx);
                        pane.git_notice = None;
                        pane.load_git_pr(cx);
                        pane.load_checkpoints(cx);
                    }
                    Err(error) => {
                        pane.git_preview = None;
                        pane.reconcile_pr_context(cx);
                        pane.git_pr_status = None;
                        pane.git_notice = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn checkpoint_context(&self, cx: &App) -> Option<(String, String, String, bool)> {
        let state = self.state.read(cx);
        let chat = state.selected_chat_row()?;
        let cwd = chat.cwd.clone()?;
        let local = state.local_device_id.as_deref() == Some(chat.device_id.as_str());
        Some((
            chat.id.clone(),
            cwd.clone(),
            format!("{}\0{cwd}\0{}", chat.device_id, chat.id),
            local,
        ))
    }

    fn load_checkpoints(&mut self, cx: &mut Context<Self>) {
        self.reconcile_git_context(cx);
        if self.git_write_owner.is_some() {
            return;
        }
        let Some(scope) = self.live_git_scope(cx) else {
            return;
        };
        self.checkpoint_generation += 1;
        let generation = self.checkpoint_generation;
        let Some((chat_id, cwd, _, local)) = self.checkpoint_context(cx) else {
            return;
        };
        self.checkpoint_diff = None;
        self.checkpoint_restore = None;
        self.checkpoint_restore_scope = None;
        if !local {
            self.checkpoints = None;
            self.checkpoint_notice =
                Some("Git checkpoints are available only on this device".into());
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.checkpoint_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::LIST_GIT_CHECKPOINTS,
                    serde_json::json!({ "chatId": chat_id, "cwd": cwd }),
                )
                .await;
            this.update(cx, |pane, cx| {
                if !pane.checkpoint_read_current(&scope, generation, cx) {
                    return;
                }
                match result.and_then(|value| {
                    serde_json::from_value::<GitCheckpointList>(value)
                        .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))
                }) {
                    Ok(list) => {
                        pane.checkpoints = Some(list);
                        pane.checkpoint_notice = None;
                    }
                    Err(error) => {
                        pane.checkpoints = None;
                        pane.checkpoint_notice = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn diff_checkpoint(&mut self, turn_id: String, cx: &mut Context<Self>) {
        self.reconcile_git_context(cx);
        if self.git_write_owner.is_some() {
            return;
        }
        let Some(scope) = self.live_git_scope(cx) else {
            return;
        };
        self.checkpoint_generation += 1;
        let generation = self.checkpoint_generation;
        let Some((chat_id, cwd, _, true)) = self.checkpoint_context(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.checkpoint_diff = None;
        self.checkpoint_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::DIFF_GIT_CHECKPOINT,
                    serde_json::json!({ "chatId": chat_id, "cwd": cwd, "turnId": turn_id }),
                )
                .await;
            this.update(cx, |pane, cx| {
                if !pane.checkpoint_read_current(&scope, generation, cx) {
                    return;
                }
                match result.and_then(|value| {
                    serde_json::from_value::<GitCheckpointDiff>(value)
                        .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))
                }) {
                    Ok(diff) => {
                        pane.checkpoint_diff = Some(diff);
                        pane.checkpoint_notice = None;
                    }
                    Err(error) => pane.checkpoint_notice = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn preview_checkpoint_restore(&mut self, turn_id: String, cx: &mut Context<Self>) {
        self.reconcile_git_context(cx);
        if self.git_write_owner.is_some() {
            return;
        }
        let Some(scope) = self.live_git_scope(cx) else {
            return;
        };
        self.checkpoint_generation += 1;
        let generation = self.checkpoint_generation;
        let Some((chat_id, cwd, _, true)) = self.checkpoint_context(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.checkpoint_restore = None;
        self.checkpoint_restore_scope = None;
        self.checkpoint_task = Some(cx.spawn(async move |this, cx| {
            let result = engine.client().call(methods::PREVIEW_GIT_CHECKPOINT_RESTORE,
                serde_json::json!({ "chatId": chat_id, "cwd": cwd, "turnId": turn_id, "point": "before" })).await;
            this.update(cx, |pane, cx| {
                if !pane.checkpoint_read_current(&scope, generation, cx) { return; }
                match result.and_then(|value| serde_json::from_value::<GitCheckpointRestorePreview>(value)
                    .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))) {
                    Ok(preview) => { pane.checkpoint_restore = Some(preview); pane.checkpoint_restore_scope = Some(scope.clone()); pane.checkpoint_notice = None; }
                    Err(error) => pane.checkpoint_notice = Some(error.to_string()),
                }
                cx.notify();
            }).ok();
        }));
    }

    fn restore_checkpoint(&mut self, cx: &mut Context<Self>) {
        self.reconcile_git_context(cx);
        if self.git_busy || self.git_write_owner.is_some() {
            return;
        }
        let Some(scope) = self.live_git_scope(cx) else {
            return;
        };
        if self.checkpoint_restore_scope.as_ref() != Some(&scope) {
            return;
        }
        let Some(preview) = self.checkpoint_restore.clone() else {
            return;
        };
        let Some((chat_id, cwd, _, true)) = self.checkpoint_context(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let request = GitCheckpointRestoreRequest {
            chat_id,
            cwd,
            turn_id: preview.turn_id,
            point: preview.point,
            expected_token: preview.token,
        };
        let Some(owner) = self.begin_git_write(cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(
                    methods::RESTORE_GIT_CHECKPOINT,
                    serde_json::to_value(request).unwrap_or_default(),
                )
                .await;
            this.update(cx, |pane, cx| {
                if !pane.finish_git_write(&owner, cx) {
                    return;
                }
                match result.and_then(|value| {
                    serde_json::from_value::<GitCheckpointRestoreReceipt>(value)
                        .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))
                }) {
                    Ok(receipt) => {
                        pane.checkpoint_receipt = Some(format!(
                            "Files restored. Recovery ref: {}",
                            receipt.recovery_ref
                        ));
                        pane.checkpoint_notice = None;
                        pane.load_git(cx);
                    }
                    Err(error) => pane.checkpoint_notice = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_git_pr(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.git_preview.as_ref() else {
            return;
        };
        let Some((cwd, target, key)) = self.git_context(cx) else {
            return;
        };
        let Some(chat_id) = self.git_chat_id(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let engine_generation = self.state.read(cx).engine_generation();
        let expected_token = preview.token.clone();
        let request = GitPrStatusRequest {
            cwd,
            checkout_id: preview.checkout_id.clone(),
            expected_token: expected_token.clone(),
            branch: preview.branch.clone(),
            remote: preview.remote.clone(),
        };
        self.git_pr_status = None;
        self.git_pr_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::to_value(request).unwrap_or_default();
            params["chatId"] = chat_id.into();
            if let Some(target) = target {
                params["targetDeviceId"] = target.into();
            }
            let result = engine
                .client()
                .call(methods::GET_GIT_PR_STATUS, params)
                .await;
            this.update(cx, |pane, cx| {
                if pane.state.read(cx).engine_generation() != engine_generation
                    || pane.git_context(cx).map(|(_, _, key)| key).as_deref() != Some(key.as_str())
                    || pane.git_target.as_deref() != Some(key.as_str())
                    || pane
                        .git_preview
                        .as_ref()
                        .is_none_or(|preview| preview.token != expected_token)
                {
                    return;
                }
                pane.git_pr_status = Some(
                    match result.and_then(|value| {
                        serde_json::from_value::<GitPrStatus>(value)
                            .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))
                    }) {
                        Ok(status) => status,
                        Err(luna_rpc::RpcError::UnknownMethod(_)) => GitPrStatus::Unavailable {
                            reason: "This device does not support PR status".into(),
                        },
                        Err(_) => GitPrStatus::Unavailable {
                            reason: "PR lookup is unavailable; refresh to retry".into(),
                        },
                    },
                );
                cx.notify();
            })
            .ok();
        }));
    }

    fn git_action(&mut self, kind: GitActionKind, paths: Vec<String>, cx: &mut Context<Self>) {
        self.reconcile_git_context(cx);
        if self.git_busy || self.git_write_owner.is_some() {
            return;
        }
        let Some(preview) = self.git_preview.clone() else {
            return;
        };
        let Some((cwd, target, key)) = self.git_context(cx) else {
            return;
        };
        let Some(chat_id) = self.git_chat_id(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        if self.git_target.as_deref() != Some(key.as_str()) {
            return;
        }
        let before_message = self.git_message.read(cx).text().to_string();
        let request = GitActionRequest {
            cwd,
            checkout_id: preview.checkout_id,
            expected_token: preview.token,
            kind,
            paths,
            message: Some(before_message.clone()),
            remote: preview.remote,
        };
        let Some(owner) = self.begin_git_write(cx) else {
            return;
        };
        self.git_notice = None;
        cx.spawn(async move |this, cx| {
            let mut params = serde_json::to_value(request).unwrap_or_default();
            params["chatId"] = chat_id.into();
            if let Some(target) = target {
                params["targetDeviceId"] = target.into();
            }
            let result = engine
                .client()
                .call(methods::EXECUTE_GIT_ACTION, params)
                .await;
            this.update(cx, |pane, cx| {
                if !pane.finish_git_write(&owner, cx) {
                    return;
                }
                match result.and_then(|value| {
                    serde_json::from_value::<GitActionReceipt>(value)
                        .map_err(|error| luna_rpc::RpcError::Failed(error.to_string()))
                }) {
                    Ok(receipt) => {
                        pane.git_selected = receipt
                            .preview
                            .files
                            .iter()
                            .filter(|file| file.staged)
                            .map(|file| file.path.clone())
                            .collect();
                        pane.git_preview = Some(receipt.preview);
                        pane.reconcile_pr_context(cx);
                        pane.load_git_pr(cx);
                        pane.git_notice = receipt.error.or_else(|| {
                            Some(if receipt.pushed {
                                "Push completed".into()
                            } else if receipt.committed {
                                format!(
                                    "Committed {}",
                                    receipt
                                        .commit_sha
                                        .unwrap_or_default()
                                        .chars()
                                        .take(7)
                                        .collect::<String>()
                                )
                            } else {
                                "Git selection updated".into()
                            })
                        });
                        if receipt.committed && pane.git_message.read(cx).text() == before_message {
                            pane.git_message
                                .update(cx, |input, cx| input.set_text("", cx));
                        }
                    }
                    Err(error) => pane.git_notice = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn generate_git_message(&mut self, cx: &mut Context<Self>) {
        self.reconcile_git_context(cx);
        if self.git_busy || self.git_write_owner.is_some() {
            return;
        }
        let Some(preview) = self.git_preview.clone() else {
            return;
        };
        let Some(scope) = self.live_git_scope(cx) else {
            return;
        };
        let expected_token = preview.token.clone();
        let Some((cwd, target, _)) = self.git_context(cx) else {
            return;
        };
        let Some(chat_id) = self.git_chat_id(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let before = self.git_message.read(cx).text().to_string();
        let request = GitMessageRequest {
            cwd,
            checkout_id: preview.checkout_id,
            expected_token: preview.token,
            paths: self.git_selected.iter().cloned().collect(),
            harness: self
                .state
                .read(cx)
                .selected_chat_row()
                .and_then(|chat| chat.config.as_ref().map(|config| config.harness)),
        };
        self.git_read_generation += 1;
        let generation = self.git_read_generation;
        self.git_busy = true;
        self.git_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::to_value(request).unwrap_or_default();
            params["chatId"] = chat_id.into();
            if let Some(target) = target {
                params["targetDeviceId"] = target.into();
            }
            let result = engine
                .client()
                .call(methods::GENERATE_GIT_COMMIT_MESSAGE, params)
                .await;
            this.update(cx, |pane, cx| {
                if !pane.git_read_current(&scope, generation, cx) {
                    return;
                }
                pane.git_busy = false;
                if pane
                    .git_preview
                    .as_ref()
                    .is_none_or(|preview| preview.token != expected_token)
                {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(value) => {
                        if let Some(message) = value.get("message").and_then(|value| value.as_str())
                            && pane.git_message.read(cx).text() == before
                        {
                            pane.git_message
                                .update(cx, |input, cx| input.set_text(message, cx));
                            pane.git_notice =
                                Some("Generated draft; review and edit before committing".into());
                        }
                    }
                    Err(error) => pane.git_notice = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Changes {
    pub(super) fn git_button(
        id: impl Into<SharedString>,
        label: &'static str,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id.into())
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(6.0))
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .text_size(px(11.0))
            .text_color(theme.text)
            .cursor_pointer()
            .child(label)
    }

    fn render_pr_status(&self, theme: &Theme) -> AnyElement {
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .text_size(px(11.0))
            .text_color(theme.text_dim);
        match self.git_pr_status.clone() {
            Some(GitPrStatus::Linked { change_request }) => {
                let model =
                    crate::change_requests::ChangeRequestBadgeModel::from_summary(&change_request);
                row = row
                    .child(crate::change_requests::pull_request_badge(
                        "git-pr-link".into(),
                        change_request,
                        crate::change_requests::ChangeRequestBadgeSurface::Composer,
                        theme,
                    ))
                    .child(
                        div()
                            .text_color(model.tone.color(theme))
                            .child(model.state_label),
                    )
                    .child(div().min_w_0().truncate().child(model.title));
            }
            Some(GitPrStatus::NoMatch) => row = row.child("No PR found in the selected remote"),
            Some(GitPrStatus::UnsupportedHost) => {
                row = row.child("PR status unsupported for this remote")
            }
            Some(GitPrStatus::Unavailable { reason }) => row = row.child(reason),
            None => row = row.child("Checking PR status…"),
        }
        row.into_any_element()
    }

    pub(super) fn render_git_card(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let preview = self.git_preview.clone();
        let busy = self.git_busy;
        let mut card = div()
            .id("git-card")
            .flex_none()
            .w_full()
            .max_h(px(530.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .px(px(Theme::SPACE_MD))
            .py(px(Theme::SPACE_SM))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.surface);
        if let Some(preview) = preview {
            let branch = preview.branch.clone();
            let remote = preview
                .remote
                .clone()
                .unwrap_or_else(|| "No push remote".into());
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_dim)
                    .child(format!("Branch: {branch}  ·  Remote: {remote}"))
                    .child(
                        Self::git_button("git-refresh", "Refresh", theme)
                            .on_click(cx.listener(|this, _, _, cx| this.load_git(cx))),
                    ),
            );
            card = card.child(self.render_pr_status(theme));
            card = card.child(self.render_pr_panel(theme, cx));
            card = card.child(self.render_checkpoint_section(theme, cx));
            let selected = self.git_selected.clone();
            let rows = preview.files.into_iter().map(|file| {
                let path = file.path.clone();
                let toggle_path = path.clone();
                let stage_path = path.clone();
                let unstaged = file.unstaged;
                let staged = file.staged;
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .min_w_0()
                    .child(
                        div()
                            .id(SharedString::from(format!("git-select-{path}")))
                            .cursor_pointer()
                            .text_size(px(11.0))
                            .text_color(theme.text)
                            .child(if selected.contains(&path) {
                                "☑"
                            } else {
                                "☐"
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if staged {
                                    if !this.git_selected.insert(toggle_path.clone()) {
                                        this.git_selected.remove(&toggle_path);
                                    }
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme.font_mono.clone())
                            .text_size(px(11.0))
                            .text_color(theme.text)
                            .child(path),
                    )
                    .when(unstaged, |el| {
                        el.child(
                            Self::git_button(format!("git-stage-{stage_path}"), "Stage", theme)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.git_action(
                                        GitActionKind::Stage,
                                        vec![stage_path.clone()],
                                        cx,
                                    )
                                })),
                        )
                    })
                    .when(staged, |el| {
                        el.child(
                            Self::git_button(
                                format!("git-unstage-{}", file.path),
                                "Unstage",
                                theme,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.git_action(
                                        GitActionKind::Unstage,
                                        vec![file.path.clone()],
                                        cx,
                                    )
                                },
                            )),
                        )
                    })
            });
            card = card.child(
                div()
                    .id("git-file-list")
                    .max_h(px(130.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .children(rows),
            );
            let selected: Vec<_> = self.git_selected.iter().cloned().collect();
            card = card
                .child(div().h(px(30.0)).child(self.git_message.clone()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            Self::git_button("git-generate", "Generate message", theme).on_click(
                                cx.listener(|this, _, _, cx| this.generate_git_message(cx)),
                            ),
                        )
                        .child(Self::git_button("git-commit", "Commit", theme).on_click(
                            cx.listener({
                                let selected = selected.clone();
                                move |this, _, _, cx| {
                                    this.git_action(GitActionKind::Commit, selected.clone(), cx)
                                }
                            }),
                        ))
                        .child(
                            Self::git_button("git-push", "Push", theme).on_click(cx.listener(
                                |this, _, _, cx| this.git_action(GitActionKind::Push, vec![], cx),
                            )),
                        )
                        .child(
                            Self::git_button("git-commit-push", "Commit & Push", theme).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.git_action(
                                        GitActionKind::CommitAndPush,
                                        selected.clone(),
                                        cx,
                                    )
                                }),
                            ),
                        ),
                );
        } else {
            card = card.child(if self.git_context(cx).is_none() {
                "Select a task with a Git checkout"
            } else if busy {
                "Loading checkout status…"
            } else {
                "Git status unavailable; refresh to retry"
            });
            if self.git_context(cx).is_some() && self.state.read(cx).engine().is_some() {
                card = card.child(
                    Self::git_button("git-refresh", "Refresh", theme)
                        .on_click(cx.listener(|this, _, _, cx| this.load_git(cx))),
                );
            }
        }
        if let Some(notice) = &self.git_notice {
            card = card.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_dim)
                    .child(SharedString::from(notice.clone())),
            );
        }
        if busy {
            card = card.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("Working…"),
            );
        }
        card.into_any_element()
    }

    fn render_checkpoint_section(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .py(px(Theme::SPACE_SM))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text)
                    .child("File checkpoints"),
            );
        if let Some(list) = &self.checkpoints {
            if let Some(reason) = &list.restore_unavailable_reason {
                section = section.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_dim)
                        .child(SharedString::from(reason.clone())),
                );
            }
            if list.checkpoints.is_empty() {
                section = section.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_dim)
                        .child("No completed turn checkpoints yet"),
                );
            }
            for row in list.checkpoints.iter().take(5) {
                let turn_id = row.turn_id.clone();
                let stamp =
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(row.started_at_ms)
                        .map(|at| at.format("%b %-d %H:%M").to_string())
                        .unwrap_or_else(|| "Unknown time".into());
                let outcome = row.outcome.as_deref().unwrap_or("in progress");
                let mut line = div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_dim)
                    .child(format!("{stamp} · {outcome}"));
                if row.before_available && row.after_available {
                    line = line.child(
                        Self::git_button(format!("checkpoint-diff-{turn_id}"), "Diff", theme)
                            .on_click(cx.listener({
                                let turn_id = turn_id.clone();
                                move |this, _, _, cx| this.diff_checkpoint(turn_id.clone(), cx)
                            })),
                    );
                }
                if row.before_available && list.restore_available {
                    line = line.child(
                        Self::git_button(
                            format!("checkpoint-preview-{turn_id}"),
                            "Preview restore",
                            theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.preview_checkpoint_restore(turn_id.clone(), cx)
                        })),
                    );
                }
                section = section.child(line);
                if let Some(error) = &row.error {
                    section = section.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_dim)
                            .child(format!("Capture failed: {error}")),
                    );
                }
            }
            if list.checkpoints.len() > 5 {
                section = section.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child(format!(
                            "Showing 5 of {} recent turns",
                            list.checkpoints.len()
                        )),
                );
            }
        } else {
            section = section.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_dim)
                    .child("Checkpoints unavailable or loading…"),
            );
        }
        if let Some(diff) = &self.checkpoint_diff {
            let snippet: String = diff.patch.chars().take(8000).collect();
            section = section.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_dim)
                            .child(format!(
                                "{} changed paths · observed checkout changes",
                                diff.paths.len()
                            )),
                    )
                    .child(
                        div()
                            .id("checkpoint-diff-view")
                            .max_h(px(140.0))
                            .overflow_y_scroll()
                            .text_size(px(10.0))
                            .font_family(theme.font_mono.clone())
                            .text_color(theme.text)
                            .child(SharedString::from(snippet)),
                    ),
            );
        }
        if let Some(preview) = &self.checkpoint_restore {
            let path_text = if preview.paths.is_empty() {
                "No file changes".into()
            } else {
                preview
                    .paths
                    .iter()
                    .take(12)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            section = section.child(div().flex().flex_col().gap(px(4.0))
                .child(div().text_size(px(11.0)).text_color(theme.text_dim)
                    .child(format!("Restore will change {} paths: {path_text}", preview.paths.len())))
                .child(div().text_size(px(11.0)).text_color(theme.text_dim)
                    .child("This changes files only. Conversation history and external actions remain as they are."))
                .child(Self::git_button("checkpoint-restore", "Restore files", theme)
                    .on_click(cx.listener(|this, _, _, cx| this.restore_checkpoint(cx)))));
        }
        if let Some(notice) = &self.checkpoint_notice {
            section = section.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_dim)
                    .child(SharedString::from(notice.clone())),
            );
        }
        if let Some(receipt) = &self.checkpoint_receipt {
            section = section.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_dim)
                    .child(SharedString::from(receipt.clone())),
            );
        }
        section.into_any_element()
    }
}

#[cfg(test)]
mod lifetime_tests;

#[cfg(test)]
mod ownership_tests;
