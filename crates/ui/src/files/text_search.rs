//! Literal workspace-text search mode of the Files surface (ZI-05).
//!
//! The chat's owning host runs the scan (`SearchWorkspaceText`). This mode
//! fences every response by query, generation and workspace context, keeps
//! complete, partial, unsupported and unreachable answers distinct, and opens
//! a match through the existing Files editor only after re-validating the
//! literal against the text the editor shows (see `preview.rs`).

use std::{collections::HashSet, ops::Range, time::Duration};

use gpui::{
    AnyElement, Context, Focusable as _, ListAlignment, ListSizingBehavior, ListState,
    SharedString, Task, Window, div, list, prelude::*, px,
};
use luna_proto::{
    SearchWorkspaceTextRequest, WorkspaceTextSearchBudget, WorkspaceTextSearchLimit,
    WorkspaceTextSearchResult, WorkspaceTextSearchStatus,
};

use super::{
    FilesEvent, FilesPresentation, FilesSurface,
    client::{TextSearchError, WorkspaceFilesClient},
    search::SEARCH_ROW_HEIGHT,
};
use crate::interaction::HoverFill as _;
use crate::{
    file_icons::{self, FileIconIdentity},
    icons::{self, icon},
    theme::Theme,
};

const TEXT_SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);
const TEXT_SEARCH_LIMIT: u16 = 200;
const TEXT_MATCH_INDENT: f32 = 26.0;
/// The as-you-type pass stays well inside the host maximum so typing does not
/// hold a scan slot for long; "Search all files" reruns with the host maximum.
const INTERACTIVE_BUDGET: WorkspaceTextSearchBudget = WorkspaceTextSearchBudget {
    max_files: Some(25_000),
    max_bytes: Some(128 * 1024 * 1024),
    max_elapsed_ms: Some(1_500),
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum FileSearchMode {
    #[default]
    Names,
    Text,
}

/// Where a selected match should land, captured from the host's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextNavigationTarget {
    pub path: String,
    /// 1-based line.
    pub line: u32,
    /// UTF-8 byte range inside that line's text.
    pub byte_start: u32,
    pub byte_end: u32,
    pub query: String,
    pub case_sensitive: bool,
    /// Hash of the bytes the host searched.
    pub content_hash: String,
    pub checkout_id: String,
}

/// The literal located again in the text the user now sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TextLocation {
    /// Still at the searched line and byte range.
    Exact { range: Range<usize>, row: usize },
    /// Gone from the searched position; this is the nearest occurrence.
    Moved { range: Range<usize>, row: usize },
    /// No occurrence remains; the cursor goes to the searched line (clamped).
    Missing { offset: usize, row: usize },
}

impl TextLocation {
    pub(super) fn selection(&self) -> Range<usize> {
        match self {
            Self::Exact { range, .. } | Self::Moved { range, .. } => range.clone(),
            Self::Missing { offset, .. } => *offset..*offset,
        }
    }

    pub(super) fn row(&self) -> usize {
        match self {
            Self::Exact { row, .. } | Self::Moved { row, .. } | Self::Missing { row, .. } => *row,
        }
    }
}

fn literal_eq(candidate: &[u8], query: &[u8], case_sensitive: bool) -> bool {
    candidate.len() == query.len()
        && if case_sensitive {
            candidate == query
        } else {
            candidate.eq_ignore_ascii_case(query)
        }
}

