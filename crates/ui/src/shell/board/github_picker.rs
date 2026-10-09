//! From GitHub issue (A7): repository/account selection, search with
//! freshness, preview with selected comments and truncation notes, Add to
//! board / Start session, Open card for existing links; the explicit PB-3
//! writeback confirmation (exact target and payload, readback); and the
//! view-model helpers the detail drawer (A9) renders for a ticket's source
//! issue, dependencies and writeback actions.
//!
//! Everything goes through `BoardApi::issue_*`: the engine on the project's
//! owning host resolves remotes and accounts, filters pull requests, scopes
//! caches per account, dedupes imports and verifies writes. This file only
//! presents what it returns. Superseded searches are dropped (their `gh`
//! subprocess is killed with the request) and late answers are ignored by
//! generation.
use std::collections::BTreeSet;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, SharedString, Stateful, Styled, Subscription, Task, Window, div,
    prelude::*, px,
};
use luna_proto::{
    BoardColumn, BoardImportStart, BoardIssueCommentsRequest, BoardIssueDetailRequest,
    BoardIssueImportOutcome, BoardIssueImportReceipt, BoardIssueImportRequest,
    BoardIssueSearchRequest, BoardIssueSources, BoardIssueSourcesRequest,
    BoardIssueWritebackPreviewRequest, BoardIssueWritebackRequest, BoardItemDetail,
    BoardStartOutcome, IssueComment, IssueDetail, IssueLinkRole, IssueSearchPage, IssueSource,
    IssueSourceError, IssueStateFilter, IssueSummary, IssueWritebackAction, IssueWritebackPreview,
    IssueWritebackReceipt, OutcomeKind,
};

use super::api::{BoardApi, BoardCallError};
use super::model::BoardModel;
use crate::AccessibleButton as _;
use crate::button::{ButtonKind, ControlSize};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons::{self, icon};
use crate::perf_counters;
use crate::theme::Theme;
use crate::typography::ui_rems;

/// Typing pauses this long before a search runs (Enter searches at once).
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(350);
const PAGE_LIMIT: u32 = 25;
const PICKER_WIDTH: f32 = 880.0;
const PICKER_HEIGHT: f32 = 700.0;
/// Layout-probe prefix for native QA (`perf_counters::probes`).
pub const PROBE_PREFIX: &str = "board-gh:";

fn probe(name: impl Into<String>) -> Option<AnyElement> {
    perf_counters::probes_enabled()
        .then(|| perf_counters::probe(format!("{PROBE_PREFIX}{}", name.into())))
}

fn ago(at_ms: i64) -> String {
    let seconds = ((chrono::Utc::now().timestamp_millis() - at_ms) / 1000).max(0);
    match seconds {
        0..=4 => "just now".into(),
        5..=59 => format!("{seconds}s ago"),
        60..=3599 => format!("{}m ago", seconds / 60),
        _ => format!("{}h ago", seconds / 3600),
    }
}

/// A short date from an ISO-8601 timestamp (`2026-10-08`).
fn day(iso: &str) -> &str {
    iso.get(..10).unwrap_or(iso)
}

/// What a source error means for the person, scoped to the account/host.
pub fn source_error_text(error: &IssueSourceError) -> String {
    match error {
        IssueSourceError::AuthRequired { host } => format!(
            "GitHub CLI is not signed in for {host} on this project's host. Run gh auth login there, then refresh."
        ),
        IssueSourceError::AccountChanged { host } => {
            format!("The GitHub account for {host} changed. Refresh to load its issues.")
        }
        IssueSourceError::NotFound => {
            "This repository is not available to the current GitHub account.".into()
        }
        IssueSourceError::RateLimited { retry_after_ms } => match retry_after_ms {
            Some(ms) => format!(
                "GitHub's rate limit was reached. Retry in about {}s.",
                ms.div_ceil(1000)
            ),
            None => "GitHub's rate limit was reached. Retry shortly.".into(),
        },
        IssueSourceError::Timeout => "GitHub did not answer in time. Refresh to retry.".into(),
        IssueSourceError::Unavailable { reason } => reason.clone(),
    }
}

/// A source as the selector names it.
pub fn source_label(source: &IssueSource) -> String {
    let mut label = format!("{}/{}", source.repository.owner, source.repository.name);
    if source.fork {
        match &source.parent {
            Some(parent) => label.push_str(&format!(" · fork of {parent}")),
            None => label.push_str(" · fork"),
        }
    }
    let remotes = if source.remote_names.is_empty() {
        source.remote_name.clone()
    } else {
        source.remote_names.join(", ")
    };
    label.push_str(&format!(" ({remotes})"));
    label
}

// ---------------------------------------------------------------------------
// View-model helpers for the detail drawer (A9)
// ---------------------------------------------------------------------------
// The drawer renders these; until it lands nothing in this crate calls them.

/// A ticket's linked issue as the detail drawer shows it.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // rendered by the detail drawer (A9)
pub struct SourceIssuePanel {
    pub issue_ref_id: String,
    /// `owner/repo#123`.
    pub display: String,
    pub canonical_url: String,
    pub source: bool,
    pub remote_state: Option<String>,
    pub remote_title: Option<String>,
    /// Edited or closed on GitHub after import (the brief is unchanged).
    pub source_changed: bool,
    pub account: Option<String>,
    pub fetched_at_ms: i64,
    /// What the imported brief included or omitted.
    pub snapshot_note: Option<String>,
    /// For a refresh (`BoardIssueDetail { refresh: true }`) and writeback.
    pub source_id: Option<String>,
    pub number: u64,
}

#[allow(dead_code)] // rendered by the detail drawer (A9)
impl SourceIssuePanel {
    /// One status line: remote state, change marker and freshness.
    pub fn status_line(&self) -> String {
        let mut parts = vec![format!(
            "GitHub: {}",
            self.remote_state.as_deref().unwrap_or("unknown")
        )];
        if self.source_changed {
            parts.push("changed on GitHub since import — the brief was not changed".into());
        }
        parts.push(format!("checked {}", ago(self.fetched_at_ms)));
        if let Some(account) = &self.account {
            parts.push(format!("@{account}"));
        }
        parts.join(" · ")
    }
}

/// Active linked issues, source first.
#[allow(dead_code)] // rendered by the detail drawer (A9)
pub fn source_issue_panels(detail: &BoardItemDetail) -> Vec<SourceIssuePanel> {
    let mut panels: Vec<SourceIssuePanel> = detail
        .issues
        .iter()
        .filter(|reference| reference.active)
        .map(|reference| SourceIssuePanel {
            issue_ref_id: reference.id.clone(),
            display: format!(
                "{}/{}#{}",
                reference.repository.owner, reference.repository.name, reference.number
            ),
            canonical_url: reference.canonical_url.clone(),
            source: reference.role == IssueLinkRole::Source,
            remote_state: reference.remote_state.clone(),
            remote_title: reference.remote_title.clone(),
            source_changed: reference.source_changed_at_ms.is_some(),
            account: reference.account.clone(),
            fetched_at_ms: reference.fetched_at_ms,
            snapshot_note: reference
                .import_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.omitted_summary.clone()),
            source_id: reference.source_id.clone(),
            number: reference.number,
        })
        .collect();
    panels.sort_by_key(|panel| !panel.source);
    panels
}

