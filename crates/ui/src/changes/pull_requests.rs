//! Draft PR preview/edit and read-only linked details in the existing Git card.
use super::*;
use luna_proto::{
    GitPrCreateOutcome, GitPrCreateReceipt, GitPrCreateRequest, GitPrDetails, GitPrDraftPreview,
};

pub(super) struct PullRequestPanel {
    open: bool,
    busy: bool,
    generation: u64,
    preview: Option<GitPrDraftPreview>,
    details: Option<GitPrDetails>,
    notice: Option<String>,
    reconcile_only: bool,
    title: Entity<ComposerInput>,
    body: Entity<ComposerInput>,
    base: Entity<ComposerInput>,
    head: Entity<ComposerInput>,
    task: Option<Task<()>>,
    _events: Vec<Subscription>,
}

impl PullRequestPanel {
    pub(super) fn new(cx: &mut Context<Changes>) -> Self {
        let title = cx.new(|cx| {
            ComposerInput::with_context("Pull request title…", "GitCommitMessage", cx)
                .with_single_line()
        });
        let body = cx.new(|cx| {
            ComposerInput::with_context("Describe your changes…", "GitCommitMessage", cx)
        });
        let base = cx.new(|cx| {
            ComposerInput::with_context("Base branch", "GitCommitMessage", cx).with_single_line()
        });
        let head = cx.new(|cx| {
            ComposerInput::with_context("Head branch", "GitCommitMessage", cx).with_single_line()
        });
        let events = [&title, &body, &base, &head]
            .into_iter()
            .map(|input| cx.subscribe(input, |_, _, _, cx| cx.notify()))
            .collect();
        Self {
            open: false,
            busy: false,
            generation: 0,
            preview: None,
            details: None,
            notice: None,
            reconcile_only: false,
            title,
            body,
            base,
            head,
            task: None,
            _events: events,
        }
    }
}

impl Changes {
    fn pr_button(
        id: &'static str,
        label: &'static str,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        Self::git_button(id, label, theme)
            .role(gpui::Role::Button)
            .aria_label(label)
    }

    pub(super) fn reset_pr_panel(&mut self, cx: &mut Context<Self>) {
        self.pr_panel.generation += 1;
        self.pr_panel.task = None;
        self.pr_panel.open = false;
        self.pr_panel.busy = false;
        self.pr_panel.preview = None;
        self.pr_panel.details = None;
        self.pr_panel.notice = None;
        self.pr_panel.reconcile_only = false;
        cx.notify();
    }

    fn close_pr_panel(&mut self, cx: &mut Context<Self>) {
        if self.pr_panel.busy && self.pr_panel.preview.is_some() {
            self.pr_panel.reconcile_only = true;
        }
        self.pr_panel.generation += 1;
        self.pr_panel.task = None;
        self.pr_panel.open = false;
        self.pr_panel.busy = false;
        cx.notify();
    }