/// Re-locate `target` in `text`, the editor's (or read-only preview's) text.
/// Lines split on `\n`; a trailing `\r` is not part of a line, matching the
/// host's ranges.
pub(super) fn locate_text_match(text: &str, target: &TextNavigationTarget) -> TextLocation {
    let bytes = text.as_bytes();
    let query = target.query.as_bytes();
    let mut line_starts = vec![0usize];
    line_starts.extend(
        bytes
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'\n')
            .map(|(index, _)| index + 1),
    );
    let line_text = |row: usize| -> Range<usize> {
        let start = line_starts[row];
        let mut end = line_starts
            .get(row + 1)
            .map_or(bytes.len(), |next| next - 1);
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
        start..end
    };
    let wanted_row = (target.line.max(1) as usize - 1).min(line_starts.len() - 1);
    if (target.line as usize) <= line_starts.len() {
        let line = line_text(wanted_row);
        let start = line.start + target.byte_start as usize;
        let end = line.start + target.byte_end as usize;
        if start <= end
            && end <= line.end
            && text.is_char_boundary(start)
            && text.is_char_boundary(end)
            && !query.is_empty()
            && literal_eq(&bytes[start..end], query, target.case_sensitive)
        {
            return TextLocation::Exact {
                range: start..end,
                row: wanted_row,
            };
        }
    }
    if !query.is_empty() && query.len() <= bytes.len() {
        let mut nearest: Option<(usize, usize)> = None;
        let mut start = 0usize;
        while start + query.len() <= bytes.len() {
            if text.is_char_boundary(start)
                && literal_eq(
                    &bytes[start..start + query.len()],
                    query,
                    target.case_sensitive,
                )
                && text.is_char_boundary(start + query.len())
            {
                let row = line_starts.partition_point(|line_start| *line_start <= start) - 1;
                let distance = row.abs_diff(wanted_row);
                if nearest.is_none_or(|(_, best)| distance < best) {
                    nearest = Some((start, distance));
                }
                start += query.len();
            } else {
                start += 1;
            }
        }
        if let Some((start, _)) = nearest {
            let row = line_starts.partition_point(|line_start| *line_start <= start) - 1;
            return TextLocation::Moved {
                range: start..start + query.len(),
                row,
            };
        }
    }
    TextLocation::Missing {
        offset: line_text(wanted_row).start,
        row: wanted_row,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TextSearchPhase {
    Idle,
    Searching,
    Ready,
    Unsupported,
    Unreachable(SharedString),
    Failed(SharedString),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextSearchRow {
    File(usize),
    Match(usize, usize),
}

/// Stable identity of a result row, so a newer answer keeps the selection.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RowIdentity {
    path: String,
    at: Option<(u32, u32)>,
}

pub(super) struct TextSearchState {
    pub(super) query: String,
    pub(super) generation: u64,
    /// Lets the owning host cancel this surface's superseded scans.
    session: String,
    pub(super) phase: TextSearchPhase,
    result: Option<WorkspaceTextSearchResult>,
    thorough: bool,
    rows: Vec<TextSearchRow>,
    collapsed: HashSet<String>,
    pub(super) active: usize,
    pub(super) list: ListState,
    task: Option<Task<()>>,
}

impl Default for TextSearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            generation: 0,
            session: format!("files-{}", uuid::Uuid::new_v4()),
            phase: TextSearchPhase::Idle,
            result: None,
            thorough: false,
            rows: Vec::new(),
            collapsed: HashSet::new(),
            active: 0,
            list: ListState::new(0, ListAlignment::Top, px(420.0)),
            task: None,
        }
    }
}

impl TextSearchState {
    fn accepts(&self, generation: u64, query: &str) -> bool {
        self.generation == generation && self.query == query
    }

    pub(super) fn visible_len(&self) -> usize {
        self.rows.len()
    }

    /// Drop the answer and any in-flight request (a new query, mode or root).
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.task = None;
        self.result = None;
        self.rows.clear();
        self.collapsed.clear();
        self.active = 0;
        self.list.reset(0);
    }

    fn identity(&self, index: usize) -> Option<RowIdentity> {
        let result = self.result.as_ref()?;
        Some(match *self.rows.get(index)? {
            TextSearchRow::File(file) => RowIdentity {
                path: result.files.get(file)?.path.clone(),
                at: None,
            },
            TextSearchRow::Match(file, index) => {
                let file = result.files.get(file)?;
                let found = file.matches.get(index)?;
                RowIdentity {
                    path: file.path.clone(),
                    at: Some((found.line, found.byte_start)),
                }
            }
        })
    }

    fn rebuild_rows(&mut self, keep: Option<RowIdentity>) {
        self.rows.clear();
        if let Some(result) = &self.result {
            for (file_index, file) in result.files.iter().enumerate() {
                self.rows.push(TextSearchRow::File(file_index));
                if !self.collapsed.contains(&file.path) {
                    self.rows.extend(
                        (0..file.matches.len())
                            .map(|match_index| TextSearchRow::Match(file_index, match_index)),
                    );
                }
            }
        }
        self.active = keep
            .and_then(|keep| {
                (0..self.rows.len()).find(|index| self.identity(*index).as_ref() == Some(&keep))
            })
            .or_else(|| {
                self.rows
                    .iter()
                    .position(|row| matches!(row, TextSearchRow::Match(..)))
            })
            .unwrap_or(0);
        self.list
            .reset_with_uniform_height(self.rows.len(), px(SEARCH_ROW_HEIGHT));
    }

    /// Partial answers that a longer, explicit pass could complete.
    fn can_search_longer(&self) -> bool {
        !self.thorough
            && self.result.as_ref().is_some_and(|result| {
                result.limits.iter().any(|limit| {
                    matches!(
                        limit,
                        WorkspaceTextSearchLimit::Elapsed
                            | WorkspaceTextSearchLimit::Files
                            | WorkspaceTextSearchLimit::Bytes
                    )
                })
            })
    }
}

