//! Evidence and results (A9 presentation over the engine's records): the
//! review section (rounds used of the repair cap, current reviewer, model
//! and phase, findings with severity and blocking labels, Review details to
//! the reviewer chat, cleanup state), outcomes (diff stats, pull request,
//! checks, artifacts, actual actor), dispatch history (first start,
//! continuations, brief updates with text preview and hash) and execution
//! history. Nothing here derives state: it renders `BoardItemDetail`.
//! Meaning never rides on colour alone: every state has its word.
use gpui::{
    AnyElement, App, IntoElement, ParentElement, SharedString, Styled, WeakEntity, div, prelude::*,
};
use luna_proto::{
    BOARD_MAX_REPAIR_ROUNDS, BoardActor, BoardColumn, BoardExecutionOptions, BoardInstructionKind,
    BoardItemDetail, LaunchSnapshot, OutcomeKind, OutcomeReference, ReviewCleanupState, ReviewRun,
    ReviewRunState, ReviewSeverity, ReviewVerdict, ReviewerPlacement, TaskLinkRole,
};

use super::detail::{BoardDetail, BoardProbe, format_time};
use super::settings_controls::describe_resolved;
use crate::button::{self, ButtonKind, ControlSize};
use crate::settings::widgets;
use crate::theme::Theme;
use crate::typography::ui_rems;

/// Characters of an instruction shown before "Show instruction".
const PREVIEW_CHARS: usize = 180;
/// Most characters of one instruction shown expanded.
const EXPANDED_CHARS: usize = 6_000;

/// What the evidence renderers need from the drawer.
pub(crate) struct EvidenceContext<'a> {
    pub theme: &'a Theme,
    pub probe: &'a BoardProbe,
    pub this: WeakEntity<BoardDetail>,
    pub options: Option<&'a BoardExecutionOptions>,
    /// A chat's title from the local chat list.
    pub chat_title: &'a dyn Fn(&str) -> Option<String>,
}

impl EvidenceContext<'_> {
    fn text(&self, key: impl Into<String>, copy: impl Into<String>) -> SharedString {
        let copy = copy.into();
        self.probe.text(key, copy.clone());
        copy.into()
    }

    fn actor(&self, actor: &BoardActor) -> String {
        actor_label(actor, self.chat_title)
    }
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

/// Who did it, in words (never a provider-native id).
pub(crate) fn actor_label(
    actor: &BoardActor,
    chat_title: &dyn Fn(&str) -> Option<String>,
) -> String {
    match actor {
        BoardActor::User { .. } => "You".into(),
        BoardActor::Agent { task_id, .. } => match chat_title(task_id) {
            Some(title) => format!("Agent in “{title}”"),
            None => "An agent chat".into(),
        },
        BoardActor::Reviewer { task_id, .. } => match chat_title(task_id) {
            Some(title) => format!("Reviewer “{title}”"),
            None => "The reviewer".into(),
        },
        BoardActor::Authoring { .. } => "Discuss with AI".into(),
        BoardActor::Engine { component } => format!("LUNA ({component})"),
    }
}

pub(crate) fn instruction_kind_label(kind: BoardInstructionKind) -> &'static str {
    match kind {
        BoardInstructionKind::FirstStart => "First start",
        BoardInstructionKind::Continuation => "Continuation",
        BoardInstructionKind::BriefUpdate => "Brief update",
        BoardInstructionKind::Recovery => "Recovery",
        BoardInstructionKind::Retry => "Retry",
        BoardInstructionKind::RepairRequest => "Repair request",
    }
}

pub(crate) fn review_state_label(state: ReviewRunState) -> &'static str {
    match state {
        ReviewRunState::Queued => "Queued",
        ReviewRunState::Starting => "Starting",
        ReviewRunState::Running => "Reviewing",
        ReviewRunState::Waiting => "Waiting",
        ReviewRunState::Completed => "Completed",
        ReviewRunState::Failed => "Failed",
        ReviewRunState::Cancelled => "Cancelled",
        ReviewRunState::OutcomeUnknown => "Outcome unknown",
        ReviewRunState::Stale => "Stale (the result changed)",
    }
}