    fn load_pr_panel(&mut self, number: Option<u64>, cx: &mut Context<Self>) {
        if self.pr_panel.busy {
            return;
        }
        let Some((cwd, target, key)) = self.git_context(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.pr_panel.open = true;
        self.pr_panel.busy = true;
        self.pr_panel.notice = None;
        self.pr_panel.generation += 1;
        let generation = self.pr_panel.generation;
        self.pr_panel.task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::json!({"cwd":cwd, "number":number});
            if let Some(target) = target {
                params["targetDeviceId"] = target.into();
            }
            let method = if number.is_some() {
                methods::GET_GITHUB_PR_DETAILS
            } else {
                methods::GET_GITHUB_PR_PREVIEW
            };
            let result = engine.client().call(method, params).await;
            this.update(cx, |pane, cx| {
                if pane.pr_panel.generation != generation
                    || pane.git_context(cx).map(|(_, _, key)| key).as_deref() != Some(&key)
                {
                    return;
                }
                pane.pr_panel.busy = false;
                match result {
                    Ok(value) if number.is_some() => {
                        match serde_json::from_value::<GitPrDetails>(value) {
                            Ok(details) => {
                                pane.pr_panel.details = Some(details);
                                pane.pr_panel.preview = None;
                            }
                            Err(_) => {
                                pane.pr_panel.notice =
                                    Some("Could not read PR details; retry".into())
                            }
                        }
                    }
                    Ok(value) => match serde_json::from_value::<GitPrDraftPreview>(value) {
                        Ok(preview) => {
                            let first = pane.pr_panel.preview.is_none();
                            if first {
                                pane.pr_panel
                                    .title
                                    .update(cx, |input, cx| input.set_text(&preview.title, cx));
                                pane.pr_panel
                                    .body
                                    .update(cx, |input, cx| input.set_text("", cx));
                                pane.pr_panel
                                    .base
                                    .update(cx, |input, cx| input.set_text(&preview.base, cx));
                                pane.pr_panel
                                    .head
                                    .update(cx, |input, cx| input.set_text(&preview.head, cx));
                            }
                            pane.pr_panel.details = None;
                            pane.pr_panel.preview = Some(preview);
                        }
                        Err(_) => {
                            pane.pr_panel.notice = Some("Could not read PR preview; retry".into())
                        }
                    },
                    Err(error) => pane.pr_panel.notice = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn submit_draft_pr(&mut self, cx: &mut Context<Self>) {
        if self.pr_panel.busy || self.git_busy {
            return;
        }
        let Some(preview) = self.pr_panel.preview.clone() else {
            return;
        };
        let Some((_, target, key)) = self.git_context(cx) else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let request = GitPrCreateRequest {
            preview,
            reconcile_only: self.pr_panel.reconcile_only,
            title: self.pr_panel.title.read(cx).text().to_owned(),
            body: self.pr_panel.body.read(cx).text().to_owned(),
            base: self.pr_panel.base.read(cx).text().trim().to_owned(),
            head: self.pr_panel.head.read(cx).text().trim().to_owned(),
        };
        if request.title.trim().is_empty() {
            self.pr_panel.notice = Some("Enter a pull request title".into());
            cx.notify();
            return;
        }
        self.pr_panel.busy = true;
        self.pr_panel.notice = None;
        self.pr_panel.generation += 1;
        let generation = self.pr_panel.generation;
        self.pr_panel.task = Some(cx.spawn(async move |this, cx| {
            let mut params = serde_json::to_value(request).expect("PR request serializes");
            if let Some(target) = target {
                params["targetDeviceId"] = target.into();
            }
            let result = engine
                .client()
                .call(methods::CREATE_GITHUB_DRAFT_PR, params)
                .await;
            this.update(cx, |pane, cx| {
                if pane.pr_panel.generation != generation
                    || pane.git_context(cx).map(|(_, _, key)| key).as_deref() != Some(&key)
                {
                    return;
                }
                pane.pr_panel.busy = false;
                match result.and_then(|v| {
                    serde_json::from_value::<GitPrCreateReceipt>(v).map_err(|_| {
                        luna_rpc::RpcError::Failed("Could not read the create result".into())
                    })
                }) {
                    Ok(receipt) => {
                        pane.pr_panel.reconcile_only = receipt.outcome
                            == GitPrCreateOutcome::Uncertain
                            || (pane.pr_panel.reconcile_only
                                && receipt.outcome == GitPrCreateOutcome::Rejected);
                        pane.pr_panel.notice = Some(receipt.message);
                        if let Some(details) = receipt.details {
                            pane.git_pr_status = Some(GitPrStatus::Linked {
                                change_request: details.summary.clone(),
                            });
                            pane.pr_panel.details = Some(details);
                            pane.pr_panel.preview = None;
                        }
                    }
                    Err(error) => {
                        // A transport error may hide a successful write. The next
                        // button must be a read-only reconciliation, never a retry POST.
                        pane.pr_panel.reconcile_only = true;
                        pane.pr_panel.notice = Some(format!(
                            "{error}. Check GitHub to resolve the result; your text is preserved"
                        ));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn render_pr_panel(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let linked = match &self.git_pr_status {
            Some(GitPrStatus::Linked { change_request }) => Some(change_request.number),
            _ => None,
        };
        if !self.pr_panel.open {
            let mut row = div().flex().items_center().gap(px(6.0));
            if let Some(number) = linked {
                row = row.child(
                    Self::pr_button("pr-details-open", "PR details & checks", theme).on_click(
                        cx.listener(move |this, _, _, cx| this.load_pr_panel(Some(number), cx)),
                    ),
                );
            }
            return row
                .child(
                    Self::pr_button("pr-draft-open", "New draft PR…", theme)
                        .on_click(cx.listener(|this, _, _, cx| this.load_pr_panel(None, cx))),
                )
                .into_any_element();
        }
        let busy = self.pr_panel.busy;
        for input in [
            &self.pr_panel.title,
            &self.pr_panel.body,
            &self.pr_panel.base,
            &self.pr_panel.head,
        ] {
            input.update(cx, |input, _| {
                input.read_only = busy || self.pr_panel.reconcile_only
            });
        }
        let mut panel = div()
            .id("github-pr-panel")
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(Theme::SPACE_SM))
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .bg(theme.surface);
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_size(px(12.0)).text_color(theme.text).child(
                    if self.pr_panel.details.is_some() {
                        "Pull request"
                    } else {
                        "New draft pull request"
                    },
                ))
                .child(
                    Self::pr_button("pr-panel-close", "Close", theme)
                        .on_click(cx.listener(|this, _, _, cx| this.close_pr_panel(cx))),
                ),
        );
        if let Some(details) = self.pr_panel.details.clone() {
            let url = details.summary.url.clone();
            panel = panel
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .child(format!(
                            "#{} · {}{}",
                            details.summary.number,
                            if details.is_draft { "Draft · " } else { "" },
                            details.summary.title
                        )),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_dim)
                        .child(format!(
                            "{} → {} · {:?}",
                            details.summary.head_ref,
                            details.summary.base_ref,
                            details.summary.state
                        )),
                )
                .child(
                    div()
                        .id("pr-description")
                        .max_h(px(120.0))
                        .overflow_y_scroll()
                        .text_size(px(11.0))
                        .text_color(theme.text)
                        .child(if details.body.is_empty() {
                            "No description".into()
                        } else {
                            details.body
                        }),
                )
                .child(div().text_size(px(11.0)).text_color(theme.text_dim).child(
                    if details.checks.is_empty() {
                        "No checks reported".into()
                    } else {
                        format!("Checks · {} reported", details.checks.len())
                    },
                ));
            for (index, check) in details.checks.into_iter().enumerate() {
                let tone = match check.status.as_str() {
                    "SUCCESS" | "NEUTRAL" | "SKIPPED" => theme.success,
                    "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" => {
                        theme.danger
                    }
                    "PENDING" | "QUEUED" | "IN_PROGRESS" | "WAITING" => theme.warning,
                    _ => theme.text_dim,
                };
                let status = check.status.replace('_', " ").to_ascii_lowercase();
                let mut row = div()
                    .id(SharedString::from(format!("pr-check-{index}")))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .text_size(px(11.0))
                    .text_color(theme.text)
                    .child(div().min_w_0().truncate().child(check.name))
                    .child(div().text_color(tone).child(status));
                if let Some(url) = check.url {
                    row = row
                        .cursor_pointer()
                        .on_click(move |_, _, cx| cx.open_url(&url));
                }
                panel = panel.child(row);
            }
            let number = details.summary.number;
            panel = panel.child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        Self::pr_button("pr-open-github", "Open on GitHub", theme)
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child(
                        Self::pr_button("pr-checks-refresh", "Refresh checks", theme).on_click(
                            cx.listener(move |this, _, _, cx| this.load_pr_panel(Some(number), cx)),
                        ),
                    ),
            );
        } else if let Some(preview) = self.pr_panel.preview.clone() {
            panel = panel
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_dim)
                        .child(preview.repository),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_dim)
                        .child(format!(
                            "{} · {}",
                            preview.checkout.cwd,
                            &preview.checkout.head[..preview.checkout.head.len().min(8)]
                        )),
                );
            for (label, input, height) in [
                ("Title", self.pr_panel.title.clone(), 32.0),
                ("Description", self.pr_panel.body.clone(), 96.0),
                ("Base branch", self.pr_panel.base.clone(), 30.0),
                (
                    "Head branch (current checkout)",
                    self.pr_panel.head.clone(),
                    30.0,
                ),
            ] {
                panel = panel
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_dim)
                            .child(label),
                    )
                    .child(
                        div()
                            .h(px(height))
                            .border_1()
                            .border_color(theme.border)
                            .rounded(px(4.0))
                            .child(input),
                    );
            }
            let label = if busy {
                "Working…"
            } else if self.pr_panel.reconcile_only {
                "Check GitHub"
            } else {
                "Create draft PR"
            };
            panel = panel
                .child(
                    div().text_size(px(11.0)).text_color(theme.text_dim).child(
                        "Creates a draft in this repository from the published head branch.",
                    ),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(6.0))
                        .child(
                            Self::pr_button("pr-create-draft", label, theme)
                                .when(busy, |b| b.opacity(0.5))
                                .on_click(cx.listener(|this, _, _, cx| this.submit_draft_pr(cx))),
                        )
                        .child(
                            Self::pr_button("pr-preview-refresh", "Refresh preview", theme)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.load_pr_panel(None, cx)),
                                ),
                        ),
                );
        } else if !busy {
            panel = panel.child(
                Self::pr_button("pr-preview-retry", "Retry", theme)
                    .on_click(cx.listener(move |this, _, _, cx| this.load_pr_panel(linked, cx))),
            );
        }
        if busy {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_dim)
                    .child("Checking GitHub…"),
            );
        }
        if let Some(notice) = self.pr_panel.notice.clone() {
            panel = panel.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_dim)
                    .child(notice),
            );
        }
        panel.into_any_element()
    }
}