fn limit_label(limit: WorkspaceTextSearchLimit, result: &WorkspaceTextSearchResult) -> String {
    match limit {
        WorkspaceTextSearchLimit::Results => format!("showing the first {}", result.match_count),
        WorkspaceTextSearchLimit::FileMatches => "some files have more matches".into(),
        WorkspaceTextSearchLimit::Files => {
            format!("stopped after {} files", result.files_scanned)
        }
        WorkspaceTextSearchLimit::Bytes => "stopped at the size limit".into(),
        WorkspaceTextSearchLimit::Elapsed => format!(
            "stopped after {:.1} s",
            f64::from(result.elapsed_ms) / 1000.0
        ),
        WorkspaceTextSearchLimit::LargeFiles => {
            format!("{} large files not searched", result.skipped_large)
        }
        WorkspaceTextSearchLimit::Unreadable => {
            format!("{} entries unreadable", result.skipped_unreadable)
        }
        WorkspaceTextSearchLimit::Unknown => "incomplete".into(),
    }
}

fn plural(count: u32, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// The summary line. A partial or failed search never reads as "no matches".
fn text_search_summary(
    phase: &TextSearchPhase,
    result: Option<&WorkspaceTextSearchResult>,
) -> SharedString {
    match (phase, result) {
        (TextSearchPhase::Idle, _) => "".into(),
        (TextSearchPhase::Searching, _) => "Searching file contents…".into(),
        (TextSearchPhase::Unsupported, _) => {
            "This workspace's host runs an older LUNA that can't search file contents. Update LUNA on that device.".into()
        }
        (TextSearchPhase::Unreachable(_), _) => {
            "The device that owns this workspace is unreachable, so nothing was searched.".into()
        }
        (TextSearchPhase::Failed(message), _) => format!("Text search failed: {message}").into(),
        (TextSearchPhase::Ready, None) => "".into(),
        (TextSearchPhase::Ready, Some(result)) => {
            let found = if result.match_count == 0 {
                None
            } else {
                Some(format!(
                    "{} in {}",
                    plural(result.match_count, "match", "matches"),
                    plural(result.files.len() as u32, "file", "files")
                ))
            };
            if result.status == WorkspaceTextSearchStatus::Complete {
                found
                    .unwrap_or_else(|| {
                        format!(
                            "No matches in {}",
                            plural(result.files_scanned, "file", "files")
                        )
                    })
                    .into()
            } else {
                let reasons = result
                    .limits
                    .iter()
                    .map(|limit| limit_label(*limit, result))
                    .collect::<Vec<_>>();
                let reasons = if reasons.is_empty() {
                    "incomplete".to_string()
                } else {
                    reasons.join(", ")
                };
                match found {
                    Some(found) => format!("{found} so far · {reasons}"),
                    None => format!("No matches yet · {reasons}"),
                }
                .into()
            }
        }
    }
}

impl FilesSurface {
    pub(super) fn search_mode(&self) -> FileSearchMode {
        self.search_state.mode
    }

    /// The summary line shown above the text matches.
    pub(super) fn text_search_status(&self) -> SharedString {
        text_search_summary(
            &self.search_state.text.phase,
            self.search_state.text.result.as_ref(),
        )
    }

    /// Text results own the tree pane whenever a text query is present.
    pub(super) fn showing_text_search(&self) -> bool {
        self.search_state.mode == FileSearchMode::Text && !self.search_state.text.query.is_empty()
    }

    pub(super) fn set_search_mode(&mut self, mode: FileSearchMode, cx: &mut Context<Self>) {
        if self.search_state.mode == mode {
            return;
        }
        self.search_state.mode = mode;
        self.search.update(cx, |search, cx| {
            search.set_placeholder(
                match mode {
                    FileSearchMode::Names => "Search files",
                    FileSearchMode::Text => "Search text in files",
                },
                cx,
            )
        });
        // Results never carry across modes: the other mode's answer is not
        // an answer to this one.
        self.search_state.query.clear();
        self.search_state.results.clear();
        self.search_state.task = None;
        self.search_state.text.query.clear();
        self.search_state.text.phase = TextSearchPhase::Idle;
        self.search_state.text.invalidate();
        self.on_search_input_edited(cx);
    }

    pub(super) fn on_search_input_edited(&mut self, cx: &mut Context<Self>) {
        match self.search_state.mode {
            FileSearchMode::Names => self.on_search_edited(cx),
            FileSearchMode::Text => self.on_text_search_edited(false, cx),
        }
    }

    /// Open this surface's text mode for `query` (the command palette entry).
    pub fn start_text_search(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_search_mode(FileSearchMode::Text, cx);
        if self.presentation.is_editor() {
            self.show_tree_sidebar(cx);
        }
        self.search
            .update(cx, |search, cx| search.set_text(query, cx));
        let focus = self.search.focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
    }

    /// Re-run the current query after the root, host or ignore scope changed.
    pub(super) fn restart_text_search(&mut self, cx: &mut Context<Self>) {
        if self.search_state.text.query.is_empty() {
            return;
        }
        self.search_state.text.query.clear();
        self.on_text_search_edited(false, cx);
    }

    fn on_text_search_edited(&mut self, thorough: bool, cx: &mut Context<Self>) {
        // Literal: the raw input, never trimmed. An all-blank input is not a
        // search the user meant to run.
        let raw = self.search.read(cx).text().to_string();
        let query = if raw.trim().is_empty() {
            String::new()
        } else {
            raw
        };
        if self.search_state.text.query == query && !thorough {
            return;
        }
        let keep = self
            .search_state
            .text
            .identity(self.search_state.text.active);
        self.search_state.text.invalidate();
        self.search_state.text.query = query.clone();
        self.search_state.text.thorough = thorough;
        if query.is_empty() {
            self.search_state.text.phase = TextSearchPhase::Idle;
            self.search.update(cx, |search, cx| {
                search.set_mention_controls(false, false, cx)
            });
            cx.notify();
            return;
        }
        self.search.update(cx, |search, cx| {
            search.set_mention_controls(true, false, cx)
        });
        let Some(context) = self.request_context.clone() else {
            self.search_state.text.phase =
                TextSearchPhase::Failed("No workspace available for this chat.".into());
            cx.notify();
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.search_state.text.phase =
                TextSearchPhase::Failed("Workspace service is still starting.".into());
            cx.notify();
            return;
        };
        self.search_state.text.phase = TextSearchPhase::Searching;
        let generation = self.search_state.text.generation;
        let request = SearchWorkspaceTextRequest {
            target: context.target.clone(),
            query: query.clone(),
            include_ignored: self.tree.include_ignored(),
            limit: Some(TEXT_SEARCH_LIMIT),
            budget: (!thorough).then_some(INTERACTIVE_BUDGET),
            search_session: Some(self.search_state.text.session.clone()),
            generation,
        };
        let client = WorkspaceFilesClient::new(engine, context.clone());
        self.search_state.text.task = Some(cx.spawn(async move |this, cx| {
            if !thorough {
                cx.background_executor().timer(TEXT_SEARCH_DEBOUNCE).await;
            }
            let response = client.search_text(request).await;
            let _ = this.update(cx, |surface, cx| {
                // Only this query, generation and workspace may publish.
                if !surface.search_state.text.accepts(generation, &query)
                    || surface.request_context.as_ref() != Some(&context)
                {
                    return;
                }
                surface.apply_text_search_response(generation, &query, response, keep, cx);
            });
        }));
        cx.notify();
    }

    fn apply_text_search_response(
        &mut self,
        generation: u64,
        query: &str,
        response: Result<WorkspaceTextSearchResult, TextSearchError>,
        keep: Option<RowIdentity>,
        cx: &mut Context<Self>,
    ) {
        let state = &mut self.search_state.text;
        state.task = None;
        state.phase = match response {
            Ok(result) if result.generation != generation || result.query != query => {
                TextSearchPhase::Failed("The host answered a different search.".into())
            }
            Ok(result) if result.status == WorkspaceTextSearchStatus::Superseded => {
                TextSearchPhase::Failed("This search was replaced by a newer one.".into())
            }
            Ok(result) => {
                state.result = Some(result);
                TextSearchPhase::Ready
            }
            Err(TextSearchError::Unsupported) => TextSearchPhase::Unsupported,
            Err(TextSearchError::Unreachable(message)) => {
                TextSearchPhase::Unreachable(message.into())
            }
            Err(TextSearchError::Failed(message)) => TextSearchPhase::Failed(message.into()),
        };
        state.rebuild_rows(keep);
        let has_rows = !state.rows.is_empty();
        self.search.update(cx, |search, cx| {
            search.set_mention_controls(true, has_rows, cx)
        });
        cx.notify();
    }

    fn search_longer(&mut self, cx: &mut Context<Self>) {
        self.on_text_search_edited(true, cx);
    }

    pub(super) fn move_text_search_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.search_state.text.visible_len();
        if len == 0 {
            return;
        }
        let state = &mut self.search_state.text;
        state.active = if delta < 0 {
            state.active.saturating_sub(1)
        } else {
            (state.active + 1).min(len - 1)
        };
        state.list.scroll_to_reveal_item(state.active);
        cx.notify();
    }

    pub(super) fn activate_text_search_result(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.search_state.text;
        let Some(row) = state.rows.get(state.active).copied() else {
            return;
        };
        let Some(result) = state.result.as_ref() else {
            return;
        };
        match row {
            TextSearchRow::File(file) => {
                let Some(path) = result.files.get(file).map(|file| file.path.clone()) else {
                    return;
                };
                if !state.collapsed.remove(&path) {
                    state.collapsed.insert(path.clone());
                }
                state.rebuild_rows(Some(RowIdentity { path, at: None }));
                state.list.scroll_to_reveal_item(state.active);
                cx.notify();
            }
            TextSearchRow::Match(file, index) => {
                let Some(file) = result.files.get(file) else {
                    return;
                };
                let Some(found) = file.matches.get(index) else {
                    return;
                };
                let target = TextNavigationTarget {
                    path: file.path.clone(),
                    line: found.line,
                    byte_start: found.byte_start,
                    byte_end: found.byte_end,
                    query: result.query.clone(),
                    case_sensitive: result.case_sensitive,
                    content_hash: file.content_hash.clone(),
                    checkout_id: result.checkout_id.clone(),
                };
                self.open_text_match(target, cx);
            }
        }
    }

    /// Open through the existing Files tabs: a browser promotes itself to an
    /// editor, an editor tab navigates in place, and another file opens its
    /// own owning-chat tab at the match.
    fn open_text_match(&mut self, target: TextNavigationTarget, cx: &mut Context<Self>) {
        match self.presentation {
            FilesPresentation::Browser => {
                self.presentation = FilesPresentation::Editor;
                self.editor_path = Some(target.path.clone());
                self.navigate_to_text_match(target, cx);
                cx.emit(FilesEvent::TitleChanged);
            }
            FilesPresentation::Editor
                if self.editor_path.as_deref() == Some(target.path.as_str()) =>
            {
                self.navigate_to_text_match(target, cx);
            }
            FilesPresentation::Editor => cx.emit(FilesEvent::OpenFileAt {
                path: target.path.clone(),
                target,
            }),
        }
    }

    pub(super) fn render_text_search_results(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let summary = self.text_search_status();
        let state = &self.search_state.text;
        let incomplete = match &state.phase {
            TextSearchPhase::Ready => state
                .result
                .as_ref()
                .is_some_and(|result| result.status != WorkspaceTextSearchStatus::Complete),
            TextSearchPhase::Searching | TextSearchPhase::Idle => false,
            _ => true,
        };
        let failed = matches!(
            state.phase,
            TextSearchPhase::Unsupported
                | TextSearchPhase::Unreachable(_)
                | TextSearchPhase::Failed(_)
        );
        let search_longer = state.can_search_longer();
        let header = div()
            .id("files-text-search-summary")
            .role(gpui::Role::Status)
            .aria_label(summary.clone())
            .min_h(crate::typography::ui_rems(24.0))
            .flex_none()
            .px(crate::typography::ui_rems(10.0))
            .py(crate::typography::ui_rems(3.0))
            .flex()
            .items_center()
            .gap(crate::typography::ui_rems(8.0))
            .font_family(theme.font_sans.clone())
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(if failed {
                theme.danger.opacity(0.82)
            } else if incomplete {
                theme.readable_on(
                    theme.warning_muted,
                    theme.warning.opacity(0.045),
                    crate::theme::TEXT_CONTRAST_TARGET,
                )
            } else {
                theme.text_faint
            })
            .when(incomplete, |element| {
                element.bg(theme.warning.opacity(0.045))
            })
            .child(div().min_w_0().flex_1().whitespace_normal().child(summary))
            .when(search_longer, |element| {
                element.child(
                    div()
                        .id("files-text-search-longer")
                        .flex_none()
                        .h(crate::typography::ui_rems(20.0))
                        .px(crate::typography::ui_rems(6.0))
                        .rounded(px(5.0))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .role(gpui::Role::Button)
                        .aria_label("Search all files")
                        .tab_index(0)
                        .focus_visible(crate::theme::focus_visible_ring)
                        .hover_fill_fg(
                            "files-text-search-longer",
                            gpui::transparent_black(),
                            crate::theme::fill(crate::theme::FillTier::Tertiary),
                            theme.text_muted,
                            theme.text,
                        )
                        .child("Search all files")
                        .on_click(cx.listener(|this, _, _, cx| this.search_longer(cx))),
                )
            });
        div()
            .id("files-text-search-results")
            .role(gpui::Role::Tree)
            .aria_label("Workspace text matches")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(
                list(
                    self.search_state.text.list.clone(),
                    cx.processor(Self::render_text_search_row),
                )
                .flex_1()
                .min_h_0()
                .with_sizing_behavior(ListSizingBehavior::Auto),
            )
            .into_any_element()
    }

    fn render_text_search_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &self.search_state.text;
        let (Some(row), Some(result)) = (state.rows.get(index).copied(), state.result.as_ref())
        else {
            return gpui::Empty.into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let selected = state.active == index;
        let base = div()
            .id(("files-text-search-row", index))
            .role(gpui::Role::TreeItem)
            .aria_selected(selected)
            .h(px(SEARCH_ROW_HEIGHT))
            .w_full()
            .flex_none()
            .pr(crate::typography::ui_rems(8.0))
            .flex()
            .items_center()
            .gap(crate::typography::ui_rems(5.0))
            .cursor_pointer()
            .font_family(theme.font_sans.clone())
            .text_size(crate::typography::ui_rems(11.5))
            .when(selected, |element| {
                element.bg(crate::theme::fill(crate::theme::FillTier::Secondary))
            })
            .when(!selected, |element| {
                element.hover_wash(format!("files-text-search-row-{index}"))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.search_state.text.active = index;
                this.activate_text_search_result(cx);
            }));
        match row {
            TextSearchRow::File(file) => {
                let Some(file) = result.files.get(file) else {
                    return gpui::Empty.into_any_element();
                };
                let expanded = !state.collapsed.contains(&file.path);
                let (directory, name) = file
                    .path
                    .rsplit_once('/')
                    .map_or(("", file.path.as_str()), |(directory, name)| {
                        (directory, name)
                    });
                let count = if file.matches_truncated {
                    format!("{}+", file.matches.len())
                } else {
                    file.matches.len().to_string()
                };
                base.aria_label(SharedString::from(format!(
                    "{}, {count} matches",
                    file.path
                )))
                .aria_expanded(expanded)
                .pl(crate::typography::ui_rems(8.0))
                .child(
                    icon(if expanded {
                        icons::ALT_ARROW_DOWN
                    } else {
                        icons::ALT_ARROW_RIGHT
                    })
                    .size(crate::typography::ui_rems(11.0))
                    .flex_none()
                    .text_color(theme.text_faint),
                )
                .child(
                    file_icons::icon(FileIconIdentity::file(name), theme.appearance)
                        .size(crate::typography::ui_rems(14.0))
                        .flex_none(),
                )
                .child(
                    div()
                        .flex_none()
                        .max_w(crate::typography::ui_rems(180.0))
                        .truncate()
                        .text_color(theme.text)
                        .child(SharedString::from(name.to_string())),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_color(theme.text_faint)
                        .child(SharedString::from(directory.to_string())),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(crate::typography::ui_rems(10.5))
                        .text_color(theme.text_faint)
                        .child(count),
                )
                .into_any_element()
            }
            TextSearchRow::Match(file, match_index) => {
                let Some(file) = result.files.get(file) else {
                    return gpui::Empty.into_any_element();
                };
                let Some(found) = file.matches.get(match_index) else {
                    return gpui::Empty.into_any_element();
                };
                // Tabs and spaces are both one byte, so the host's preview
                // range stays valid after this display substitution.
                let preview = found.preview.replace('\t', " ");
                let start = (found.preview_match_start as usize).min(preview.len());
                let end = (found.preview_match_end as usize).clamp(start, preview.len());
                let highlight = (preview.is_char_boundary(start) && preview.is_char_boundary(end))
                    .then_some(start..end);
                let mut text = String::new();
                if found.preview_truncated_before {
                    text.push('…');
                }
                let offset = text.len();
                text.push_str(&preview);
                if found.preview_truncated_after {
                    text.push('…');
                }
                let label = SharedString::from(format!(
                    "{} line {}: {}",
                    file.path,
                    found.line,
                    preview.trim()
                ));
                let mut styled = gpui::StyledText::new(text);
                if let Some(range) = highlight {
                    styled = styled.with_highlights([(
                        range.start + offset..range.end + offset,
                        gpui::HighlightStyle {
                            color: Some(theme.code_text),
                            background_color: Some(theme.code_wash),
                            ..Default::default()
                        },
                    )]);
                }
                base.aria_label(label)
                    .pl(crate::typography::ui_rems(TEXT_MATCH_INDENT))
                    .child(
                        div()
                            .flex_none()
                            .min_w(crate::typography::ui_rems(28.0))
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.text_faint)
                            .child(found.line.to_string()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .whitespace_nowrap()
                            .font_family(theme.font_mono.clone())
                            .text_color(theme.text_muted)
                            .child(styled),
                    )
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(
        line: u32,
        start: u32,
        end: u32,
        query: &str,
        case_sensitive: bool,
    ) -> TextNavigationTarget {
        TextNavigationTarget {
            path: "src/lib.rs".into(),
            line,
            byte_start: start,
            byte_end: end,
            query: query.into(),
            case_sensitive,
            content_hash: "hash".into(),
            checkout_id: "checkout".into(),
        }
    }

    /// The editor selects UTF-8 byte offsets in its own text. Failure modes
    /// pinned here: treating byte columns as characters (wrong range after
    /// multibyte text), counting `\r` into a CRLF line, claiming an exact
    /// highlight after the line changed, and a deleted literal still
    /// producing a precise selection.
    #[test]
    fn located_ranges_follow_utf8_bytes_and_report_changed_text_truthfully() {
        let text = "fn größe() {}\r\nlet naïve = \"Needle\"; // 🦀 needle\nend\n";
        let line2 = "let naïve = \"Needle\"; // 🦀 needle";
        let start = line2.find("needle").unwrap() as u32;
        let upper = line2.find("Needle").unwrap() as u32;
        let cases = [
            // Exact, after multibyte characters on the same line.
            (
                target(2, start, start + 6, "needle", false),
                Some(("exact", "needle")),
            ),
            // Smart-case insensitive: the first occurrence is "Needle".
            (
                target(2, upper, upper + 6, "needle", false),
                Some(("exact", "Needle")),
            ),
            // Case-sensitive "Needle" no longer at the stale range: moved.
            (
                target(2, start, start + 6, "Needle", true),
                Some(("moved", "Needle")),
            ),
            // A range on the wrong line moves to the nearest occurrence.
            (target(1, 0, 6, "needle", false), Some(("moved", "Needle"))),
            // A literal that is gone never yields a selection.
            (target(2, 0, 4, "absent", false), None),
            // Past the end of a truncated file: clamp, no selection.
            (target(40, 0, 6, "absent", false), None),
        ];
        for (target, expected) in cases {
            let location = locate_text_match(text, &target);
            let range = location.selection();
            match expected {
                Some((kind, selected)) => {
                    assert_eq!(&text[range.clone()], selected, "{target:?}");
                    assert_eq!(
                        matches!(location, TextLocation::Exact { .. }),
                        kind == "exact",
                        "{target:?} -> {location:?}"
                    );
                }
                None => {
                    assert!(
                        matches!(location, TextLocation::Missing { .. }) && range.is_empty(),
                        "{target:?} -> {location:?}"
                    );
                }
            }
        }
        // CRLF: the first line's text excludes `\r`, so a range ending at the
        // line end is still exact.
        let crlf = "abc\r\nabc\r\n";
        assert_eq!(
            locate_text_match(crlf, &target(2, 0, 3, "abc", true)),
            TextLocation::Exact {
                range: 5..8,
                row: 1
            }
        );
    }
}