pub(crate) fn verdict_label(verdict: ReviewVerdict) -> &'static str {
    match verdict {
        ReviewVerdict::Passed => "Passed",
        ReviewVerdict::ChangesRequested => "Changes requested",
        ReviewVerdict::Blocked => "Blocked",
        ReviewVerdict::Failed => "Failed",
        ReviewVerdict::Cancelled => "Cancelled",
    }
}

pub(crate) fn severity_label(severity: ReviewSeverity) -> &'static str {
    match severity {
        ReviewSeverity::Blocker => "Blocker",
        ReviewSeverity::Major => "Major",
        ReviewSeverity::Minor => "Minor",
        ReviewSeverity::Note => "Note",
    }
}

pub(crate) fn cleanup_label(run: &ReviewRun) -> String {
    match run.cleanup.state {
        ReviewCleanupState::NotStarted => {
            "Not started: the reviewer chat stays until the ticket passes.".into()
        }
        ReviewCleanupState::Pending => {
            "Pending: archiving the reviewer chat (retried with the same cleanup).".into()
        }
        ReviewCleanupState::Archived => "Archived; the review history is kept.".into(),
        ReviewCleanupState::Closed => "Archived and closed; the review history is kept.".into(),
        ReviewCleanupState::Failed => format!(
            "Failed{}",
            run.cleanup
                .detail
                .as_ref()
                .map(|detail| format!(": {detail}"))
                .unwrap_or_else(|| ".".into())
        ),
    }
}

fn outcome_kind_label(kind: OutcomeKind) -> &'static str {
    match kind {
        OutcomeKind::Diff => "Diff",
        OutcomeKind::PullRequest => "Pull request",
        OutcomeKind::Check => "Check",
        OutcomeKind::Artifact => "Artifact",
        OutcomeKind::Document => "Document",
        OutcomeKind::ReviewResult => "Review result",
        OutcomeKind::Summary => "Summary",
        OutcomeKind::IssueWriteback => "Issue update",
    }
}

/// An observed state word, capitalised (`passed` → `Passed`).
fn state_word(state: &str) -> String {
    let mut chars = state.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn number(detail: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| detail.get(*key).and_then(serde_json::Value::as_u64))
}

/// Diff statistics from an outcome's bounded detail.
pub(crate) fn diff_stats(outcome: &OutcomeReference) -> Option<String> {
    let detail = &outcome.detail;
    let files = number(detail, &["filesChanged", "files", "changedFiles"]);
    let additions = number(detail, &["additions", "insertions", "added"]);
    let deletions = number(detail, &["deletions", "removed"]);
    if files.is_none() && additions.is_none() && deletions.is_none() {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(files) = files {
        parts.push(format!("{files} file{}", if files == 1 { "" } else { "s" }));
    }
    if let Some(additions) = additions {
        parts.push(format!("+{additions}"));
    }
    if let Some(deletions) = deletions {
        parts.push(format!("−{deletions}"));
    }
    Some(parts.join(" · "))
}

fn short(value: &str) -> &str {
    value.get(..12).unwrap_or(value)
}

fn one_line(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// Counts of dispatched instructions by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DispatchCounts {
    pub starts: usize,
    pub continuations: usize,
    pub updates: usize,
    pub recoveries: usize,
    pub retries: usize,
    pub repairs: usize,
}

impl DispatchCounts {
    pub fn of(launches: &[LaunchSnapshot]) -> Self {
        let mut counts = Self::default();
        for launch in launches {
            match launch.kind {
                BoardInstructionKind::FirstStart => counts.starts += 1,
                BoardInstructionKind::Continuation => counts.continuations += 1,
                BoardInstructionKind::BriefUpdate => counts.updates += 1,
                BoardInstructionKind::Recovery => counts.recoveries += 1,
                BoardInstructionKind::Retry => counts.retries += 1,
                BoardInstructionKind::RepairRequest => counts.repairs += 1,
            }
        }
        counts
    }

    /// "1 start · 2 continuations · 1 brief update".
    pub fn summary(&self) -> String {
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut parts = vec![
            plural(self.starts, "start", "starts"),
            plural(self.continuations, "continuation", "continuations"),
        ];
        if self.updates > 0 {
            parts.push(plural(self.updates, "brief update", "brief updates"));
        }
        if self.repairs > 0 {
            parts.push(plural(self.repairs, "repair request", "repair requests"));
        }
        if self.retries > 0 {
            parts.push(plural(self.retries, "retry", "retries"));
        }
        if self.recoveries > 0 {
            parts.push(plural(self.recoveries, "recovery", "recoveries"));
        }
        parts.join(" · ")
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "starts": self.starts,
            "continuations": self.continuations,
            "updates": self.updates,
            "recoveries": self.recoveries,
            "retries": self.retries,
            "repairs": self.repairs,
        })
    }
}