/// One end of a blocked-by link.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // rendered by the detail drawer (A9)
pub struct DependencyRow {
    pub work_item_id: String,
    /// Key/title/column when the ticket is loaded in the Board model.
    pub key: Option<String>,
    pub title: Option<String>,
    pub column: Option<BoardColumn>,
    /// Still blocking (not Done, not archived).
    pub open: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[allow(dead_code)] // rendered by the detail drawer (A9)
pub struct DependencyPanel {
    pub blocked_by: Vec<DependencyRow>,
    /// Tickets this one blocks (the other end).
    pub blocks: Vec<DependencyRow>,
    /// Open blockers exist. Visible only: clearing a blocker never starts
    /// work; Start/Ready remain explicit actions.
    pub blocked: bool,
}

#[allow(dead_code)] // rendered by the detail drawer (A9)
pub fn dependency_panel(detail: &BoardItemDetail, model: &BoardModel) -> DependencyPanel {
    let row = |id: &str, open: bool| {
        let view = model.item(id);
        DependencyRow {
            work_item_id: id.to_owned(),
            key: view.map(|view| view.item.key.clone()),
            title: view.map(|view| view.item.title.clone()),
            column: view.map(|view| view.item.column),
            open,
        }
    };
    let blocked_by: Vec<DependencyRow> = detail
        .dependencies
        .iter()
        .map(|dependency| {
            row(
                &dependency.blocked_by_id,
                detail.view.blocked_by.contains(&dependency.blocked_by_id),
            )
        })
        .collect();
    let finished =
        matches!(detail.view.item.column, BoardColumn::Done) || detail.view.item.archived;
    let blocks = detail
        .dependents
        .iter()
        .map(|dependency| row(&dependency.work_item_id, !finished))
        .collect();
    DependencyPanel {
        blocked: !detail.view.blocked_by.is_empty(),
        blocked_by,
        blocks,
    }
}

/// The explicit GitHub writes offered for a linked issue (PB-3). Local Done
/// never performs any of them by itself.
#[derive(Debug, Clone, PartialEq)]
pub enum WritebackChoice {
    /// Post the result / PR link (draft prefilled from the evidence).
    Comment {
        draft: String,
    },
    AddLabel,
    RemoveLabel {
        label: String,
    },
    /// Close with an accepted outcome: offered only when the ticket is Done.
    Close,
}

#[allow(dead_code)] // rendered by the detail drawer (A9)
pub fn writeback_choices(detail: &BoardItemDetail, issue_ref_id: &str) -> Vec<WritebackChoice> {
    let Some(reference) = detail
        .issues
        .iter()
        .find(|reference| reference.id == issue_ref_id && reference.active)
    else {
        return Vec::new();
    };
    let mut choices = vec![
        WritebackChoice::Comment {
            draft: result_comment_draft(detail),
        },
        WritebackChoice::AddLabel,
    ];
    if detail.view.item.column == BoardColumn::Done
        && reference.remote_state.as_deref() != Some("closed")
    {
        choices.push(WritebackChoice::Close);
    }
    choices
}

/// A result comment drafted from recorded evidence (PR links, summaries).
/// The person edits it; nothing is posted without the confirmation dialog.
#[allow(dead_code)] // rendered by the detail drawer (A9)
pub fn result_comment_draft(detail: &BoardItemDetail) -> String {
    let item = &detail.view.item;
    let mut lines = vec![format!(
        "Update from LUNA ticket {}: {}",
        item.key, item.title
    )];
    for outcome in detail.outcomes.iter().filter(|outcome| !outcome.historical) {
        match outcome.kind {
            OutcomeKind::PullRequest => lines.push(format!(
                "- Pull request: {}{}",
                outcome.url.as_deref().unwrap_or(&outcome.label),
                outcome
                    .state
                    .as_deref()
                    .map(|state| format!(" ({state})"))
                    .unwrap_or_default()
            )),
            OutcomeKind::Summary | OutcomeKind::Document | OutcomeKind::ReviewResult => {
                lines.push(format!("- {}", outcome.label))
            }
            OutcomeKind::Check => lines.push(format!(
                "- Check {}: {}",
                outcome.label,
                outcome.state.as_deref().unwrap_or("observed")
            )),
            _ => {}
        }
    }
    if item.column == BoardColumn::Done {
        lines.push("The ticket's outcome was accepted.".into());
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// The From GitHub issue picker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum GitHubPickerEvent {
    Dismissed,
    /// Open this ticket (an existing card, or the one just added).
    OpenCard {
        work_item_id: String,
    },
    /// A committed import: show the card immediately.
    Imported(Box<BoardIssueImportReceipt>),
}

enum Sources {
    Loading,
    Ready(BoardIssueSources),
    Failed(String),
}

pub struct GitHubIssuePicker {
    pub(crate) model: Entity<BoardModel>,
    pub(crate) project_id: String,
    focus: FocusHandle,
    focus_pending: bool,
    sources: Sources,
    sources_task: Option<Task<()>>,
    source_id: Option<String>,
    search: Entity<ComposerInput>,
    prompt: Entity<ComposerInput>,
    state_filter: IssueStateFilter,
    results: Vec<IssueSummary>,
    page: Option<IssueSearchPage>,
    pull_requests_excluded: u32,
    searching: bool,
    search_error: Option<String>,
    search_generation: u64,
    search_task: Option<Task<()>>,
    selected: Option<u64>,
    detail: Option<IssueDetail>,
    detail_error: Option<String>,
    detail_task: Option<Task<()>>,
    comments: Vec<IssueComment>,
    comments_next: Option<String>,
    comments_requested: bool,
    comments_error: Option<String>,
    comments_task: Option<Task<()>>,
    selected_comments: BTreeSet<String>,
    import_task: Option<Task<()>>,
    /// Kept across a lost answer so the retry reconciles instead of
    /// importing again.
    import_request_id: Option<String>,
    import_error: Option<String>,
    last_import: Option<BoardIssueImportReceipt>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<GitHubPickerEvent> for GitHubIssuePicker {}

impl Focusable for GitHubIssuePicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl GitHubIssuePicker {
    pub fn new(model: Entity<BoardModel>, project_id: String, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            ComposerInput::new("Search issues", cx)
                .with_single_line()
                .with_accessibility_role(gpui::Role::SearchInput)
        });
        let prompt = cx.new(|cx| {
            ComposerInput::new(
                "Your initial prompt: what should the agent do with this issue?",
                cx,
            )
            .with_accessibility_role(gpui::Role::MultilineTextInput)
        });
        let search_events = cx.subscribe(&search, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Submitted => this.run_search(None, false, cx),
            ComposerInputEvent::Edited => this.debounce_search(cx),
            _ => {}
        });
        let mut picker = Self {
            model,
            project_id,
            focus: cx.focus_handle(),
            focus_pending: true,
            sources: Sources::Loading,
            sources_task: None,
            source_id: None,
            search,
            prompt,
            state_filter: IssueStateFilter::Open,
            results: Vec::new(),
            page: None,
            pull_requests_excluded: 0,
            searching: false,
            search_error: None,
            search_generation: 0,
            search_task: None,
            selected: None,
            detail: None,
            detail_error: None,
            detail_task: None,
            comments: Vec::new(),
            comments_next: None,
            comments_requested: false,
            comments_error: None,
            comments_task: None,
            selected_comments: BTreeSet::new(),
            import_task: None,
            import_request_id: None,
            import_error: None,
            last_import: None,
            _subscriptions: vec![search_events],
        };
        picker.load_sources(cx);
        picker
    }

    fn api(&self, cx: &App) -> Option<BoardApi> {
        self.model.read(cx).api(cx)
    }

