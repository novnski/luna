//! Git actions card in the existing Changes surface.

use super::*;
use zeron_proto::{
    GitActionKind, GitActionReceipt, GitActionRequest, GitMessageRequest, GitPrStatus,
    GitPrStatusRequest,
};

impl Changes {
    pub(super) fn git_context(&self, cx: &App) -> Option<(String, Option<String>, String)> {
        let state = self.state.read(cx);
        let chat = state.selected_chat_row()?;
        let cwd = chat.cwd.clone()?;
        let target = (state.local_device_id.as_deref() != Some(chat.device_id.as_str()))
            .then(|| chat.device_id.clone());
        let key = format!("{}\0{}", chat.device_id, cwd);
        Some((cwd, target, key))
    }

    pub(super) fn load_git(&mut self, cx: &mut Context<Self>) {
        let Some((cwd, target, key)) = self.git_context(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.git_target = Some(key.clone());
        self.git_busy = true;
        self.git_pr_status = None;
        self.git_pr_task = None;
        self.git_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::json!({ "cwd": cwd });
            if let Some(target) = target {
                params["targetDeviceId"] = target.into();
            }
            let result = engine
                .client()
                .call(methods::GET_GIT_ACTION_PREVIEW, params)
                .await;
            this.update(cx, |pane, cx| {
                if pane.git_target.as_deref() != Some(key.as_str()) {
                    return;
                }
                pane.git_busy = false;
                match result.and_then(|value| {
                    serde_json::from_value::<GitActionPreview>(value)
                        .map_err(|error| zeron_rpc::RpcError::Failed(error.to_string()))
                }) {
                    Ok(preview) => {
                        pane.git_selected = preview
                            .files
                            .iter()
                            .filter(|file| file.staged)
                            .map(|file| file.path.clone())
                            .collect();
                        pane.git_preview = Some(preview);
                        pane.git_notice = None;
                        pane.load_git_pr(cx);
                    }
                    Err(error) => {
                        pane.git_preview = None;
                        pane.git_pr_status = None;
                        pane.git_notice = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn load_git_pr(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.git_preview.as_ref() else {
            return;
        };
        let Some((cwd, target, key)) = self.git_context(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
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
            if let Some(target) = target {
                params["targetDeviceId"] = target.into();
            }
            let result = engine
                .client()
                .call(methods::GET_GIT_PR_STATUS, params)
                .await;
            this.update(cx, |pane, cx| {
                if pane.git_target.as_deref() != Some(key.as_str())
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
                            .map_err(|error| zeron_rpc::RpcError::Failed(error.to_string()))
                    }) {
                        Ok(status) => status,
                        Err(zeron_rpc::RpcError::UnknownMethod(_)) => GitPrStatus::Unavailable {
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
        if self.git_busy {
            return;
        }
        let Some(preview) = self.git_preview.clone() else {
            return;
        };
        let Some((cwd, target, key)) = self.git_context(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        if self.git_target.as_deref() != Some(key.as_str()) {
            return;
        }
        let request = GitActionRequest {
            cwd,
            checkout_id: preview.checkout_id,
            expected_token: preview.token,
            kind,
            paths,
            message: Some(self.git_message.read(cx).text().to_string()),
            remote: preview.remote,
        };
        self.git_busy = true;
        self.git_notice = None;
        self.git_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::to_value(request).unwrap_or_default();
            if let Some(target) = target {
                params["targetDeviceId"] = target.into();
            }
            let result = engine
                .client()
                .call(methods::EXECUTE_GIT_ACTION, params)
                .await;
            this.update(cx, |pane, cx| {
                if pane.git_target.as_deref() != Some(key.as_str()) {
                    return;
                }
                pane.git_busy = false;
                match result.and_then(|value| {
                    serde_json::from_value::<GitActionReceipt>(value)
                        .map_err(|error| zeron_rpc::RpcError::Failed(error.to_string()))
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
                        if receipt.committed {
                            pane.git_message
                                .update(cx, |input, cx| input.set_text("", cx));
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

    fn generate_git_message(&mut self, cx: &mut Context<Self>) {
        if self.git_busy {
            return;
        }
        let Some(preview) = self.git_preview.clone() else {
            return;
        };
        let Some((cwd, target, key)) = self.git_context(cx) else {
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
        self.git_busy = true;
        self.git_task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::to_value(request).unwrap_or_default();
            if let Some(target) = target {
                params["targetDeviceId"] = target.into();
            }
            let result = engine
                .client()
                .call(methods::GENERATE_GIT_COMMIT_MESSAGE, params)
                .await;
            this.update(cx, |pane, cx| {
                if pane.git_target.as_deref() != Some(key.as_str()) {
                    return;
                }
                pane.git_busy = false;
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
    fn git_button(
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
            .flex_none()
            .w_full()
            .max_h(px(350.0))
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
}