// ---------------------------------------------------------------------------
// Section frame
// ---------------------------------------------------------------------------

pub(crate) fn section(
    theme: &Theme,
    id: &'static str,
    title: &'static str,
    description: Option<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    widgets::section_card(theme)
        .id(id)
        .mt(ui_rems(12.0))
        .child(widgets::section_header(theme, title, description, vec![]))
}

fn line(theme: &Theme, copy: SharedString) -> gpui::Div {
    widgets::card_row(theme, false).child(
        div()
            .w_full()
            .min_w_0()
            .child(widgets::row_description(theme, copy).text_color(theme.text)),
    )
}

fn labelled(theme: &Theme, label: SharedString, copy: SharedString) -> gpui::Div {
    widgets::card_row(theme, false).child(
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(ui_rems(2.0))
            .child(widgets::row_title(theme, label))
            .child(widgets::row_description(theme, copy)),
    )
}

fn open_url_button(id: String, label: &'static str, url: String, theme: &Theme) -> AnyElement {
    button::button(id, ButtonKind::Ghost, ControlSize::Sm, true, theme)
        .aria_label(label)
        .child(button::glyph(crate::icons::ARROW_UP_RIGHT, ControlSize::Sm))
        .child(label)
        .on_click(move |_, _, cx: &mut App| cx.open_url(&url))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Not started
// ---------------------------------------------------------------------------

/// Nothing was dispatched, reviewed or recorded yet: Review, Outcome and
/// Dispatch would each only say so.
pub(crate) fn not_started(detail: &BoardItemDetail) -> bool {
    detail.launches.is_empty()
        && detail.review_runs.is_empty()
        && detail.outcomes.is_empty()
        && detail.view.item.column != BoardColumn::Done
        && detail
            .intent
            .as_ref()
            .is_none_or(|intent| intent.review_passes == 0 && intent.repair_rounds == 0)
}

/// One line in place of the three empty lifecycle sections.
pub(crate) fn render_not_started(ecx: &EvidenceContext) -> AnyElement {
    let copy = ecx.text(
        "detail-lifecycle-none",
        "Not started: no instruction sent, no review and no results yet. The brief is sent once, \
         on the first start; dispatches, review rounds and results then appear here.",
    );
    section(
        ecx.theme,
        "detail-lifecycle",
        "Review, results and dispatch",
        None,
    )
    .child(line(ecx.theme, copy))
    .into_any_element()
}

// ---------------------------------------------------------------------------
// Review
// ---------------------------------------------------------------------------

fn current_run(detail: &BoardItemDetail) -> Option<&ReviewRun> {
    detail.review_runs.iter().max_by_key(|run| {
        (
            run.execution_generation,
            run.round,
            run.updated_at_ms,
            run.created_at_ms,
        )
    })
}

pub(crate) fn render_review(detail: &BoardItemDetail, ecx: &EvidenceContext) -> AnyElement {
    let theme = ecx.theme;
    let item = &detail.view.item;
    let (passes, repairs) = detail.intent.as_ref().map_or((0, 0), |intent| {
        (intent.review_passes, intent.repair_rounds)
    });
    let counters = ecx.text(
        "detail-review-repairs",
        format!("Repairs used: {repairs} of {BOARD_MAX_REPAIR_ROUNDS} · Review passes: {passes}"),
    );
    let mut card = section(
        theme,
        "detail-review",
        "Review",
        Some(
            "Each result is checked in a temporary reviewer side chat; at most two repair rounds \
             follow the first review, then the ticket needs attention."
                .into(),
        ),
    )
    .child(line(theme, counters));
    let Some(run) = current_run(detail) else {
        let copy = ecx.text(
            "detail-review-none",
            "Not reviewed yet. When the implementation submits a result, a reviewer side chat \
             checks it.",
        );
        return card.child(line(theme, copy)).into_any_element();
    };
    let name = ecx.text(
        "detail-review-name",
        format!("Review · {} · {}", item.key, item.title),
    );
    let resolved = &run.profile.resolved;
    let reviewer = ecx.text(
        "detail-review-reviewer",
        format!(
            "Reviewer: {}{}",
            describe_resolved(resolved, ecx.options, false),
            if run.profile.implementation_fallback {
                " (the implementation model; no reviewer is configured)"
            } else {
                ""
            }
        ),
    );
    let observed = ecx.text(
        "detail-review-observed",
        match (&run.profile.observed_model, run.profile.observed_harness) {
            (Some(model), Some(harness)) => format!(
                "Observed running: {} · {model}",
                super::settings_controls::harness_label(harness)
            ),
            (Some(model), None) => format!("Observed running: {model}"),
            (None, _) => "Observed model: not reported yet".into(),
        },
    );
    let phase = ecx.text(
        "detail-review-phase",
        format!(
            "Round {}{} · {}",
            run.round,
            if run.round == 0 {
                " (initial review)".to_owned()
            } else {
                format!(" (after repair {})", run.round)
            },
            review_state_label(run.state)
        ),
    );
    let placement = match run.placement {
        ReviewerPlacement::UnderTicket => "A side chat under the ticket's chat.".to_owned(),
        ReviewerPlacement::SiblingUnderMain => format!(
            "A side chat beside the ticket's chat, under “{}”.",
            (ecx.chat_title)(&run.display_main_parent_id).unwrap_or_else(|| "its main chat".into())
        ),
    };
    card = card
        .child(labelled(theme, name, reviewer))
        .child(line(theme, observed))
        .child(line(theme, phase))
        .child(line(theme, placement.into()));
    if let Some(result) = &run.result {
        let verdict = ecx.text(
            "detail-review-verdict",
            format!(
                "Result: {} · {}",
                verdict_label(result.verdict),
                result.summary
            ),
        );
        card = card.child(line(theme, verdict));
        for (index, finding) in result.findings.iter().enumerate() {
            let location = match (&finding.file, finding.line) {
                (Some(file), Some(line)) => format!(" · {file}:{line}"),
                (Some(file), None) => format!(" · {file}"),
                _ => String::new(),
            };
            let heading = ecx.text(
                format!("detail-finding-{index}"),
                format!(
                    "{} · {}{location}",
                    severity_label(finding.severity),
                    if finding.blocking {
                        "Blocking"
                    } else {
                        "Not blocking"
                    }
                ),
            );
            let mut body = format!(
                "{} Required: {}",
                finding.explanation, finding.required_action
            );
            if finding.repeats_finding_id.is_some() {
                body.push_str(" (Repeats an earlier finding.)");
            }
            card = card.child(
                widgets::card_row(theme, false).child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(ui_rems(8.0))
                        .child(
                            crate::icons::icon(if finding.blocking {
                                crate::icons::DANGER_TRIANGLE
                            } else {
                                crate::icons::INFO_CIRCLE
                            })
                            .size(ui_rems(14.0))
                            .flex_none()
                            .text_color(theme.readable_muted(0.9)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(ui_rems(2.0))
                                .child(widgets::row_title(theme, heading))
                                .child(widgets::row_description(theme, body)),
                        ),
                ),
            );
        }
    }
    let cleanup = ecx.text(
        "detail-review-cleanup",
        format!("Cleanup: {}", cleanup_label(run)),
    );
    card = card.child(line(theme, cleanup));
    if let Some(reviewer_task) = &run.reviewer_task_id {
        let archived = matches!(
            run.cleanup.state,
            ReviewCleanupState::Archived | ReviewCleanupState::Closed
        );
        let id = format!("detail-review-details:{}", run.id);
        let note = if archived {
            "Opens the archived reviewer chat read-only; it stays archived."
        } else {
            "Opens the reviewer side chat."
        };
        let this = ecx.this.clone();
        let task = reviewer_task.clone();
        card = card.child(
            widgets::card_row(theme, false)
                .child(widgets::row_text(
                    theme,
                    "Review details",
                    Some(note.into()),
                ))
                .child(
                    widgets::row_controls().child(
                        button::button(
                            id.clone(),
                            ButtonKind::Secondary,
                            ControlSize::Sm,
                            true,
                            theme,
                        )
                        .aria_label("Review details")
                        .child(button::glyph(crate::icons::ARROW_UP_RIGHT, ControlSize::Sm))
                        .child("Review details")
                        .children(ecx.probe.bounds_el(id))
                        .on_click(move |_, _, cx: &mut App| {
                            this.update(cx, |detail, cx| {
                                detail.open_review_details(task.clone(), cx)
                            })
                            .ok();
                        }),
                    ),
                ),
        );
    }
    // Earlier rounds, compactly.
    let mut earlier: Vec<&ReviewRun> = detail
        .review_runs
        .iter()
        .filter(|other| other.id != run.id)
        .collect();
    earlier.sort_by_key(|other| std::cmp::Reverse((other.execution_generation, other.round)));
    for (index, other) in earlier.iter().enumerate() {
        let findings = other
            .result
            .as_ref()
            .map_or(0, |result| result.findings.len());
        let blocking = other.result.as_ref().map_or(0, |result| {
            result
                .findings
                .iter()
                .filter(|finding| finding.blocking)
                .count()
        });
        let copy = ecx.text(
            format!("detail-review-earlier-{index}"),
            format!(
                "Round {} · {}{} · {findings} finding{} ({blocking} blocking)",
                other.round,
                review_state_label(other.state),
                other
                    .result
                    .as_ref()
                    .map(|result| format!(" · {}", verdict_label(result.verdict)))
                    .unwrap_or_default(),
                if findings == 1 { "" } else { "s" },
            ),
        );
        card = card.child(line(theme, copy));
    }
    card.into_any_element()
}

// ---------------------------------------------------------------------------
// Outcomes / evidence
// ---------------------------------------------------------------------------

fn outcome_row(index: usize, outcome: &OutcomeReference, ecx: &EvidenceContext) -> gpui::Div {
    let theme = ecx.theme;
    let state = outcome.state.as_deref().map(state_word);
    let headline = match outcome.kind {
        OutcomeKind::Diff => {
            let stats = diff_stats(outcome).unwrap_or_else(|| outcome.label.clone());
            format!("Diff · {stats}")
        }
        OutcomeKind::Check => format!(
            "Check · {} · {}",
            outcome.label,
            state.clone().unwrap_or_else(|| "State not reported".into())
        ),
        kind => match &state {
            Some(state) => format!("{} · {} · {state}", outcome_kind_label(kind), outcome.label),
            None => format!("{} · {}", outcome_kind_label(kind), outcome.label),
        },
    };
    let headline = ecx.text(format!("detail-outcome-{index}"), headline);
    let mut meta = vec![format!("Recorded by {}", ecx.actor(&outcome.observed_by))];
    meta.push(format_time(outcome.observed_at_ms));
    meta.push(format!("generation {}", outcome.execution_generation));
    if let Some(basis) = &outcome.basis {
        meta.push(format!("basis {}", short(basis)));
    }
    if let Some(path) = &outcome.path {
        meta.push(path.clone());
    }
    let meta = ecx.text(format!("detail-outcome-{index}-meta"), meta.join(" · "));
    let icon = match (outcome.kind, outcome.state.as_deref()) {
        (OutcomeKind::Check, Some("passed" | "success" | "succeeded")) => crate::icons::CHECK,
        (OutcomeKind::Check, Some("failed" | "failure" | "error")) => crate::icons::CLOSE_CIRCLE,
        (OutcomeKind::Diff, _) => crate::icons::GIT_DIFF,
        (OutcomeKind::PullRequest, _) => crate::icons::PULL_REQUEST,
        (OutcomeKind::Artifact | OutcomeKind::Document, _) => crate::icons::FILE_TEXT,
        _ => crate::icons::CHECKLIST,
    };
    let mut row = widgets::card_row(theme, false)
        .child(widgets::row_tile(theme, icon))
        .child(widgets::row_text(theme, headline, Some(meta)));
    if let Some(url) = &outcome.url {
        let label = match outcome.kind {
            OutcomeKind::PullRequest => "Open pull request",
            _ => "Open",
        };
        row = row.child(widgets::row_controls().child(open_url_button(
            format!("detail-outcome-{index}-open"),
            label,
            url.clone(),
            theme,
        )));
    }
    row
}

pub(crate) fn render_evidence(detail: &BoardItemDetail, ecx: &EvidenceContext) -> AnyElement {
    let theme = ecx.theme;
    let mut card = section(
        theme,
        "detail-evidence",
        "Outcome and evidence",
        Some("References to the result: diff, pull request, checks and artifacts.".into()),
    );
    if detail.view.item.column == BoardColumn::Done {
        let done = ecx.text(
            "detail-evidence-done",
            "Done: the configured checks and review passed for the current result.",
        );
        card = card.child(line(theme, done));
    }
    let current: Vec<&OutcomeReference> = detail
        .outcomes
        .iter()
        .filter(|outcome| !outcome.historical)
        .collect();
    let historical = detail.outcomes.len() - current.len();
    if current.is_empty() {
        let copy = ecx.text("detail-evidence-none", "No results recorded yet.");
        card = card.child(line(theme, copy));
    }
    for (index, outcome) in current.into_iter().enumerate() {
        card = card.child(outcome_row(index, outcome, ecx));
    }
    if historical > 0 {
        let copy = ecx.text(
            "detail-evidence-historical",
            format!(
                "{historical} earlier result{} kept as history.",
                if historical == 1 { "" } else { "s" }
            ),
        );
        card = card.child(line(theme, copy));
    }
    card.into_any_element()
}

// ---------------------------------------------------------------------------
// Dispatch history
// ---------------------------------------------------------------------------

pub(crate) fn render_dispatch(
    detail: &BoardItemDetail,
    expanded: Option<&str>,
    ecx: &EvidenceContext,
) -> AnyElement {
    let theme = ecx.theme;
    let counts = DispatchCounts::of(&detail.launches);
    let summary = ecx.text("detail-dispatch-summary", counts.summary());
    let mut card = section(theme, "detail-dispatch", "Dispatch history", Some(summary));
    if detail.launches.is_empty() {
        let copy = ecx.text(
            "detail-dispatch-none",
            "No instruction sent yet. The brief is sent once, on the first start.",
        );
        return card.child(line(theme, copy)).into_any_element();
    }
    for (index, launch) in detail.launches.iter().enumerate() {
        let open = expanded == Some(launch.id.as_str());
        let heading = ecx.text(
            format!("detail-dispatch-{index}"),
            format!(
                "{} · generation {} · brief revision {} · {}",
                instruction_kind_label(launch.kind),
                launch.execution_generation,
                launch.brief_revision,
                format_time(launch.created_at_ms)
            ),
        );
        let chat = (ecx.chat_title)(&launch.task_id)
            .map(|title| format!("Sent to “{title}”"))
            .unwrap_or_else(|| "Sent to the working chat".into());
        let preview = ecx.text(
            format!("detail-dispatch-{index}-preview"),
            one_line(&launch.instruction_text, PREVIEW_CHARS),
        );
        let meta = ecx.text(
            format!("detail-dispatch-{index}-meta"),
            format!(
                "{chat} · instruction hash {} · resolved {}",
                short(&launch.instruction_hash),
                describe_resolved(&launch.resolved, ecx.options, true)
            ),
        );
        let id = format!("detail-dispatch-toggle:{}", launch.id);
        let this = ecx.this.clone();
        let launch_id = launch.id.clone();
        let mut row = widgets::card_row(theme, false)
            .child(
                div()
                    .flex_1()
                    .min_w(ui_rems(widgets::ROW_TEXT_MIN))
                    .flex()
                    .flex_col()
                    .gap(ui_rems(2.0))
                    .child(widgets::row_title(theme, heading))
                    .child(widgets::row_description(theme, preview).text_color(theme.text))
                    .child(widgets::row_description(theme, meta)),
            )
            .child(
                widgets::row_controls().child(
                    button::button(id.clone(), ButtonKind::Ghost, ControlSize::Sm, true, theme)
                        .aria_label(if open {
                            "Hide instruction"
                        } else {
                            "Show instruction"
                        })
                        .aria_expanded(open)
                        .child(if open {
                            "Hide instruction"
                        } else {
                            "Show instruction"
                        })
                        .children(ecx.probe.bounds_el(id))
                        .on_click(move |_, _, cx: &mut App| {
                            this.update(cx, |detail, cx| {
                                detail.toggle_launch(launch_id.clone(), cx)
                            })
                            .ok();
                        }),
                ),
            );
        if open {
            let full: String = launch
                .instruction_text
                .chars()
                .take(EXPANDED_CHARS)
                .collect();
            row = row.child(
                div()
                    .w_full()
                    .px(ui_rems(10.0))
                    .py(ui_rems(8.0))
                    .rounded(gpui::px(8.0))
                    .bg(theme.fill(crate::theme::FillTier::Quaternary))
                    .text_size(ui_rems(12.0))
                    .line_height(ui_rems(17.0))
                    .text_color(theme.text)
                    .child(full),
            );
        }
        card = card.child(row);
    }
    card.into_any_element()
}

// ---------------------------------------------------------------------------
// Execution history
// ---------------------------------------------------------------------------

fn event_label(name: &str) -> String {
    let mut words = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            words.push(' ');
            words.extend(ch.to_lowercase());
        } else if index == 0 {
            words.extend(ch.to_uppercase());
        } else {
            words.push(ch);
        }
    }
    words
}