    fn load_sources(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.api(cx) else {
            self.sources = Sources::Failed("Engine not connected.".into());
            return;
        };
        let request = BoardIssueSourcesRequest {
            project_id: self.project_id.clone(),
        };
        self.sources = Sources::Loading;
        self.sources_task = Some(cx.spawn(async move |this, cx| {
            let result = api.issue_sources(request).await;
            this.update(cx, |picker, cx| {
                match result {
                    Ok(sources) => {
                        // A single verified repository is preselected; a fork
                        // and its upstream need an explicit choice.
                        picker.source_id = sources
                            .sources
                            .iter()
                            .find(|source| source.default)
                            .map(|source| source.id.clone());
                        picker.sources = Sources::Ready(sources);
                        if picker.source_id.is_some() {
                            picker.run_search(None, false, cx);
                        }
                    }
                    Err(error) => picker.sources = Sources::Failed(error.message()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn select_source(&mut self, source_id: String, cx: &mut Context<Self>) {
        if self.source_id.as_deref() == Some(source_id.as_str()) {
            return;
        }
        self.source_id = Some(source_id);
        self.results.clear();
        self.clear_selection();
        self.run_search(None, false, cx);
    }

    fn clear_selection(&mut self) {
        self.selected = None;
        self.detail = None;
        self.detail_error = None;
        self.detail_task = None;
        self.comments.clear();
        self.comments_next = None;
        self.comments_requested = false;
        self.comments_error = None;
        self.comments_task = None;
        self.selected_comments.clear();
        self.import_request_id = None;
        self.import_error = None;
    }

    fn debounce_search(&mut self, cx: &mut Context<Self>) {
        if self.source_id.is_none() {
            return;
        }
        self.search_generation += 1;
        let generation = self.search_generation;
        self.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            this.update(cx, |picker, cx| {
                if picker.search_generation == generation {
                    picker.run_search(None, false, cx);
                }
            })
            .ok();
        }));
    }

    /// Search (first page) or load more (`cursor`). Replacing the task drops
    /// the superseded request.
    fn run_search(&mut self, cursor: Option<String>, refresh: bool, cx: &mut Context<Self>) {
        let (Some(api), Some(source_id)) = (self.api(cx), self.source_id.clone()) else {
            return;
        };
        let query = self.search.read(cx).text().trim().to_owned();
        let request = BoardIssueSearchRequest {
            project_id: self.project_id.clone(),
            source_id,
            query: (!query.is_empty()).then_some(query),
            state: self.state_filter,
            labels: Vec::new(),
            assignee: None,
            cursor: cursor.clone(),
            limit: Some(PAGE_LIMIT),
            refresh,
        };
        self.search_generation += 1;
        let generation = self.search_generation;
        self.searching = true;
        if cursor.is_none() {
            self.search_error = None;
        }
        cx.notify();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result = api.issue_search(request).await;
            this.update(cx, |picker, cx| {
                if picker.search_generation != generation {
                    return; // superseded
                }
                picker.searching = false;
                match result {
                    Ok(page) => {
                        if cursor.is_none() {
                            picker.results.clear();
                            picker.pull_requests_excluded = 0;
                        }
                        for item in &page.items {
                            if !picker
                                .results
                                .iter()
                                .any(|existing| existing.number == item.number)
                            {
                                picker.results.push(item.clone());
                            }
                        }
                        picker.pull_requests_excluded += page.pull_requests_excluded;
                        picker.search_error = page.error.as_ref().map(source_error_text);
                        picker.page = Some(page);
                    }
                    Err(error) => picker.search_error = Some(error.message()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn set_state_filter(&mut self, filter: IssueStateFilter, cx: &mut Context<Self>) {
        if self.state_filter != filter {
            self.state_filter = filter;
            self.run_search(None, false, cx);
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.source_id.is_none() {
            self.load_sources(cx);
            return;
        }
        self.run_search(None, true, cx);
        if let Some(number) = self.selected {
            self.load_detail(number, true, cx);
        }
    }

    pub fn select_issue(&mut self, number: u64, cx: &mut Context<Self>) {
        if self.selected == Some(number) {
            return;
        }
        self.clear_selection();
        self.selected = Some(number);
        self.load_detail(number, false, cx);
    }

    fn load_detail(&mut self, number: u64, refresh: bool, cx: &mut Context<Self>) {
        let (Some(api), Some(source_id)) = (self.api(cx), self.source_id.clone()) else {
            return;
        };
        let request = BoardIssueDetailRequest {
            project_id: self.project_id.clone(),
            source_id,
            number,
            refresh,
        };
        cx.notify();
        self.detail_task = Some(cx.spawn(async move |this, cx| {
            let result = api.issue_detail(request).await;
            this.update(cx, |picker, cx| {
                if picker.selected != Some(number) {
                    return;
                }
                match result {
                    Ok(detail) => {
                        if let Some(row) = picker
                            .results
                            .iter_mut()
                            .find(|row| row.number == detail.summary.number)
                        {
                            *row = detail.summary.clone();
                        }
                        picker.detail = Some(detail);
                        picker.detail_error = None;
                    }
                    Err(error) => picker.detail_error = Some(error.message()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Comments load on demand; the person selects which to include.
    pub fn load_comments(&mut self, cx: &mut Context<Self>) {
        let (Some(api), Some(source_id), Some(number)) =
            (self.api(cx), self.source_id.clone(), self.selected)
        else {
            return;
        };
        let request = BoardIssueCommentsRequest {
            project_id: self.project_id.clone(),
            source_id,
            number,
            cursor: self.comments_next.clone(),
            limit: Some(30),
        };
        self.comments_requested = true;
        cx.notify();
        self.comments_task = Some(cx.spawn(async move |this, cx| {
            let result = api.issue_comments(request).await;
            this.update(cx, |picker, cx| {
                if picker.selected != Some(number) {
                    return;
                }
                match result {
                    Ok(page) => {
                        picker.comments.extend(page.comments);
                        picker.comments_next = page.next_cursor;
                        picker.comments_error = None;
                    }
                    Err(error) => picker.comments_error = Some(error.message()),
                }
                picker.comments_task = None;
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn toggle_comment(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.selected_comments.remove(&id) && self.selected_comments.len() < 10 {
            self.selected_comments.insert(id);
        }
        cx.notify();
    }

    /// The ticket already holding the selected issue, if any.
    fn linked_work_item(&self) -> Option<String> {
        self.detail
            .as_ref()
            .and_then(|detail| detail.summary.linked_work_item_id.clone())
    }

    fn key_for(&self, work_item_id: &str, cx: &App) -> Option<String> {
        self.model
            .read(cx)
            .item(work_item_id)
            .map(|view| view.item.key.clone())
            .or_else(|| {
                self.last_import
                    .as_ref()
                    .filter(|receipt| receipt.mutation.work_item_id == work_item_id)
                    .map(|receipt| receipt.mutation.key.clone())
            })
    }

    /// Add to board, or Add and start (through the lifecycle start path).
    pub fn import(&mut self, start: bool, cx: &mut Context<Self>) {
        if self.import_task.is_some() || self.linked_work_item().is_some() {
            return;
        }
        let (Some(api), Some(source_id), Some(detail)) =
            (self.api(cx), self.source_id.clone(), self.detail.clone())
        else {
            return;
        };
        let request_id = self
            .import_request_id
            .get_or_insert_with(|| format!("gh-import-{}", uuid::Uuid::new_v4().simple()))
            .clone();
        let additions = self.prompt.read(cx).text().trim().to_owned();
        let request = BoardIssueImportRequest {
            request_id,
            project_id: self.project_id.clone(),
            source_id,
            number: detail.summary.number,
            additions: (!additions.is_empty()).then_some(additions),
            comment_ids: self.selected_comments.iter().cloned().collect(),
            start: start.then(|| BoardImportStart {
                destination: Default::default(),
                execution: None,
            }),
            previewed_updated_at: Some(detail.summary.updated_at.clone()),
        };
        self.import_error = None;
        cx.notify();
        self.import_task = Some(cx.spawn(async move |this, cx| {
            let mut result = api.issue_import(request.clone()).await;
            if matches!(result, Err(BoardCallError::Lost)) {
                // Reconcile with the same request id: never a second card.
                result = api.issue_import(request).await;
            }
            this.update(cx, |picker, cx| {
                picker.import_task = None;
                match result {
                    Ok(receipt) => picker.on_imported(receipt, cx),
                    Err(BoardCallError::Board(luna_proto::BoardError::InvalidInput {
                        field, ..
                    })) if field == "previewedUpdatedAt" => {
                        picker.import_error = Some(
                            "The issue changed on GitHub after this preview. Review the updated issue, then add it."
                                .into(),
                        );
                        picker.import_request_id = None;
                        if let Some(number) = picker.selected {
                            picker.load_detail(number, true, cx);
                        }
                    }
                    Err(error) => picker.import_error = Some(error.message()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn on_imported(&mut self, receipt: BoardIssueImportReceipt, cx: &mut Context<Self>) {
        let work_item_id = receipt.mutation.work_item_id.clone();
        for row in self
            .results
            .iter_mut()
            .filter(|row| row.number == receipt.issue.number)
        {
            row.linked_work_item_id = Some(work_item_id.clone());
        }
        if let Some(detail) = &mut self.detail
            && detail.summary.number == receipt.issue.number
        {
            detail.summary.linked_work_item_id = Some(work_item_id.clone());
        }
        if receipt.outcome == BoardIssueImportOutcome::Created {
            cx.emit(GitHubPickerEvent::Imported(Box::new(receipt.clone())));
        }
        self.last_import = Some(receipt);
        self.import_request_id = None;
        // Re-read so the on-board marker comes from the engine.
        self.run_search(None, false, cx);
    }

    fn import_status(&self) -> Option<String> {
        let receipt = self.last_import.as_ref()?;
        if Some(receipt.issue.number) != self.selected {
            return None;
        }
        let key = &receipt.mutation.key;
        let mut text = match receipt.outcome {
            BoardIssueImportOutcome::Created => format!("Added to Backlog as {key}."),
            BoardIssueImportOutcome::Existing => format!("Already on board as {key}."),
        };
        if let Some(start) = &receipt.start {
            text.push(' ');
            text.push_str(&match &start.outcome {
                BoardStartOutcome::Started => "Session started.".to_owned(),
                BoardStartOutcome::Reused => "Its existing session continues.".to_owned(),
                BoardStartOutcome::Queued { .. } => "Start queued.".to_owned(),
                BoardStartOutcome::Reconciling => "Start outcome unknown; reconciling.".to_owned(),
                BoardStartOutcome::Failed { error } => {
                    format!("Not started: {}", error.message())
                }
            });
        }
        Some(text)
    }

    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_readback(&self, cx: &App) -> serde_json::Value {
        let sources = match &self.sources {
            Sources::Loading => serde_json::json!({"state": "loading"}),
            Sources::Failed(error) => serde_json::json!({"state": "failed", "error": error}),
            Sources::Ready(sources) => serde_json::json!({
                "state": "ready",
                "ids": sources.sources.iter().map(|source| source.id.clone()).collect::<Vec<_>>(),
                "defaults": sources.sources.iter().filter(|source| source.default).count(),
                "error": sources.error,
            }),
        };
        let linked = self.linked_work_item();
        serde_json::json!({
            "sources": sources,
            "sourceId": self.source_id,
            "query": self.search.read(cx).text(),
            "searching": self.searching,
            "searchError": self.search_error,
            "rows": self.results.iter().map(|row| serde_json::json!({
                "number": row.number,
                "title": row.title,
                "labels": row.labels,
                "assignees": row.assignees,
                "state": row.state,
                "updatedAt": row.updated_at,
                "onBoard": row.linked_work_item_id,
            })).collect::<Vec<_>>(),
            "pullRequestsExcluded": self.pull_requests_excluded,
            "page": self.page.as_ref().map(|page| serde_json::json!({
                "partial": page.partial,
                "nextCursor": page.next_cursor,
                "freshness": page.freshness,
            })),
            "selected": self.selected,
            "detail": self.detail.as_ref().map(|detail| serde_json::json!({
                "number": detail.summary.number,
                "url": detail.summary.canonical_url,
                "bodyBytes": detail.body.len(),
                "bodyTruncated": detail.body_truncated,
                "omitted": detail.omitted_summary,
                "onBoard": detail.summary.linked_work_item_id,
            })),
            "comments": self.comments.iter().map(|comment| comment.id.clone()).collect::<Vec<_>>(),
            "selectedComments": self.selected_comments.iter().cloned().collect::<Vec<_>>(),
            "prompt": self.prompt.read(cx).text(),
            "addEnabled": self.detail.is_some() && linked.is_none() && self.import_task.is_none(),
            "openCard": linked,
            "importStatus": self.import_status(),
            "importError": self.import_error,
            "lastImport": self.last_import.as_ref().map(|receipt| serde_json::json!({
                "outcome": receipt.outcome,
                "workItemId": receipt.mutation.work_item_id,
                "key": receipt.mutation.key,
            })),
        })
    }
}

fn action_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: &str,
    kind: ButtonKind,
    enabled: bool,
) -> Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let button = crate::button::button_keyed(
        ElementId::Name(id.clone()),
        SharedString::from(format!("board-gh-{id}")),
        kind,
        ControlSize::Md,
        enabled,
        theme,
    )
    .relative()
    .child(SharedString::from(label.to_owned()))
    .children(probe(id.to_string()));
    if enabled {
        button.accessible_button(label.to_owned(), theme.accent)
    } else {
        button.aria_label(label.to_owned())
    }
}

/// A click anywhere in a field's frame (padding, the empty lines below the
/// text) puts the caret in its input. Preventing the default stops the
/// picker's own focus tracking (an ancestor) from taking focus back in the
/// same mouse-down, which left typing with nowhere to go.
fn focus_field(input: &Entity<ComposerInput>, window: &mut Window, cx: &mut App) {
    window.focus(&input.focus_handle(cx), cx);
    window.prevent_default();
}

fn meta_text(theme: &Theme, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(ui_rems(12.0))
        .text_color(theme.text_muted)
        .child(text.into())
}

fn notice(theme: &Theme, text: impl Into<SharedString>, warning: bool) -> gpui::Div {
    div()
        .px(ui_rems(10.0))
        .py(ui_rems(6.0))
        .rounded(px(8.0))
        .bg(if warning {
            theme.warning_muted
        } else {
            theme.element_hover
        })
        .text_size(ui_rems(12.0))
        .text_color(if warning {
            theme.text
        } else {
            theme.text_muted
        })
        .child(text.into())
}

impl GitHubIssuePicker {
    fn render_sources(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        match &self.sources {
            Sources::Loading => {
                meta_text(theme, "Resolving repositories on the project's host…").into_any_element()
            }
            Sources::Failed(error) => notice(theme, error.clone(), true).into_any_element(),
            Sources::Ready(sources) if sources.sources.is_empty() => notice(
                theme,
                sources
                    .error
                    .as_ref()
                    .map(source_error_text)
                    .unwrap_or_else(|| {
                        "No GitHub repository is available for this project.".into()
                    }),
                true,
            )
            .into_any_element(),
            Sources::Ready(sources) => {
                let several = sources.sources.len() > 1;
                let mut row = div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(ui_rems(6.0));
                if several && self.source_id.is_none() {
                    row = row.child(meta_text(
                        theme,
                        "This checkout has several GitHub remotes. Choose the issue repository:",
                    ));
                }
                for source in &sources.sources {
                    let selected = self.source_id.as_deref() == Some(source.id.as_str());
                    if !several && selected {
                        row = row.child(meta_text(
                            theme,
                            format!(
                                "{} · @{}",
                                source_label(source),
                                source.account.as_deref().unwrap_or("unknown")
                            ),
                        ));
                        continue;
                    }
                    let id = source.id.clone();
                    row = row.child(
                        action_button(
                            theme,
                            format!("source:{}", source.id),
                            &source_label(source),
                            if selected {
                                ButtonKind::Secondary
                            } else {
                                ButtonKind::Ghost
                            },
                            true,
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.select_source(id.clone(), cx)),
                        ),
                    );
                }
                if let Some(account) = sources
                    .sources
                    .iter()
                    .find(|source| Some(source.id.as_str()) == self.source_id.as_deref())
                    .and_then(|source| source.account.clone())
                    && several
                {
                    row = row.child(meta_text(theme, format!("@{account}")));
                }
                row.into_any_element()
            }
        }
    }

    fn render_freshness(&self, theme: &Theme) -> Option<AnyElement> {
        let page = self.page.as_ref()?;
        let mut parts = vec![format!("Updated {}", ago(page.freshness.fetched_at_ms))];
        if page.freshness.from_cache {
            parts.push("cached".into());
        }
        if page.freshness.stale {
            parts.push("may be out of date".into());
        }
        if let Some(account) = &page.freshness.account {
            parts.push(format!("@{account}"));
        }
        Some(meta_text(theme, parts.join(" · ")).into_any_element())
    }

    fn render_list(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div()
            .id("board-gh-results")
            .flex()
            .flex_col()
            .gap(ui_rems(2.0))
            .h_full()
            .overflow_y_scroll();
        if self.source_id.is_none() {
            return list.into_any_element();
        }
        if self.results.is_empty() {
            let text = if self.searching {
                "Searching…".to_owned()
            } else if self.search_error.is_some() {
                String::new()
            } else {
                "No matching issues.".to_owned()
            };
            if !text.is_empty() {
                list = list.child(meta_text(theme, text).p(ui_rems(8.0)));
            }
        }
        for row in &self.results {
            let number = row.number;
            let selected = self.selected == Some(number);
            let mut meta = Vec::new();
            meta.push(row.state.clone());
            if !row.labels.is_empty() {
                meta.push(row.labels.join(", "));
            }
            if !row.assignees.is_empty() {
                meta.push(
                    row.assignees
                        .iter()
                        .map(|login| format!("@{login}"))
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
            meta.push(format!("updated {}", day(&row.updated_at)));
            if row.comments > 0 {
                meta.push(format!("{} comments", row.comments));
            }
            let on_board = row
                .linked_work_item_id
                .as_ref()
                .map(|id| self.key_for(id, cx).unwrap_or_else(|| "a card".into()));
            list = list.child(
                div()
                    .id(ElementId::Name(format!("board-gh-row-{number}").into()))
                    .relative()
                    .px(ui_rems(10.0))
                    .py(ui_rems(6.0))
                    .rounded(px(8.0))
                    .cursor_pointer()
                    .when(selected, |el| el.bg(theme.element_active))
                    .hover(|style| style.bg(theme.element_hover))
                    .role(gpui::Role::ListItem)
                    .aria_label(format!("Issue #{number}: {}", row.title))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_issue(number, cx)))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(ui_rems(6.0))
                            .child(
                                div()
                                    .text_size(ui_rems(12.0))
                                    .text_color(theme.text_muted)
                                    .child(format!("#{number}")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui_rems(13.0))
                                    .text_color(theme.text)
                                    .child(row.title.clone()),
                            )
                            .children(on_board.map(|key| {
                                div()
                                    .px(ui_rems(6.0))
                                    .rounded(px(6.0))
                                    .bg(theme.accent_wash)
                                    .text_size(ui_rems(11.0))
                                    .text_color(theme.accent)
                                    .child(format!("On board · {key}"))
                            })),
                    )
                    .child(meta_text(theme, meta.join(" · ")).truncate())
                    .children(probe(format!("row:{number}"))),
            );
        }
        let mut footer = Vec::new();
        if self.pull_requests_excluded > 0 {
            footer.push(format!(
                "{} pull request{} hidden (issues only).",
                self.pull_requests_excluded,
                if self.pull_requests_excluded == 1 {
                    ""
                } else {
                    "s"
                }
            ));
        }
        if let Some(page) = &self.page
            && page.partial
        {
            footer.push(if page.next_cursor.is_some() || page.error.is_some() {
                "Partial results: some pages are not loaded.".into()
            } else {
                "Partial results: GitHub stopped early. Narrow the search to see the rest.".into()
            });
        }
        list = list.children(
            footer
                .into_iter()
                .map(|text| meta_text(theme, text).px(ui_rems(10.0)).py(ui_rems(4.0))),
        );
        if let Some(cursor) = self.page.as_ref().and_then(|page| page.next_cursor.clone()) {
            list = list.child(
                div().px(ui_rems(10.0)).py(ui_rems(4.0)).child(
                    action_button(
                        theme,
                        "more",
                        "Load more",
                        ButtonKind::Ghost,
                        !self.searching,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_search(Some(cursor.clone()), false, cx)
                    })),
                ),
            );
        }
        list.into_any_element()
    }

    /// Issue content scrolls; the prompt and the actions stay in view below.
    fn render_preview(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut pane = div()
            .id("board-gh-preview")
            .flex()
            .flex_col()
            .gap(ui_rems(10.0))
            .size_full()
            .overflow_y_scroll();
        let Some(detail) = &self.detail else {
            let text = if let Some(error) = &self.detail_error {
                error.clone()
            } else if self.selected.is_some() {
                "Loading issue…".into()
            } else {
                "Select an issue to preview it.".into()
            };
            return pane.child(meta_text(theme, text)).into_any_element();
        };
        let summary = &detail.summary;
        let url = summary.canonical_url.clone();
        let linked = summary.linked_work_item_id.clone();
        pane = pane
            .child(
                div()
                    .text_size(ui_rems(15.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(summary.title.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap(ui_rems(8.0))
                    .child(meta_text(
                        theme,
                        format!(
                            "#{} · {} · updated {}",
                            summary.number,
                            summary.state,
                            day(&summary.updated_at)
                        ),
                    ))
                    .child(meta_text(theme, url.clone()))
                    .child(
                        action_button(
                            theme,
                            "open-github",
                            "Open on GitHub",
                            ButtonKind::Ghost,
                            true,
                        )
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
            );
        if !summary.labels.is_empty() || !summary.assignees.is_empty() {
            pane = pane.child(meta_text(
                theme,
                format!(
                    "Labels: {} · Assignees: {}",
                    if summary.labels.is_empty() {
                        "none".into()
                    } else {
                        summary.labels.join(", ")
                    },
                    if summary.assignees.is_empty() {
                        "none".into()
                    } else {
                        summary
                            .assignees
                            .iter()
                            .map(|login| format!("@{login}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ),
            ));
        }
        if detail.freshness.stale {
            pane = pane.child(notice(
                theme,
                "This preview may be out of date (GitHub did not answer). Refresh before adding.",
                true,
            ));
        }
        let body = if detail.body.trim().is_empty() {
            "(The issue has no description.)".to_owned()
        } else {
            detail.body.clone()
        };
        pane = pane.child(
            div()
                .id("board-gh-body")
                .relative()
                .p(ui_rems(10.0))
                .rounded(px(8.0))
                .bg(theme.code_wash)
                .text_size(ui_rems(12.0))
                .text_color(theme.text)
                .whitespace_normal()
                .child(body)
                .children(probe("body")),
        );
        if let Some(omitted) = &detail.omitted_summary {
            pane = pane.child(notice(theme, omitted.clone(), true));
        }
        // Comments: on demand, selectable.
        let comment_count = summary.comments;
        if comment_count > 0 && !self.comments_requested {
            pane = pane.child(
                action_button(
                    theme,
                    "load-comments",
                    &format!("Load comments ({comment_count})"),
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| this.load_comments(cx))),
            );
        }
        if let Some(error) = &self.comments_error {
            pane = pane.child(notice(theme, error.clone(), true));
        }
        if !self.comments.is_empty() {
            pane = pane.child(meta_text(
                theme,
                format!(
                    "Select comments to include ({} selected). Unselected comments stay out of the brief.",
                    self.selected_comments.len()
                ),
            ));
        }
        for comment in &self.comments {
            let id = comment.id.clone();
            let checked = self.selected_comments.contains(&comment.id);
            pane = pane.child(
                div()
                    .id(ElementId::Name(
                        format!("board-gh-comment-{}", comment.id).into(),
                    ))
                    .relative()
                    .flex()
                    .flex_row()
                    .gap(ui_rems(8.0))
                    .p(ui_rems(8.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(if checked { theme.accent } else { theme.border })
                    .cursor_pointer()
                    .role(gpui::Role::CheckBox)
                    .aria_label(format!("Include comment by @{}", comment.author))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_comment(id.clone(), cx)),
                    )
                    .child(
                        div()
                            .size(px(16.0))
                            .flex_none()
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(theme.border_strong)
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(checked, |el| {
                                el.bg(theme.accent).child(
                                    icon(icons::CHECK)
                                        .size(px(12.0))
                                        .text_color(theme.on_accent),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(ui_rems(4.0))
                            .child(meta_text(
                                theme,
                                format!("@{} · {}", comment.author, day(&comment.created_at)),
                            ))
                            .child(
                                div()
                                    .text_size(ui_rems(12.0))
                                    .text_color(theme.text)
                                    .whitespace_normal()
                                    .line_clamp(6)
                                    .child(comment.body.clone()),
                            )
                            .children(comment.body_truncated.then(|| {
                                meta_text(theme, "Comment truncated for the preview and brief.")
                            })),
                    )
                    .children(probe(format!("comment:{}", comment.id))),
            );
        }
        if self.comments_next.is_some() && self.comments_task.is_none() {
            pane = pane.child(
                action_button(
                    theme,
                    "more-comments",
                    "Load more comments",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| this.load_comments(cx))),
            );
        }
        // The user's editable initial prompt (additions to the brief).
        let mut footer = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(ui_rems(8.0))
            .pt(ui_rems(8.0))
            .border_t_1()
            .border_color(theme.border);
        footer = footer.child(
            div()
                .flex()
                .flex_col()
                .gap(ui_rems(4.0))
                .child(meta_text(
                    theme,
                    "Initial prompt — added above the quoted issue in the ticket brief:",
                ))
                .child(
                    div()
                        .relative()
                        .min_h(ui_rems(64.0))
                        .px(ui_rems(10.0))
                        .py(ui_rems(8.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.input_bg)
                        .cursor_text()
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                focus_field(&this.prompt, window, cx)
                            }),
                        )
                        .child(self.prompt.clone())
                        .children(probe("prompt")),
                ),
        );
        if let Some(error) = &self.import_error {
            footer = footer.child(notice(theme, error.clone(), true));
        }
        if let Some(status) = self.import_status() {
            footer = footer.child(notice(theme, status, false));
        }
        let busy = self.import_task.is_some();
        let actions = match linked {
            Some(work_item_id) => {
                let key = self
                    .key_for(&work_item_id, cx)
                    .unwrap_or_else(|| "a card".into());
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(8.0))
                    .child(meta_text(
                        theme,
                        format!("Already on board as {key}. Another attempt starts from its card."),
                    ))
                    .child(div().flex_1())
                    .child(
                        action_button(theme, "open-card", "Open card", ButtonKind::Primary, true)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(GitHubPickerEvent::OpenCard {
                                    work_item_id: work_item_id.clone(),
                                })
                            })),
                    )
            }
            None => div()
                .flex()
                .flex_row()
                .items_center()
                .gap(ui_rems(8.0))
                .child(meta_text(theme, "Neither action edits the GitHub issue."))
                .child(div().flex_1())
                .child(
                    action_button(
                        theme,
                        "start",
                        "Start session",
                        ButtonKind::Secondary,
                        !busy,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.import(true, cx))),
                )
                .child(
                    action_button(theme, "add", "Add to board", ButtonKind::Primary, !busy)
                        .on_click(cx.listener(|this, _, _, cx| this.import(false, cx))),
                ),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(ui_rems(8.0))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(pane)
                    .children(probe("preview-scroll")),
            )
            .child(footer.child(actions))
            .into_any_element()
    }
}

impl Render for GitHubIssuePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_pending) {
            window.focus(&self.search.focus_handle(cx), cx);
        }
        let theme = Theme::of(cx).for_popup();
        let filters = [
            (IssueStateFilter::Open, "Open"),
            (IssueStateFilter::Closed, "Closed"),
            (IssueStateFilter::All, "All"),
        ];
        let project = self.project_id.clone();
        let _ = project;
        div()
            .id("board-github-picker")
            .track_focus(&self.focus)
            .key_context("BoardGitHubPicker")
            .w(ui_rems(PICKER_WIDTH))
            .h(ui_rems(PICKER_HEIGHT))
            .max_w(px(f32::from(window.viewport_size().width) - 32.0))
            .max_h(px(f32::from(window.viewport_size().height) - 32.0))
            .p(ui_rems(20.0))
            .rounded(px(16.0))
            .bg(crate::popover::surface_bg(&theme))
            .border_1()
            .border_color(crate::theme::separator(false))
            .flex()
            .flex_col()
            .gap(ui_rems(12.0))
            .text_color(theme.text)
            .role(gpui::Role::Dialog)
            .aria_label("From GitHub issue")
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(GitHubPickerEvent::Dismissed);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(8.0))
                    .child(crate::popover::dialog_title(&theme, "From GitHub issue"))
                    .child(div().flex_1())
                    .children(self.render_freshness(&theme))
                    .child(
                        action_button(&theme, "refresh", "Refresh", ButtonKind::Ghost, true)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(
                        action_button(&theme, "close", "Close", ButtonKind::Ghost, true).on_click(
                            cx.listener(|_, _, _, cx| cx.emit(GitHubPickerEvent::Dismissed)),
                        ),
                    ),
            )
            .child(self.render_sources(&theme, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(ui_rems(8.0))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .px(ui_rems(10.0))
                            .py(ui_rems(6.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.input_bg)
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(ui_rems(6.0))
                            .cursor_text()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    focus_field(&this.search, window, cx)
                                }),
                            )
                            .child(
                                icon(icons::PALETTE_SEARCH)
                                    .size(px(14.0))
                                    .text_color(theme.text_muted),
                            )
                            .child(div().flex_1().child(self.search.clone()))
                            .children(probe("search")),
                    )
                    .children(filters.into_iter().map(|(filter, label)| {
                        action_button(
                            &theme,
                            format!("state-{label}"),
                            label,
                            if self.state_filter == filter {
                                ButtonKind::Secondary
                            } else {
                                ButtonKind::Ghost
                            },
                            self.source_id.is_some(),
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.set_state_filter(filter, cx)),
                        )
                    })),
            )
            .children(
                self.search_error
                    .clone()
                    .map(|error| notice(&theme, error, true)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .gap(ui_rems(16.0))
                    .child(
                        div()
                            .w(ui_rems(340.0))
                            .flex_none()
                            .h_full()
                            .child(self.render_list(&theme, cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_preview(&theme, cx)),
                    ),
            )
    }
}

// ---------------------------------------------------------------------------
// PB-3 writeback confirmation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum WritebackDialogEvent {
    Dismissed,
    Written(Box<IssueWritebackReceipt>),
}

enum WritebackStage {
    Editing,
    Previewing,
    Confirm(IssueWritebackPreview),
    Writing(IssueWritebackPreview),
    Done(IssueWritebackReceipt),
    /// The write was sent but its result could not be confirmed. Check
    /// again reconciles with the same request id and never resends.
    Unknown(IssueWritebackPreview),
}

/// Explicit writeback: edit → preview (exact account/repo/issue/payload) →
/// confirm → engine write + independent readback.
pub struct IssueWritebackDialog {
    model: Entity<BoardModel>,
    work_item_id: String,
    issue_ref_id: String,
    display: String,
    kind: WritebackKind,
    input: Entity<ComposerInput>,
    stage: WritebackStage,
    error: Option<String>,
    /// Generated on confirm; every retry reuses it.
    request_id: Option<String>,
    task: Option<Task<()>>,
    focus_pending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WritebackKind {
    Comment,
    AddLabel,
    RemoveLabel,
    Close,
}

impl EventEmitter<WritebackDialogEvent> for IssueWritebackDialog {}

impl IssueWritebackDialog {
    pub fn new(
        model: Entity<BoardModel>,
        work_item_id: String,
        issue_ref_id: String,
        display: String,
        choice: WritebackChoice,
        cx: &mut Context<Self>,
    ) -> Self {
        let (kind, text, placeholder) = match choice {
            WritebackChoice::Comment { draft } => (WritebackKind::Comment, draft, "Comment"),
            WritebackChoice::AddLabel => (WritebackKind::AddLabel, String::new(), "Label"),
            WritebackChoice::RemoveLabel { label } => (WritebackKind::RemoveLabel, label, "Label"),
            WritebackChoice::Close => (WritebackKind::Close, String::new(), "Reason"),
        };
        let input = cx.new(|cx| {
            let input = ComposerInput::new(placeholder, cx);
            if kind == WritebackKind::Comment {
                input
            } else {
                input.with_single_line()
            }
        });
        input.update(cx, |input, cx| input.set_text(text, cx));
        Self {
            model,
            work_item_id,
            issue_ref_id,
            display,
            kind,
            input,
            stage: WritebackStage::Editing,
            error: None,
            request_id: None,
            task: None,
            focus_pending: true,
        }
    }

    fn action(&self, cx: &App) -> IssueWritebackAction {
        let text = self.input.read(cx).text().trim().to_owned();
        match self.kind {
            WritebackKind::Comment => IssueWritebackAction::Comment { body: text },
            WritebackKind::AddLabel => IssueWritebackAction::AddLabel { label: text },
            WritebackKind::RemoveLabel => IssueWritebackAction::RemoveLabel { label: text },
            WritebackKind::Close => IssueWritebackAction::Close {
                reason: if text.is_empty() {
                    "completed".into()
                } else {
                    text
                },
            },
        }
    }

    pub fn review(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.model.read(cx).api(cx) else {
            return;
        };
        let request = BoardIssueWritebackPreviewRequest {
            work_item_id: self.work_item_id.clone(),
            issue_ref_id: self.issue_ref_id.clone(),
            action: self.action(cx),
        };
        self.stage = WritebackStage::Previewing;
        self.error = None;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = api.issue_writeback_preview(request).await;
            this.update(cx, |dialog, cx| {
                match result {
                    Ok(preview) => {
                        dialog.request_id = None;
                        dialog.stage = WritebackStage::Confirm(preview);
                    }
                    Err(error) => {
                        dialog.error = Some(error.message());
                        dialog.stage = WritebackStage::Editing;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        let preview = match &self.stage {
            WritebackStage::Confirm(preview) | WritebackStage::Unknown(preview) => preview.clone(),
            _ => return,
        };
        let Some(api) = self.model.read(cx).api(cx) else {
            return;
        };
        let request = BoardIssueWritebackRequest {
            request_id: self
                .request_id
                .get_or_insert_with(|| format!("gh-write-{}", uuid::Uuid::new_v4().simple()))
                .clone(),
            work_item_id: self.work_item_id.clone(),
            preview_id: preview.preview_id.clone(),
        };
        self.stage = WritebackStage::Writing(preview.clone());
        self.error = None;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let mut result = api.issue_writeback(request.clone()).await;
            if matches!(result, Err(BoardCallError::Lost)) {
                // Same request id: the engine reconciles, never resends.
                result = api.issue_writeback(request).await;
            }
            this.update(cx, |dialog, cx| {
                match result {
                    Ok(receipt) => {
                        cx.emit(WritebackDialogEvent::Written(Box::new(receipt.clone())));
                        dialog.stage = WritebackStage::Done(receipt);
                    }
                    Err(BoardCallError::Board(luna_proto::BoardError::OutcomeUnknown { .. }))
                    | Err(BoardCallError::Lost) => {
                        dialog.error = Some(
                            "GitHub did not confirm the write. Check again reads it back without sending it twice."
                                .into(),
                        );
                        dialog.stage = WritebackStage::Unknown(preview);
                    }
                    Err(error) => {
                        dialog.error = Some(error.message());
                        dialog.stage = WritebackStage::Editing;
                        dialog.request_id = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn confirm_label(&self) -> &'static str {
        match self.kind {
            WritebackKind::Comment => "Post comment",
            WritebackKind::AddLabel => "Add label",
            WritebackKind::RemoveLabel => "Remove label",
            WritebackKind::Close => "Close issue",
        }
    }
}

fn payload_text(action: &IssueWritebackAction) -> String {
    match action {
        IssueWritebackAction::Comment { body } => body.clone(),
        IssueWritebackAction::AddLabel { label } => format!("Add label: {label}"),
        IssueWritebackAction::RemoveLabel { label } => format!("Remove label: {label}"),
        IssueWritebackAction::Close { reason } => format!("Close issue (state_reason: {reason})"),
    }
}

impl Render for IssueWritebackDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_pending) {
            window.focus(&self.input.focus_handle(cx), cx);
        }
        let theme = Theme::of(cx).for_popup();
        let title = match self.kind {
            WritebackKind::Comment => format!("Comment on {}", self.display),
            WritebackKind::AddLabel => format!("Add a label to {}", self.display),
            WritebackKind::RemoveLabel => format!("Remove a label from {}", self.display),
            WritebackKind::Close => format!("Close {}", self.display),
        };
        let mut card = crate::popover::dialog_card(&theme)
            .id("board-gh-writeback")
            .w(ui_rems(520.0))
            .gap(ui_rems(10.0))
            .role(gpui::Role::Dialog)
            .aria_label(title.clone())
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(WritebackDialogEvent::Dismissed);
                    cx.stop_propagation();
                }
            }))
            .child(crate::popover::dialog_title(&theme, &title));
        let target = |preview: &IssueWritebackPreview| {
            format!(
                "As @{} on {} · {}/{}#{} · {}",
                preview.account.as_deref().unwrap_or("unknown"),
                preview.repository.host,
                preview.repository.owner,
                preview.repository.name,
                preview.number,
                preview.canonical_url
            )
        };
        let payload = |preview: &IssueWritebackPreview| {
            div()
                .id("board-gh-writeback-payload")
                .relative()
                .max_h(ui_rems(240.0))
                .overflow_y_scroll()
                .p(ui_rems(10.0))
                .rounded(px(8.0))
                .bg(theme.code_wash)
                .text_size(ui_rems(12.0))
                .whitespace_normal()
                .child(payload_text(&preview.action))
                .children(probe("writeback-payload"))
        };
        let mut buttons = div()
            .mt(ui_rems(6.0))
            .flex()
            .flex_row()
            .justify_end()
            .gap(ui_rems(8.0))
            .child(
                action_button(
                    &theme,
                    "writeback-cancel",
                    "Cancel",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(|_, _, _, cx| cx.emit(WritebackDialogEvent::Dismissed))),
            );
        match &self.stage {
            WritebackStage::Editing | WritebackStage::Previewing => {
                card = card
                    .child(meta_text(
                        &theme,
                        "Nothing is sent until you review the exact target and payload.",
                    ))
                    .child(crate::popover::dialog_field(
                        self.input.clone().into_any_element(),
                    ));
                buttons = buttons.child(
                    action_button(
                        &theme,
                        "writeback-review",
                        "Review",
                        ButtonKind::Primary,
                        matches!(self.stage, WritebackStage::Editing),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.review(cx))),
                );
            }
            WritebackStage::Confirm(preview) | WritebackStage::Writing(preview) => {
                card = card
                    .child(meta_text(&theme, target(preview)))
                    .child(meta_text(&theme, "Exactly this will be sent:"))
                    .child(payload(preview));
                if let WritebackStage::Confirm(_) = self.stage {
                    buttons = buttons
                        .child(
                            action_button(
                                &theme,
                                "writeback-edit",
                                "Edit",
                                ButtonKind::Ghost,
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.stage = WritebackStage::Editing;
                                cx.notify();
                            })),
                        )
                        .child(
                            action_button(
                                &theme,
                                "writeback-confirm",
                                self.confirm_label(),
                                ButtonKind::Primary,
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                        );
                } else {
                    card = card.child(meta_text(&theme, "Writing, then reading it back…"));
                }
            }
            WritebackStage::Unknown(preview) => {
                card = card
                    .child(meta_text(&theme, target(preview)))
                    .child(payload(preview));
                buttons = buttons.child(
                    action_button(
                        &theme,
                        "writeback-check",
                        "Check again",
                        ButtonKind::Primary,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                );
            }
            WritebackStage::Done(receipt) => {
                let url = receipt.canonical_url.clone();
                card = card.child(notice(
                    &theme,
                    if receipt.readback_verified {
                        format!("Done and verified on GitHub: {}", receipt.outcome.label)
                    } else {
                        format!(
                            "Sent; GitHub has not confirmed it yet: {}",
                            receipt.outcome.label
                        )
                    },
                    !receipt.readback_verified,
                ));
                buttons = buttons.child(
                    action_button(
                        &theme,
                        "writeback-open",
                        "Open on GitHub",
                        ButtonKind::Primary,
                        true,
                    )
                    .on_click(move |_, _, cx| cx.open_url(&url)),
                );
            }
        }
        if let Some(error) = &self.error {
            card = card.child(notice(&theme, error.clone(), true));
        }
        card.child(buttons)
    }
}

// ---------------------------------------------------------------------------
// Shell hosting
// ---------------------------------------------------------------------------

/// The GitHub overlay the Shell shows over the Board page.
pub(crate) enum BoardGitHubOverlay {
    Picker(Entity<GitHubIssuePicker>),
    Writeback(Entity<IssueWritebackDialog>),
}

pub(crate) struct BoardGitHubHost {
    overlay: BoardGitHubOverlay,
    _events: Subscription,
}

impl super::super::Shell {
    /// Open "From GitHub issue" for a project (the Board page's New ticket
    /// menu calls this).
    pub fn open_board_github_picker(&mut self, project_id: String, cx: &mut Context<Self>) {
        let page = self.board_page(cx);
        let model = page.read(cx).model().clone();
        let picker = cx.new(|cx| GitHubIssuePicker::new(model.clone(), project_id, cx));
        let events = cx.subscribe(&picker, move |shell: &mut Self, _, event, cx| match event {
            GitHubPickerEvent::Dismissed => {
                shell.board_github = None;
                shell.board_restore_drawer_focus(cx);
                cx.notify();
            }
            GitHubPickerEvent::OpenCard { work_item_id } => {
                shell.board_github = None;
                // The Board page owns its selection: it opens the card's
                // detail drawer and selects it (saved with its prefs).
                let request = super::page::BoardPageRequest::OpenDetail {
                    work_item_id: work_item_id.clone(),
                };
                model.update(cx, |_, cx| cx.emit(request));
                cx.notify();
            }
            GitHubPickerEvent::Imported(receipt) => {
                model.update(cx, |model, cx| model.apply_receipt(&receipt.mutation, cx));
            }
        });
        self.board_github = Some(BoardGitHubHost {
            overlay: BoardGitHubOverlay::Picker(picker),
            _events: events,
        });
        cx.notify();
    }

    /// Open the explicit writeback confirmation for a linked issue (the
    /// detail drawer's source-issue panel calls this).
    pub fn open_board_issue_writeback(
        &mut self,
        work_item_id: String,
        issue_ref_id: String,
        display: String,
        choice: WritebackChoice,
        cx: &mut Context<Self>,
    ) {
        let page = self.board_page(cx);
        let model = page.read(cx).model().clone();
        let dialog = cx.new(|cx| {
            IssueWritebackDialog::new(
                model.clone(),
                work_item_id,
                issue_ref_id,
                display,
                choice,
                cx,
            )
        });
        let events = cx.subscribe(&dialog, move |shell: &mut Self, _, event, cx| match event {
            WritebackDialogEvent::Dismissed => {
                shell.board_github = None;
                // Escape must reach the drawer again (focus left with the
                // dialog).
                shell.board_restore_drawer_focus(cx);
                cx.notify();
            }
            WritebackDialogEvent::Written(receipt) => {
                tracing::info!(url = %receipt.canonical_url, verified = receipt.readback_verified, "board issue writeback confirmed");
                model.update(cx, |model, cx| model.refresh(cx));
            }
        });
        self.board_github = Some(BoardGitHubHost {
            overlay: BoardGitHubOverlay::Writeback(dialog),
            _events: events,
        });
        cx.notify();
    }

    /// The GitHub overlay (picker or writeback), as a modal.
    pub(crate) fn render_board_github_overlay(
        &mut self,
        viewport: gpui::Size<gpui::Pixels>,
        _cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let host = self.board_github.as_ref()?;
        let card = match &host.overlay {
            BoardGitHubOverlay::Picker(picker) => picker.clone().into_any_element(),
            BoardGitHubOverlay::Writeback(dialog) => dialog.clone().into_any_element(),
        };
        Some(crate::popover::modal(
            "board-github-overlay",
            viewport,
            card,
        ))
    }

    #[cfg_attr(not(feature = "appshots-fixture"), allow(dead_code))]
    fn board_github_picker(&self) -> Option<Entity<GitHubIssuePicker>> {
        match self.board_github.as_ref().map(|host| &host.overlay) {
            Some(BoardGitHubOverlay::Picker(picker)) => Some(picker.clone()),
            _ => None,
        }
    }

    /// QA: what the open picker shows plus the Board page's cards.
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_board_github_readback(&self, cx: &App) -> serde_json::Value {
        let cards = self
            .board_page
            .as_ref()
            .map(|page| {
                page.read(cx)
                    .model()
                    .read(cx)
                    .items()
                    .iter()
                    .map(|view| {
                        serde_json::json!({
                            "id": view.item.id,
                            "key": view.item.key,
                            "title": view.item.title,
                            "origin": view.item.origin,
                            "issue": view.issue,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        serde_json::json!({
            "route": format!("{:?}", self.route),
            "picker": self
                .board_github_picker()
                .map(|picker| picker.read(cx).fixture_readback(cx)),
            "cards": cards,
        })
    }

    /// QA: re-render the picker so its layout probes repaint.
    #[cfg(feature = "appshots-fixture")]
    pub fn fixture_board_github_repaint(&self, cx: &mut App) {
        if let Some(picker) = self.board_github_picker() {
            picker.update(cx, |_, cx| cx.notify());
        }
    }
}