pub(crate) fn render_history(detail: &BoardItemDetail, ecx: &EvidenceContext) -> AnyElement {
    let theme = ecx.theme;
    let mut card = section(
        theme,
        "detail-history",
        "Execution history",
        Some("Attempts and recent activity, newest first.".into()),
    );
    let attempts: Vec<_> = detail
        .links
        .iter()
        .filter(|link| {
            link.role == TaskLinkRole::Attempt
                || (link.role == TaskLinkRole::Working && !link.active)
        })
        .collect();
    for (index, link) in attempts.iter().enumerate() {
        let title = (ecx.chat_title)(&link.task_id).unwrap_or_else(|| "A chat".into());
        let copy = ecx.text(
            format!("detail-attempt-{index}"),
            format!(
                "Attempt · “{title}” · linked {}{}",
                format_time(link.linked_at_ms),
                link.unlinked_at_ms
                    .map(|at| format!(" · ended {}", format_time(at)))
                    .unwrap_or_default()
            ),
        );
        card = card.child(line(theme, copy));
    }
    let events: Vec<_> = detail
        .recent_events
        .iter()
        .filter(|event| event.activity)
        .collect();
    if events.is_empty() && attempts.is_empty() {
        card = card.child(line(theme, "No activity yet.".into()));
    }
    for (index, event) in events.iter().enumerate() {
        let copy = ecx.text(
            format!("detail-event-{index}"),
            format!(
                "{} · {} · revision {} · {}",
                event_label(event.kind.name()),
                ecx.actor(&event.actor),
                event.revision,
                format_time(event.at_ms)
            ),
        );
        card = card.child(line(theme, copy));
    }
    card.into_any_element()
}
