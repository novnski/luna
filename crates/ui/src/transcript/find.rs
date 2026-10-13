//! Find in Conversation (IC-5): a chat-local Find bar over the complete
//! conversation this transcript shows. Failure contract, criteria mapping
//! and evidence commands:
//! `docs/pending-tasks/interactive-chat-surfaces/ic-5/README.md`.
//!
//! Data flow. `Transcript::rows` holds a row for every entry of the chat
//! (the doc watch replays the whole history; the virtual list only paints
//! what is visible), so Find reads complete history without an engine API.
//! Each searchable row projects to display-text segments keyed by the
//! renderer's own element discriminators ([`crate::markdown::find`]). The
//! background executor projects, case-folds and matches those segments in
//! chunks; every chunk is fenced by a query generation, so an obsolete query
//! never updates the bar. Results are kept per row id and version: streaming
//! re-matches only the rows whose content changed, and the active match is
//! an identity (row id, element, range), never a list index.
//!
//! Searchable: user prompts as displayed (mention chips as their labels),
//! assistant/system Markdown (prose, headings, lists, quotes, tables, code)
//! and run-error text. Never searched: reasoning, tool calls and results,
//! question/secret-input chips, images, Mermaid source shown as a diagram,
//! agent cards and other app surfaces.

use super::*;
use crate::button::{ButtonKind, ControlSize};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::markdown::find::{
    ElementKey, FindPaint, ProjectOptions, TextSegment, project_block, push_segment,
};
use gpui::Focusable as _;

gpui::actions!(transcript, [FindInConversation]);

/// Longest query kept; a longer paste is cut at a char boundary.
const MAX_QUERY_CHARS: usize = 256;
/// Background chunk budgets before results are handed back to the UI
/// thread: a small first chunk for first-match latency, then larger ones so
/// a 10 MiB pass needs a handful of UI-thread merges, not hundreds.
const FIRST_CHUNK_BYTES: usize = 128 * 1024;
const CHUNK_BYTES: usize = 1024 * 1024;
const CHUNK_ROWS: usize = 4_096;
/// A transcript selection seeds a new query only when it is this short.
const SEED_SELECTION_CHARS: usize = 120;
const FIND_BAR_WIDTH: f32 = 420.0;
const FIND_BAR_HEIGHT: f32 = 40.0;
/// Glyph advance (px) assumed when estimating whether a match sits past a
/// collapsed prompt's visible lines. Deliberately wide: the estimate errs
/// toward opening the fold rather than leaving the match clipped.
const FOLD_ESTIMATE_GLYPH_PX: f32 = 8.5;

/// Bind the Find shortcut app-wide unless a customizable shortcut already
/// owns the chord. Surfaces with their own Find (file editor `Input`, Board
/// search) keep it: their deeper binding or raw key handler runs first, and
/// the chat surfaces route the action only from their own focus.
pub(crate) fn bind_keys(cx: &mut gpui::App, keymap: &crate::settings::KeymapConfig) {
    use crate::settings::{ShortcutId, platform_combo};
    let combo = platform_combo("mod-f");
    let wanted = gpui::Keystroke::parse(&combo).ok();
    let taken = ShortcutId::ALL
        .iter()
        .any(|id| gpui::Keystroke::parse(&platform_combo(keymap.get(*id))).ok() == wanted);
    if wanted.is_some() && !taken {
        cx.bind_keys([gpui::KeyBinding::new(&combo, FindInConversation, None)]);
    }
}

/// The chat surfaces' Cmd+F (and menu) route: open `transcript`'s Find when
/// the keyboard is in that chat's composer or nowhere. False lets the action
/// continue, so a focused file editor, browser, terminal or Board keeps its
/// own Find.
pub(crate) fn open_find_from_composer(
    transcript: &Entity<Transcript>,
    composer: &gpui::FocusHandle,
    window: &mut Window,
    cx: &mut gpui::App,
) -> bool {
    let from_chat = window.focused(cx).is_none() || composer.contains_focused(window, cx);
    from_chat && transcript.update(cx, |transcript, cx| transcript.open_find(window, cx))
}

// ---------------------------------------------------------------------------
// Matching (pure; runs on the background executor)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FindQuery {
    text: String,
    folded: String,
    case_sensitive: bool,
}

impl FindQuery {
    /// `None` for a blank query: whitespace alone matches nothing.
    fn new(text: &str, case_sensitive: bool) -> Option<Self> {
        if text.trim().is_empty() {
            return None;
        }
        let text: String = text.chars().take(MAX_QUERY_CHARS).collect();
        let mut folded = String::with_capacity(text.len());
        fold_into(&text, &mut folded);
        Some(Self {
            text,
            folded,
            case_sensitive,
        })
    }
}

/// Unicode lowercase per character plus the full case-folding equivalences
/// `char::to_lowercase` lacks: `ß`/`ẞ` → `ss`, final sigma → `σ`, and
/// dotted capital `İ` → `i` (std yields `i̇`, which no typed `i` matches).
/// Folding is per character so every folded byte maps back to one source
/// character; offsets of a lowercased string are never reused directly.
fn push_folded(ch: char, out: &mut String) {
    match ch {
        'ß' | 'ẞ' => out.push_str("ss"),
        'ς' => out.push('σ'),
        'İ' => out.push('i'),
        _ => out.extend(ch.to_lowercase()),
    }
}

fn fold_into(text: &str, out: &mut String) {
    for ch in text.chars() {
        push_folded(ch, out);
    }
}

/// One searchable element's display text, with its case fold cached.
pub(super) struct Segment {
    element: ElementKey,
    text: Box<str>,
    /// `None` when folding leaves the text unchanged.
    folded: Option<Box<str>>,
}

impl Segment {
    fn new(source: TextSegment) -> Self {
        let mut folded = String::with_capacity(source.text.len());
        fold_into(&source.text, &mut folded);
        let folded = (folded != source.text).then(|| folded.into_boxed_str());
        Self {
            element: source.element,
            text: source.text.into_boxed_str(),
            folded,
        }
    }

    fn bytes(&self) -> usize {
        self.text.len() + self.folded.as_ref().map_or(0, |folded| folded.len())
    }
}

/// A match: the element and the byte range in its display text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ElementMatch {
    element: ElementKey,
    range: Range<usize>,
}

/// Literal, non-overlapping matches of `query` in one segment.
fn match_segment(segment: &Segment, query: &FindQuery, out: &mut Vec<ElementMatch>) {
    if query.case_sensitive {
        for (start, hit) in segment.text.match_indices(query.text.as_str()) {
            out.push(ElementMatch {
                element: segment.element,
                range: start..start + hit.len(),
            });
        }
        return;
    }
    let folded = segment.folded.as_deref().unwrap_or(&segment.text);
    let hits: Vec<Range<usize>> = folded
        .match_indices(query.folded.as_str())
        .map(|(start, hit)| start..start + hit.len())
        .collect();
    if hits.is_empty() {
        return;
    }
    let ranges = if segment.folded.is_some() {
        map_folded(&segment.text, &hits)
    } else {
        hits
    };
    let mut last_end = 0;
    for range in ranges {
        // Two folded hits inside one expanded character (`ß` → `ss`) widen
        // to the same source character; keep source ranges non-overlapping.
        if range.start < last_end {
            continue;
        }
        last_end = range.end;
        out.push(ElementMatch {
            element: segment.element,
            range,
        });
    }
}

/// Map ascending, non-overlapping folded byte ranges back to source byte
/// ranges, widened to whole source characters, in one pass over `source`.
fn map_folded(source: &str, hits: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out = Vec::with_capacity(hits.len());
    let mut folded_at = 0;
    let mut expansion = String::new();
    let mut hit = 0;
    let mut start: Option<usize> = None;
    for (offset, ch) in source.char_indices() {
        if hit == hits.len() {
            break;
        }
        expansion.clear();
        push_folded(ch, &mut expansion);
        let next = folded_at + expansion.len();
        while let Some(range) = hits.get(hit) {
            if start.is_none() {
                if range.start >= next {
                    break;
                }
                start = Some(offset);
            }
            if range.end > next {
                break;
            }
            out.extend(start.take().map(|start| start..offset + ch.len_utf8()));
            hit += 1;
        }
        folded_at = next;
    }
    out
}

// ---------------------------------------------------------------------------
// Row sources and background chunks
// ---------------------------------------------------------------------------

/// The searchable content of one row, cheap to clone and `Send`.
#[derive(Clone)]
pub(super) enum FindSource {
    Markdown {
        tree: Arc<BlockTree>,
        block_ix: usize,
    },
    User {
        text: SharedString,
        mentions: Arc<Vec<crate::composer::SentMentionSpan>>,
    },
    Error {
        message: SharedString,
    },
}

impl FindSource {
    /// `None` for rows whose content is metadata, a tool trace, reasoning,
    /// a question/secret-input chip, an image or an app/visual card.
    fn of(row: &Row) -> Option<Self> {
        // A child-creation notice paints as its agent card, not as text.
        if row.role == MessageRole::System && row.entry_id.starts_with("child-created:") {
            return None;
        }
        match &row.kind {
            RowKind::User { text, mentions, .. } => Some(Self::User {
                text: text.clone(),
                mentions: mentions.clone(),
            }),
            RowKind::Markdown { tree, block_ix } | RowKind::LiveMarkdown { tree, block_ix } => {
                Some(Self::Markdown {
                    tree: tree.clone(),
                    block_ix: *block_ix,
                })
            }
            RowKind::ErrorChip { message } => Some(Self::Error {
                message: message.clone(),
            }),
            RowKind::GeneratedImage { .. }
            | RowKind::ToolGroup { .. }
            | RowKind::AsyncQuestionCard { .. }
            | RowKind::InputChip { .. }
            | RowKind::GoalAchieved { .. }
            | RowKind::AutomationCard { .. } => None,
        }
    }

    fn estimated_bytes(&self) -> usize {
        match self {
            Self::Markdown { tree, block_ix } => {
                tree.blocks.get(*block_ix).map_or(0, |top| top.range.len())
            }
            Self::User { text, .. } => text.len(),
            Self::Error { message } => message.len(),
        }
    }

    /// The text this row paints, one segment per painted element.
    fn project(&self, workspace_root: Option<&str>) -> Vec<TextSegment> {
        let mut out = Vec::new();
        match self {
            Self::Markdown { tree, block_ix } => {
                if let Some(top) = tree.blocks.get(*block_ix) {
                    let opts = ProjectOptions {
                        workspace_root,
                        diagrams: true,
                    };
                    project_block(&top.block, *block_ix, opts, &mut out);
                }
            }
            Self::User { text, mentions } => {
                // The bubble paints the chip-decorated text; ranges index it.
                let visible = crate::composer::sent_chip_visual(text, mentions)
                    .map_or_else(|| text.to_string(), |(visible, _)| visible);
                push_segment(&mut out, ElementKey::text(0), visible);
            }
            Self::Error { message } => {
                push_segment(&mut out, ElementKey::text(0), message.to_string())
            }
        }
        out
    }
}

enum WorkInput {
    Cached(Arc<[Segment]>),
    Project(FindSource),
}

struct FindWork {
    row_id: SharedString,
    version: u64,
    bytes: usize,
    input: WorkInput,
}

struct RowResult {
    row_id: SharedString,
    version: u64,
    projected: Option<Arc<[Segment]>>,
    matches: Arc<[ElementMatch]>,
}

fn run_chunk(
    work: Vec<FindWork>,
    query: &FindQuery,
    workspace_root: Option<&str>,
) -> Vec<RowResult> {
    work.into_iter()
        .map(|item| {
            let (segments, projected) = match item.input {
                WorkInput::Cached(segments) => (segments, None),
                WorkInput::Project(source) => {
                    let segments: Arc<[Segment]> = source
                        .project(workspace_root)
                        .into_iter()
                        .map(Segment::new)
                        .collect();
                    (segments.clone(), Some(segments))
                }
            };
            let mut matches = Vec::new();
            for segment in segments.iter() {
                match_segment(segment, query, &mut matches);
            }
            RowResult {
                row_id: item.row_id,
                version: item.version,
                projected,
                matches: matches.into(),
            }
        })
        .collect()
}

/// Take the next chunk of work, bounded by `budget` bytes and [`CHUNK_ROWS`].
fn next_chunk(work: &mut std::vec::IntoIter<FindWork>, budget: usize) -> Vec<FindWork> {
    let mut chunk = Vec::new();
    let mut bytes = 0;
    while chunk.len() < CHUNK_ROWS && bytes < budget {
        let Some(item) = work.next() else {
            break;
        };
        bytes += item.bytes;
        chunk.push(item);
    }
    chunk
}

fn spawn_chunk(
    cx: &gpui::AsyncApp,
    chunk: Vec<FindWork>,
    query: FindQuery,
    workspace_root: Option<SharedString>,
) -> Task<Vec<RowResult>> {
    cx.background_executor()
        .spawn(async move { run_chunk(chunk, &query, workspace_root.as_deref()) })
}

fn no_matches() -> Arc<[ElementMatch]> {
    Vec::<ElementMatch>::new().into()
}

fn ms_since(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 100_000.0).round() / 100.0
}

/// The selection seeds a new query only when it is one short line.
fn selection_seed() -> Option<String> {
    let text = crate::markdown::selection::selected_text()?;
    let text = text.trim();
    (!text.is_empty() && !text.contains('\n') && text.chars().count() <= SEED_SELECTION_CHARS)
        .then(|| text.to_owned())
}

/// Whether a match ending at `end` may lie past a collapsed prompt's
/// visible lines, given a conservative characters-per-line estimate.
fn match_may_be_folded(text: &str, end: usize, chars_per_line: usize) -> bool {
    let prefix = text.get(..end.min(text.len())).unwrap_or(text);
    let lines: usize = prefix
        .split('\n')
        .map(|line| line.chars().count().div_ceil(chars_per_line.max(1)).max(1))
        .sum();
    lines >= USER_COLLAPSED_LINES
}

// ---------------------------------------------------------------------------
// Chat-local Find state
// ---------------------------------------------------------------------------

/// The active match by identity. `row_hint` is only where it last was, used
/// to choose the next surviving match after its row disappears.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FindAnchor {
    row_id: SharedString,
    element: ElementKey,
    range: Range<usize>,
    row_hint: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MatchPos {
    row_ix: usize,
    match_ix: usize,
}

/// Measurements for the evidence record (P01); observation only.
#[derive(Clone, Debug, Default)]
pub(super) struct FindStats {
    started: Option<Instant>,
    first_match_ms: Option<f64>,
    complete_ms: Option<f64>,
    chunks: usize,
    max_apply_ms: f64,
    rows: usize,
    projected_rows: usize,
}

#[derive(Default)]
pub(super) struct FindState {
    open: bool,
    input: Option<Entity<ComposerInput>>,
    _input_events: Option<Subscription>,
    case_sensitive: bool,
    query: Option<FindQuery>,
    /// Bumped by every change that makes in-flight results obsolete.
    generation: u64,
    task: Option<Task<()>>,
    searching: bool,
    /// Rows changed while a job ran; one incremental pass follows it.
    dirty: bool,
    /// The chat's authoritative history has arrived (else Partial history).
    history_complete: bool,
    /// Display segments per row, reused across queries (version-keyed).
    segments: HashMap<SharedString, (u64, Arc<[Segment]>)>,
    /// Matches of the current query per row (version-keyed).
    matches: HashMap<SharedString, (u64, Arc<[ElementMatch]>)>,
    total: usize,
    ordered: Option<Rc<[MatchPos]>>,
    active: Option<FindAnchor>,
    /// The running full pass picks (and reveals) the first match it finds.
    pick_first: bool,
    return_focus: Option<gpui::FocusHandle>,
    /// The bar claims keyboard focus (and selects its query) in the frame
    /// that mounts it.
    focus_pending: bool,
    opened_viewport: Option<SavedViewport>,
    /// Find moved the viewport, and the user has not scrolled since.
    moved_viewport: bool,
    user_scrolled: bool,
    /// User-bubble folds Find opened: row → (fold epoch it set, previous).
    search_folds: HashMap<SharedString, (usize, Option<bool>)>,
    active_rect: Rc<Cell<Option<Bounds<Pixels>>>>,
    reveal_frames: u8,
    reveal_scheduled: bool,
    chat: Option<String>,
    /// Memory-only: the last query per chat, offered on reopening.
    remembered: HashMap<String, (String, bool)>,
    stats: FindStats,
}

impl FindState {
    /// Note a wheel/touch scroll: the viewport is the user's again.
    pub(super) fn note_user_scroll(&mut self) {
        if self.open {
            self.user_scrolled = true;
        }
    }
}

impl Transcript {
    /// Open the Find bar for this view's chat, or refocus it. Returns false
    /// when there is no conversation here, so the caller can let the action
    /// continue to another surface.
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(chat) = self.chat_id.clone() else {
            return false;
        };
        let input = self.find_input(cx);
        let focus = input.read(cx).focus_handle(cx);
        if !self.find.open {
            self.find.open = true;
            self.find.chat = Some(chat.clone());
            self.find.return_focus = window.focused(cx).filter(|focused| focused != &focus);
            let distance = if self.pinned {
                0.0
            } else {
                self.distance_from_bottom()
            };
            self.find.opened_viewport = SavedViewport::capture(
                &self.rows,
                self.list.logical_scroll_top(),
                self.pinned,
                distance,
                self.own_turn.as_ref(),
            );
            self.find.moved_viewport = false;
            self.find.user_scrolled = false;
            let seed = selection_seed()
                .map(|text| (text, self.find.case_sensitive))
                .or_else(|| self.find.remembered.get(&chat).cloned());
            let text = match seed {
                Some((text, case_sensitive)) => {
                    self.find.case_sensitive = case_sensitive;
                    text
                }
                None => String::new(),
            };
            // Emits `Edited`, which (re)starts the search for this text.
            input.update(cx, |input, cx| input.set_text(text, cx));
        }
        window.focus(&focus, cx);
        // The shell recovers focus against the last *rendered* frame after
        // every render, and GPUI redraws a dirty window before dispatching a
        // key, so that recovery runs right after this shortcut and, seeing
        // the bar not yet mounted, hands the keyboard back to the composer.
        // Claim it again once those deferred recoveries have run (same
        // update), and once more as the bar mounts (`render_find_bar`).
        let this = cx.weak_entity();
        let claim = focus.clone();
        window.defer(cx, move |window, cx| {
            if this.upgrade().is_some_and(|this| this.read(cx).find.open) {
                window.focus(&claim, cx);
            }
        });
        self.find.focus_pending = true;
        cx.notify();
        true
    }

    /// Close the bar: restore what Find itself changed (folds, viewport)
    /// unless the user changed it since, and return focus only when it is
    /// still in the bar.
    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_find(Some(window), true, cx);
    }

    /// A chat switch ends Find without restoring the departing viewport.
    pub(super) fn detach_find(&mut self, cx: &mut Context<Self>) {
        self.end_find(None, false, cx);
    }

    fn end_find(&mut self, window: Option<&mut Window>, restore: bool, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        if let Some(chat) = self.find.chat.take()
            && let Some(input) = self.find.input.as_ref()
        {
            let text = input.read(cx).text().to_owned();
            if text.trim().is_empty() {
                self.find.remembered.remove(&chat);
            } else {
                self.find
                    .remembered
                    .insert(chat, (text, self.find.case_sensitive));
            }
        }
        self.find.open = false;
        self.find.generation = self.find.generation.wrapping_add(1);
        self.find.task = None;
        self.find.searching = false;
        self.find.dirty = false;
        self.find.query = None;
        self.find.matches.clear();
        self.find.segments.clear();
        self.find.total = 0;
        self.find.ordered = None;
        self.find.active = None;
        self.find.active_rect.set(None);
        self.find.reveal_frames = 0;
        self.find.focus_pending = false;
        let saved = self.find.opened_viewport.take();
        if restore {
            self.restore_search_folds(None);
            if self.find.moved_viewport
                && !self.find.user_scrolled
                && let Some(saved) = saved
            {
                self.restore_find_viewport(saved);
            }
        } else {
            self.find.search_folds.clear();
        }
        self.find.moved_viewport = false;
        self.find.user_scrolled = false;
        let back = self.find.return_focus.take();
        if let Some(window) = window
            && let Some(input) = self.find.input.as_ref()
            && input.read(cx).focus_handle(cx).contains_focused(window, cx)
            && let Some(back) = back
        {
            window.focus(&back, cx);
        }
        cx.notify();
    }

    fn restore_find_viewport(&mut self, saved: SavedViewport) {
        match saved {
            SavedViewport::FollowTail => {
                self.begin_scroll_navigation();
                self.pinned = true;
                self.list.scroll_to_end();
                self.last_scroll_distance = 0.0;
                self.show_jump_button = false;
                self.wake_spring();
            }
            anchored => {
                if let Some(restored) = anchored.resolve(&self.rows, true) {
                    self.begin_scroll_navigation();
                    self.list.scroll_to(restored.offset);
                    self.last_scroll_distance = restored.distance_from_bottom;
                    self.show_jump_button =
                        restored.distance_from_bottom > SCROLL_BUTTON_THRESHOLD_PX;
                    self.viewport_finalize_pending = true;
                }
            }
        }
    }

    fn find_input(&mut self, cx: &mut Context<Self>) -> Entity<ComposerInput> {
        if let Some(input) = &self.find.input {
            return input.clone();
        }
        let input = cx.new(|cx| {
            ComposerInput::with_context("Find in conversation", "PaletteSearch", cx)
                .with_text_metrics(13.0, 18.0)
                .with_single_line()
                .with_accessibility_role(gpui::Role::SearchInput)
        });
        let events = cx.subscribe(
            &input,
            |this: &mut Self, _, event: &ComposerInputEvent, cx| {
                if matches!(event, ComposerInputEvent::Edited) {
                    this.find_query_changed(false, cx);
                }
            },
        );
        self.find.input = Some(input.clone());
        self.find._input_events = Some(events);
        input
    }

    fn focus_find_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = &self.find.input {
            window.focus(&input.read(cx).focus_handle(cx), cx);
        }
    }

    fn find_query_changed(&mut self, force: bool, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        let text = self
            .find
            .input
            .as_ref()
            .map(|input| input.read(cx).text().to_owned())
            .unwrap_or_default();
        let query = FindQuery::new(&text, self.find.case_sensitive);
        if !force && query == self.find.query {
            return;
        }
        self.find.query = query;
        self.restart_find(cx);
    }

    fn toggle_find_case(&mut self, cx: &mut Context<Self>) {
        self.find.case_sensitive = !self.find.case_sensitive;
        self.find_query_changed(true, cx);
    }

    /// Whether the chat's authoritative history has arrived. Before it does,
    /// a result is Partial history, never a complete zero.
    fn find_history_complete(&self, cx: &gpui::App) -> bool {
        let state = self.state.read(cx);
        match &self.doc_override {
            None => state.transcript_replayed,
            Some(doc_id) if self.pane_chat => {
                crate::chat_binding::ChatBinding::Chat(doc_id.clone()).transcript_replayed(state)
            }
            Some(doc_id) => {
                state.transcript_baseline(doc_id).is_some()
                    || !state.sub_transcript(doc_id).is_empty()
            }
        }
    }

    /// A new query (or case mode): drop every result and search all rows,
    /// starting at the viewport so the first match found is the first one
    /// at or after what the reader is looking at.
    fn restart_find(&mut self, cx: &mut Context<Self>) {
        self.find.generation = self.find.generation.wrapping_add(1);
        self.find.task = None;
        self.find.searching = false;
        self.find.dirty = false;
        self.find.matches.clear();
        self.find.total = 0;
        self.find.ordered = None;
        self.find.active = None;
        self.find.active_rect.set(None);
        self.find.history_complete = self.find_history_complete(cx);
        self.find.stats = FindStats {
            started: Some(Instant::now()),
            rows: self.rows.len(),
            ..Default::default()
        };
        self.find.pick_first = self.find.query.is_some();
        if self.find.query.is_some() {
            let start = self.list.logical_scroll_top().item_ix.min(self.rows.len());
            let order: Vec<usize> = (start..self.rows.len()).chain(0..start).collect();
            self.run_find_job(order, true, cx);
        }
        cx.notify();
    }

    /// After every row sync: re-match only rows whose content changed,
    /// forget removed rows, and restart once history becomes complete.
    pub(super) fn refresh_find(&mut self, cx: &mut Context<Self>) {
        if !self.find.open {
            return;
        }
        let complete = self.find_history_complete(cx);
        let became_complete = complete && !self.find.history_complete;
        if complete != self.find.history_complete {
            self.find.history_complete = complete;
            cx.notify();
        }
        if self.find.query.is_none() {
            return;
        }
        if became_complete && self.find.active.is_none() {
            // The opening replay landed under a query: search it as a new
            // pass so its first match is chosen and revealed.
            self.restart_find(cx);
            return;
        }
        self.find.ordered = None;
        if self.find.searching {
            self.find.dirty = true;
            return;
        }
        let mut changed = Vec::new();
        let mut indexed = 0;
        for (ix, row) in self.rows.iter().enumerate() {
            match self.find.matches.get(&row.id) {
                Some((version, _)) => {
                    indexed += 1;
                    if *version != row.version {
                        changed.push(ix);
                    }
                }
                None => changed.push(ix),
            }
        }
        if indexed != self.find.matches.len() {
            let live: HashSet<&SharedString> = self.rows.iter().map(|row| &row.id).collect();
            let mut removed = 0;
            self.find.matches.retain(|id, (_, matches)| {
                let keep = live.contains(id);
                if !keep {
                    removed += matches.len();
                }
                keep
            });
            self.find.segments.retain(|id, _| live.contains(id));
            self.find.total -= removed;
            self.resolve_find_active();
        }
        if changed.is_empty() {
            self.resolve_find_active();
        } else {
            self.run_find_job(changed, false, cx);
        }
        cx.notify();
    }

    fn run_find_job(&mut self, order: Vec<usize>, full: bool, cx: &mut Context<Self>) {
        let Some(query) = self.find.query.clone() else {
            return;
        };
        let mut work = Vec::new();
        for ix in order {
            let Some(row) = self.rows.get(ix) else {
                continue;
            };
            let Some(source) = FindSource::of(row) else {
                if let Some((_, old)) = self
                    .find
                    .matches
                    .insert(row.id.clone(), (row.version, no_matches()))
                {
                    self.find.total -= old.len();
                }
                continue;
            };
            let input = match self.find.segments.get(&row.id) {
                Some((version, segments)) if *version == row.version => {
                    WorkInput::Cached(segments.clone())
                }
                _ => WorkInput::Project(source),
            };
            let bytes = match &input {
                WorkInput::Cached(segments) => segments.iter().map(Segment::bytes).sum(),
                WorkInput::Project(source) => source.estimated_bytes(),
            };
            work.push(FindWork {
                row_id: row.id.clone(),
                version: row.version,
                bytes,
                input,
            });
        }
        self.find.ordered = None;
        if work.is_empty() {
            self.finish_find_job(full, cx);
            return;
        }
        let generation = self.find.generation;
        let workspace_root = self.row_workspace_root(cx);
        self.find.searching = true;
        self.find.task = Some(cx.spawn(async move |this, cx| {
            let mut work = work.into_iter();
            // Pipelined: the next chunk runs on the background executor while
            // the UI thread merges the previous one. A small first chunk gets
            // the first match out quickly; larger later chunks keep the
            // number of UI-thread merges (and re-renders) per pass small.
            let first = next_chunk(&mut work, FIRST_CHUNK_BYTES);
            let mut pending = Some(spawn_chunk(
                cx,
                first,
                query.clone(),
                workspace_root.clone(),
            ));
            while let Some(running) = pending.take() {
                let results = running.await;
                let rest = next_chunk(&mut work, CHUNK_BYTES);
                let last = rest.is_empty();
                if !last {
                    pending = Some(spawn_chunk(cx, rest, query.clone(), workspace_root.clone()));
                }
                let proceed = this
                    .update(cx, |this, cx| {
                        this.apply_find_chunk(generation, full, results, last, cx)
                    })
                    .unwrap_or(false);
                if !proceed {
                    // Dropping the queued chunk cancels it.
                    break;
                }
            }
        }));
    }

    /// Merge one chunk. False (and nothing applied) when its query
    /// generation is obsolete or the bar closed.
    fn apply_find_chunk(
        &mut self,
        generation: u64,
        full: bool,
        results: Vec<RowResult>,
        last: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if generation != self.find.generation || !self.find.open {
            return false;
        }
        let started = Instant::now();
        let mut first: Option<(SharedString, ElementMatch)> = None;
        for result in results {
            if let Some(segments) = result.projected {
                self.find.stats.projected_rows += 1;
                self.find
                    .segments
                    .insert(result.row_id.clone(), (result.version, segments));
            }
            if first.is_none()
                && let Some(found) = result.matches.first()
            {
                first = Some((result.row_id.clone(), found.clone()));
            }
            self.find.total += result.matches.len();
            if let Some((_, old)) = self
                .find
                .matches
                .insert(result.row_id, (result.version, result.matches))
            {
                self.find.total -= old.len();
            }
        }
        self.find.ordered = None;
        self.find.stats.chunks += 1;
        if self.find.pick_first
            && let Some((row_id, found)) = first
        {
            self.find.pick_first = false;
            self.find.stats.first_match_ms = self.find.stats.started.map(ms_since);
            self.set_find_active(row_id, found, true, cx);
        } else if !full {
            self.resolve_find_active();
        }
        if last {
            self.finish_find_job(full, cx);
        }
        let elapsed = ms_since(started);
        if elapsed > self.find.stats.max_apply_ms {
            self.find.stats.max_apply_ms = elapsed;
        }
        cx.notify();
        true
    }

    fn finish_find_job(&mut self, full: bool, cx: &mut Context<Self>) {
        self.find.searching = false;
        if full {
            self.find.pick_first = false;
            self.find.stats.complete_ms = self.find.stats.started.map(ms_since);
            let stats = &self.find.stats;
            tracing::debug!(
                rows = stats.rows,
                projected_rows = stats.projected_rows,
                chunks = stats.chunks,
                matches = self.find.total,
                first_match_ms = ?stats.first_match_ms,
                complete_ms = ?stats.complete_ms,
                max_apply_ms = stats.max_apply_ms,
                "conversation find pass complete"
            );
        }
        self.resolve_find_active();
        if std::mem::take(&mut self.find.dirty) {
            self.refresh_find(cx);
        }
    }

    fn set_find_active(
        &mut self,
        row_id: SharedString,
        found: ElementMatch,
        reveal: bool,
        cx: &mut Context<Self>,
    ) {
        let row_hint = self
            .rows
            .iter()
            .position(|row| row.id == row_id)
            .unwrap_or(0);
        self.find.active = Some(FindAnchor {
            row_id,
            element: found.element,
            range: found.range,
            row_hint,
        });
        self.find.active_rect.set(None);
        if reveal {
            self.reveal_find_active(cx);
        }
    }

    /// Keep the active match by identity; when it is gone (rollback, edit),
    /// take the next surviving match in reading order, wrapping.
    fn resolve_find_active(&mut self) {
        let Some(anchor) = self.find.active.clone() else {
            return;
        };
        let row_ix = self.find_row_ix(&anchor);
        if let Some(row_ix) = row_ix
            && let Some((_, matches)) = self.find.matches.get(&anchor.row_id)
        {
            let kept = matches
                .iter()
                .find(|found| found.element == anchor.element && found.range == anchor.range)
                .or_else(|| {
                    matches.iter().find(|found| {
                        found.element == anchor.element && found.range.start >= anchor.range.start
                    })
                })
                .cloned();
            if let Some(found) = kept {
                if found.range != anchor.range {
                    self.find.active_rect.set(None);
                }
                self.find.active = Some(FindAnchor {
                    row_id: anchor.row_id,
                    element: found.element,
                    range: found.range,
                    row_hint: row_ix,
                });
                return;
            }
        }
        let start = row_ix
            .map_or(anchor.row_hint, |ix| ix + 1)
            .min(self.rows.len());
        let next = (start..self.rows.len()).chain(0..start).find_map(|ix| {
            let row = &self.rows[ix];
            self.find
                .matches
                .get(&row.id)
                .and_then(|(_, matches)| matches.first())
                .map(|found| (ix, row.id.clone(), found.clone()))
        });
        self.find.active = next.map(|(row_hint, row_id, found)| FindAnchor {
            row_id,
            element: found.element,
            range: found.range,
            row_hint,
        });
        self.find.active_rect.set(None);
    }

    fn find_row_ix(&self, anchor: &FindAnchor) -> Option<usize> {
        self.rows
            .get(anchor.row_hint)
            .filter(|row| row.id == anchor.row_id)
            .map(|_| anchor.row_hint)
            .or_else(|| self.rows.iter().position(|row| row.id == anchor.row_id))
    }

    /// Every match in reading order, rebuilt lazily after any change.
    fn find_ordered(&mut self) -> Rc<[MatchPos]> {
        if let Some(ordered) = &self.find.ordered {
            return ordered.clone();
        }
        let mut out = Vec::with_capacity(self.find.total);
        for (row_ix, row) in self.rows.iter().enumerate() {
            if let Some((_, matches)) = self.find.matches.get(&row.id) {
                out.extend((0..matches.len()).map(|match_ix| MatchPos { row_ix, match_ix }));
            }
        }
        let ordered: Rc<[MatchPos]> = out.into();
        self.find.ordered = Some(ordered.clone());
        ordered
    }

    fn find_active_position(&self, ordered: &[MatchPos]) -> Option<usize> {
        let anchor = self.find.active.as_ref()?;
        let row_ix = self.find_row_ix(anchor)?;
        let (_, matches) = self.find.matches.get(&anchor.row_id)?;
        let match_ix = matches
            .iter()
            .position(|found| found.element == anchor.element && found.range == anchor.range)?;
        ordered.binary_search(&MatchPos { row_ix, match_ix }).ok()
    }

    /// Next/previous match, wrapping. Without an active match, the first
    /// match at or after the viewport (or the last before it).
    fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let ordered = self.find_ordered();
        let len = ordered.len();
        if len == 0 {
            return;
        }
        let target = match self.find_active_position(&ordered) {
            Some(position) if forward => (position + 1) % len,
            Some(position) => (position + len - 1) % len,
            None => {
                let top = self.list.logical_scroll_top().item_ix;
                let after = ordered.iter().position(|found| found.row_ix >= top);
                if forward {
                    after.unwrap_or(0)
                } else {
                    after.map_or(len - 1, |ix| (ix + len - 1) % len)
                }
            }
        };
        let position = ordered[target];
        let Some(row) = self.rows.get(position.row_ix) else {
            return;
        };
        let row_id = row.id.clone();
        let Some(found) = self
            .find
            .matches
            .get(&row_id)
            .and_then(|(_, matches)| matches.get(position.match_ix))
            .cloned()
        else {
            return;
        };
        self.set_find_active(row_id, found, true, cx);
        cx.notify();
    }

    fn find_bar_top(&self) -> f32 {
        if self.doc_override.is_some() {
            8.0
        } else {
            // Below the shell's chrome AND its top fade band, which would
            // otherwise wash the bar out.
            self.top_chrome + Theme::TRANSCRIPT_FADE_BAND + 6.0
        }
    }

    /// Rows revealed by Find land below the bar.
    fn find_reveal_inset(&self) -> f32 {
        self.find_bar_top() + FIND_BAR_HEIGHT + 12.0
    }

    fn find_row_visible(&self, row_ix: usize) -> bool {
        let Some(bounds) = self.list.bounds_for_item(row_ix) else {
            return false;
        };
        let viewport = self.list.viewport_bounds();
        f32::from(bounds.top()) >= f32::from(viewport.top()) + self.find_reveal_inset()
            && f32::from(bounds.bottom())
                <= f32::from(viewport.bottom()) - self.bottom_clearance - 16.0
    }

    /// Bring the active match into view by row identity: open only the fold
    /// it needs, scroll its row below the bar, then correct against the
    /// painted match rect once layout has measured it.
    fn reveal_find_active(&mut self, cx: &mut Context<Self>) {
        let Some(anchor) = self.find.active.clone() else {
            return;
        };
        let Some(row_ix) = self.find_row_ix(&anchor) else {
            return;
        };
        self.restore_search_folds(Some(&anchor.row_id));
        self.open_search_fold(row_ix, &anchor);
        if !self.find_row_visible(row_ix) {
            self.begin_scroll_navigation();
            self.list.scroll_to(ListOffset {
                item_ix: row_ix,
                offset_in_item: px(-self.find_reveal_inset()),
            });
            self.last_scroll_distance = self.distance_from_bottom();
            self.show_jump_button = self.last_scroll_distance > SCROLL_BUTTON_THRESHOLD_PX;
            self.find.moved_viewport = true;
        }
        self.find.user_scrolled = false;
        self.find.active_rect.set(None);
        self.find.reveal_frames = 3;
        cx.notify();
    }

    /// Schedule the post-paint reveal correction (called from `render`).
    pub(super) fn schedule_find_reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.reveal_frames == 0 || self.find.reveal_scheduled {
            return;
        }
        self.find.reveal_scheduled = true;
        let entity = cx.weak_entity();
        window.on_next_frame(move |_, cx| {
            entity
                .update(cx, |this: &mut Transcript, cx| {
                    this.find.reveal_scheduled = false;
                    this.correct_find_reveal(cx);
                })
                .ok();
        });
    }

    /// A row taller than the viewport, or one revealed under the bar, puts
    /// its match out of sight: scroll by the painted rect's overshoot.
    fn correct_find_reveal(&mut self, cx: &mut Context<Self>) {
        if !self.find.open || self.find.reveal_frames == 0 {
            self.find.reveal_frames = 0;
            return;
        }
        self.find.reveal_frames -= 1;
        let Some(rect) = self.find.active_rect.get() else {
            if self.find.reveal_frames > 0 {
                cx.notify();
            }
            return;
        };
        let viewport = self.list.viewport_bounds();
        let top = f32::from(viewport.top()) + self.find_reveal_inset();
        let bottom = f32::from(viewport.bottom()) - self.bottom_clearance - 24.0;
        let (rect_top, rect_bottom) = (f32::from(rect.top()), f32::from(rect.bottom()));
        let delta = if rect_top < top {
            rect_top - top
        } else if rect_bottom > bottom {
            (rect_bottom - bottom).min(rect_top - top)
        } else {
            0.0
        };
        if delta.abs() > 0.5 {
            self.begin_scroll_navigation();
            self.list.scroll_by(px(delta));
            self.last_scroll_distance = self.distance_from_bottom();
            self.show_jump_button = self.last_scroll_distance > SCROLL_BUTTON_THRESHOLD_PX;
            self.find.moved_viewport = true;
            self.find.active_rect.set(None);
            cx.notify();
        } else {
            self.find.reveal_frames = 0;
        }
    }

    /// Expand a collapsed prompt only when the match may sit past its
    /// visible lines; remember the fold so closing can restore it.
    fn open_search_fold(&mut self, row_ix: usize, anchor: &FindAnchor) {
        let Some(row) = self.rows.get(row_ix) else {
            return;
        };
        if !matches!(row.kind, RowKind::User { .. }) {
            return;
        }
        if self
            .user_folds
            .get(&row.id)
            .is_some_and(|fold| fold.open.unwrap_or(false))
        {
            return;
        }
        let Some((_, segments)) = self.find.segments.get(&row.id) else {
            return;
        };
        let Some(segment) = segments.first() else {
            return;
        };
        let chars_per_line =
            ((self.content_width * 0.8 - 32.0) / FOLD_ESTIMATE_GLYPH_PX).max(12.0) as usize;
        if !match_may_be_folded(&segment.text, anchor.range.end, chars_per_line) {
            return;
        }
        let row_id = row.id.clone();
        let fold = self.user_folds.entry(row_id.clone()).or_default();
        let previous = fold.open;
        fold.open = Some(true);
        fold.epoch += 1;
        fold.toggled_at = None;
        fold.disclosure_at = None;
        let epoch = fold.epoch;
        self.find.search_folds.insert(row_id, (epoch, previous));
        self.list.remeasure_items(row_ix..row_ix + 1);
    }

    /// Put back folds Find opened, except `keep`'s, and only while their
    /// epoch shows the user has not toggled them since.
    fn restore_search_folds(&mut self, keep: Option<&SharedString>) {
        let folds = std::mem::take(&mut self.find.search_folds);
        for (row_id, (epoch, previous)) in folds {
            if keep == Some(&row_id) {
                self.find.search_folds.insert(row_id, (epoch, previous));
                continue;
            }
            let Some(fold) = self.user_folds.get_mut(&row_id) else {
                continue;
            };
            if fold.epoch != epoch {
                continue;
            }
            fold.open = previous;
            fold.epoch += 1;
            fold.toggled_at = None;
            if let Some(ix) = self.rows.iter().position(|row| row.id == row_id) {
                self.list.remeasure_items(ix..ix + 1);
            }
        }
    }

    /// This row's highlights, with the active match marked.
    pub(super) fn find_paint_for(
        &self,
        row_id: &SharedString,
        theme: &Theme,
    ) -> Option<Rc<FindPaint>> {
        if !self.find.open {
            return None;
        }
        let (_, matches) = self.find.matches.get(row_id)?;
        if matches.is_empty() {
            return None;
        }
        let active = self
            .find
            .active
            .as_ref()
            .filter(|anchor| &anchor.row_id == row_id);
        let mut paint = FindPaint::new(theme, self.find.active_rect.clone());
        for found in matches.iter() {
            let is_active = active.is_some_and(|anchor| {
                anchor.element == found.element && anchor.range == found.range
            });
            paint.push(found.element, found.range.clone(), is_active);
        }
        Some(Rc::new(paint))
    }

    /// The bar's status text: position and total, progress, or coverage.
    fn find_status(&mut self) -> SharedString {
        if self.find.query.is_none() {
            return SharedString::default();
        }
        let total = self.find.total;
        let position = if total > 0 {
            let ordered = self.find_ordered();
            self.find_active_position(&ordered)
        } else {
            None
        };
        let count = match (position, total) {
            (_, 0) => None,
            (Some(position), total) => Some(format!("{} of {total}", position + 1)),
            (None, 1) => Some("1 match".to_string()),
            (None, total) => Some(format!("{total} matches")),
        };
        let text = if self.find.searching {
            count.map_or_else(|| "Searching…".to_string(), |count| format!("{count}…"))
        } else if !self.find.history_complete {
            count.map_or_else(
                || "Partial history".to_string(),
                |count| format!("{count} · Partial history"),
            )
        } else {
            count.unwrap_or_else(|| "No results".to_string())
        };
        text.into()
    }

    pub(super) fn render_find_bar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.find.open {
            return None;
        }
        let input = self.find.input.clone()?;
        if std::mem::take(&mut self.find.focus_pending) {
            // Focus set while rendering the frame that mounts the bar is in
            // that frame's focus path, so no recovery can undo it.
            let focus = input.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
            // Select the query once it is mounted, so typing replaces a
            // reopened query; never over text typed in the meantime.
            let seeded = input.read(cx).text().to_owned();
            let select = input.clone();
            window.on_next_frame(move |window, cx| {
                if focus.is_focused(window) && select.read(cx).text() == seeded {
                    window.dispatch_action(Box::new(crate::composer::SelectAll), cx);
                }
            });
        }
        let theme = Theme::of(cx).clone();
        let status = self.find_status();
        let has_matches = self.find.total > 0;
        let case_sensitive = self.find.case_sensitive;
        let bar = crate::popover::popover_card(&theme)
            .w(crate::typography::ui_rems(FIND_BAR_WIDTH))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .px(px(8.0))
            .py(px(4.0))
            .child(
                crate::icons::icon(crate::icons::MAGNIFER)
                    .size(crate::typography::ui_rems(14.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .child(div().flex_1().min_w_0().child(input))
            .child(
                div()
                    .id("conversation-find-status")
                    .flex_none()
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.content_muted)
                    .child(status),
            )
            .child(
                crate::button::button(
                    "conversation-find-case",
                    if case_sensitive {
                        ButtonKind::Secondary
                    } else {
                        ButtonKind::Ghost
                    },
                    ControlSize::Sm,
                    true,
                    &theme,
                )
                .aria_label(if case_sensitive {
                    "Match case, on"
                } else {
                    "Match case, off"
                })
                .tooltip(crate::popover::text_tooltip("Match case"))
                .child("Aa")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_find_case(cx);
                    this.focus_find_input(window, cx);
                })),
            )
            .child(
                crate::button::icon_button(
                    "conversation-find-previous",
                    crate::icons::ALT_ARROW_UP,
                    ControlSize::Sm,
                    has_matches,
                    &theme,
                )
                .aria_label("Previous match")
                .tooltip(crate::popover::text_tooltip("Previous match (Shift+Enter)"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.find_step(false, cx);
                    this.focus_find_input(window, cx);
                })),
            )
            .child(
                crate::button::icon_button(
                    "conversation-find-next",
                    crate::icons::ALT_ARROW_DOWN,
                    ControlSize::Sm,
                    has_matches,
                    &theme,
                )
                .aria_label("Next match")
                .tooltip(crate::popover::text_tooltip("Next match (Enter)"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.find_step(true, cx);
                    this.focus_find_input(window, cx);
                })),
            )
            .child(
                crate::button::icon_button(
                    "conversation-find-close",
                    crate::icons::CLOSE,
                    ControlSize::Sm,
                    true,
                    &theme,
                )
                .aria_label("Close Find")
                .tooltip(crate::popover::text_tooltip("Close (Esc)"))
                .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            );
        Some(
            div()
                .id("conversation-find")
                .key_context("ConversationFind")
                .absolute()
                .top(px(self.find_bar_top()))
                .right(px(16.0))
                .occlude()
                // Keep clicks on the bar from starting a transcript selection.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    let keystroke = &event.keystroke;
                    let modifiers = keystroke.modifiers;
                    if modifiers.platform || modifiers.control || modifiers.alt {
                        return;
                    }
                    match keystroke.key.as_str() {
                        "enter" => {
                            this.find_step(!modifiers.shift, cx);
                            cx.stop_propagation();
                        }
                        "escape" => {
                            this.close_find(window, cx);
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                }))
                .child(bar)
                .into_any_element(),
        )
    }
}

/// Native fixture evidence (IC-5 P01 and captures); observation only.
#[cfg(feature = "appshots-fixture")]
impl Transcript {
    /// Open Find with `query` exactly as typed into the bar.
    pub fn fixture_find(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.open_find(window, cx) {
            return false;
        }
        if let Some(input) = self.find.input.clone() {
            input.update(cx, |input, cx| input.set_text(query.to_string(), cx));
        }
        true
    }

    pub fn fixture_find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.find_step(forward, cx);
    }

    pub fn fixture_close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_find(window, cx);
    }

    pub fn fixture_find_readback(&mut self) -> serde_json::Value {
        let status = self.find_status();
        let active = self.find.active.clone().map(|anchor| {
            let text = self
                .find
                .segments
                .get(&anchor.row_id)
                .and_then(|(_, segments)| {
                    segments
                        .iter()
                        .find(|segment| segment.element == anchor.element)
                })
                .and_then(|segment| segment.text.get(anchor.range.clone()))
                .map(str::to_owned);
            let entry = self
                .find_row_ix(&anchor)
                .and_then(|ix| self.rows.get(ix))
                .map(|row| row.entry_id.to_string());
            serde_json::json!({
                "rowId": anchor.row_id.to_string(),
                "entryId": entry,
                "range": [anchor.range.start, anchor.range.end],
                "text": text,
            })
        });
        let segment_bytes: usize = self
            .find
            .segments
            .values()
            .flat_map(|(_, segments)| segments.iter().map(Segment::bytes))
            .sum();
        let stats = &self.find.stats;
        serde_json::json!({
            "open": self.find.open,
            "status": status.to_string(),
            "total": self.find.total,
            "searching": self.find.searching,
            "historyComplete": self.find.history_complete,
            "active": active,
            "scrollTopItem": self.list.logical_scroll_top().item_ix,
            "pinned": self.pinned,
            "rows": self.rows.len(),
            "searchFolds": self.find.search_folds.len(),
            "stats": {
                "rows": stats.rows,
                "projectedRows": stats.projected_rows,
                "chunks": stats.chunks,
                "firstMatchMs": stats.first_match_ms,
                "completeMs": stats.complete_ms,
                "maxApplyMs": stats.max_apply_ms,
                "segmentBytes": segment_bytes,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    //! Owner tests for IC-5 S01–S04 and S06 at the real view boundary: the
    //! production `Transcript` fed by real transcript frames
    //! (`AppState::apply_transcript_frame`, the doc watch's path), the real
    //! Find bar input and key handling, and the background executor. Native
    //! pixels, Cmd+F routing between surfaces (S05) and P01 timings are the
    //! native fixture's and QA's (see the IC-5 README).
    use super::*;
    use gpui::{Focusable as _, TestApp, TestAppWindow, point};
    use luna_doc::{MessagePart, MessageStatus, SessionMessageEntry, TextAppend, TranscriptFrame};

    const CHAT: &str = "find-chat";

    struct FindRoot {
        state: Entity<AppState>,
        composer: Entity<ComposerInput>,
        transcript: Entity<Transcript>,
    }

    impl Render for FindRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().h(px(40.0)).child(self.composer.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .child(self.transcript.clone()),
                )
        }
    }

    fn entry(id: &str, role: MessageRole, parts: Vec<MessagePart>) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role,
            parts,
            created_at: 1_790_000_000_000,
            device_id: "local".into(),
            status: (role != MessageRole::User).then_some(MessageStatus::Complete),
            continuation_of: None,
        }
    }

    fn text(id: &str, text: &str) -> MessagePart {
        MessagePart::Text {
            id: id.into(),
            text: text.into(),
        }
    }

    fn user(id: &str, body: &str) -> SessionMessageEntry {
        entry(id, MessageRole::User, vec![text("p0", body)])
    }

    fn assistant(id: &str, body: &str) -> SessionMessageEntry {
        entry(id, MessageRole::Assistant, vec![text("p0", body)])
    }

    fn filler(count: usize) -> Vec<SessionMessageEntry> {
        (0..count)
            .map(|ix| {
                assistant(
                    &format!("filler-{ix:04}"),
                    &format!("Ordinary paragraph number {ix} about the weather."),
                )
            })
            .collect()
    }

    fn private_work(id: &str) -> SessionMessageEntry {
        entry(
            id,
            MessageRole::Assistant,
            vec![
                MessagePart::Reasoning {
                    id: "r0".into(),
                    text: "SECRET-REASONING-MARKER".into(),
                },
                MessagePart::Tool {
                    id: "t0".into(),
                    call: luna_proto::ToolCall::Mcp {
                        server: "fixture".into(),
                        tool: "lookup".into(),
                        input: Some(serde_json::json!({"secret": "PRIVATE-TOOL-INPUT"})),
                    },
                    is_error: false,
                    resolved: true,
                    output: Some("PRIVATE-TOOL-OUTPUT".into()),
                    diff: None,
                    output_ref: None,
                    output_bytes: None,
                    diff_ref: None,
                    diff_stats: None,
                    subagent_ref: None,
                    subagent_status: None,
                    subagent_tail: None,
                    subagent_activity: None,
                },
                text("p1", "Visible reply after private work."),
            ],
        )
    }

    struct Harness {
        _selection: std::sync::MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
        _app: TestApp,
        window: TestAppWindow<FindRoot>,
    }

    /// The app's globals and keymap (the composer's and Find's bindings).
    fn test_app(dir: &std::path::Path) -> TestApp {
        let mut app = TestApp::new();
        app.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir, cx);
            crate::composer::init(cx, crate::settings::ComposerSendBehavior::default());
            bind_keys(cx, &crate::settings::KeymapConfig::default());
        });
        app
    }

    impl Harness {
        /// `None` leaves the chat selected but its replay not yet arrived.
        fn new(entries: Option<Vec<SessionMessageEntry>>) -> Self {
            let selection = crate::markdown::selection::test_state_lock();
            let dir = tempfile::tempdir().unwrap();
            let mut app = test_app(dir.path());
            let window = app.open_window(|_, cx| {
                let state = cx.new(|_| {
                    let mut state = AppState::new();
                    state.selected_chat = Some(CHAT.into());
                    state
                });
                let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
                let composer = cx.new(|cx| ComposerInput::new("Message", cx));
                FindRoot {
                    state,
                    composer,
                    transcript,
                }
            });
            let mut harness = Self {
                _selection: selection,
                _dir: dir,
                _app: app,
                window,
            };
            if let Some(entries) = entries {
                harness.frame(TranscriptFrame::reset(&entries));
            }
            harness.window.draw();
            harness
        }

        /// Apply one doc-watch frame exactly as the selected chat's watch does.
        fn frame(&mut self, frame: TranscriptFrame) {
            self.window.update(|root, _, cx| {
                root.state.update(cx, |state, cx| {
                    state.apply_transcript_frame(frame).expect("frame applies");
                    cx.notify();
                })
            });
            self.window.draw();
        }

        fn transcript<R>(
            &mut self,
            f: impl FnOnce(&mut Transcript, &mut Window, &mut Context<Transcript>) -> R,
        ) -> R {
            self.window.update(|root, window, cx| {
                root.transcript.update(cx, |this, cx| f(this, window, cx))
            })
        }

        fn open(&mut self) {
            assert!(self.transcript(|this, window, cx| this.open_find(window, cx)));
            self.window.draw();
        }

        /// Replace the bar's text programmatically (typing is exercised once).
        fn query(&mut self, query: &str) {
            let query = query.to_owned();
            self.transcript(move |this, _, cx| {
                let input = this.find.input.clone().expect("the bar has an input");
                input.update(cx, |input, cx| input.set_text(query, cx));
            });
            self.window.draw();
        }

        /// Every match in reading order as (entry id, displayed text).
        fn hits(&mut self) -> Vec<(String, String)> {
            self.transcript(|this, _, _| {
                let mut out = Vec::new();
                for row in &this.rows {
                    let Some((_, matches)) = this.find.matches.get(&row.id) else {
                        continue;
                    };
                    for found in matches.iter() {
                        let (_, segments) = &this.find.segments[&row.id];
                        let segment = segments
                            .iter()
                            .find(|segment| segment.element == found.element)
                            .expect("a match names a projected element");
                        out.push((
                            row.entry_id.to_string(),
                            segment.text[found.range.clone()].to_string(),
                        ));
                    }
                }
                out
            })
        }

        fn active(&mut self) -> Option<(String, String)> {
            self.transcript(|this, _, _| {
                let anchor = this.find.active.clone()?;
                let row = &this.rows[this.find_row_ix(&anchor)?];
                let (_, segments) = this.find.segments.get(&row.id)?;
                let segment = segments
                    .iter()
                    .find(|segment| segment.element == anchor.element)?;
                Some((
                    row.entry_id.to_string(),
                    segment.text.get(anchor.range.clone())?.to_string(),
                ))
            })
        }

        fn status(&mut self) -> String {
            self.transcript(|this, _, _| this.find_status().to_string())
        }

        /// (row index of `entry`, its row id) in the current rows.
        fn row_of(&mut self, entry: &str) -> (usize, SharedString) {
            let entry = entry.to_owned();
            self.transcript(move |this, _, _| {
                let ix = this
                    .rows
                    .iter()
                    .position(|row| row.entry_id.as_ref() == entry)
                    .expect("entry has a row");
                (ix, this.rows[ix].id.clone())
            })
        }
    }

    fn owned(hits: &[(&str, &str)]) -> Vec<(String, String)> {
        hits.iter()
            .map(|(entry, text)| (entry.to_string(), text.to_string()))
            .collect()
    }

    /// S01, S02 (F1–F11, F13). Regression caught: searching only mounted
    /// rows, metadata or raw Markdown; a phrase split by emphasis; link
    /// destinations, reasoning, tool data, diagram source or another chat
    /// becoming searchable; folded Unicode mapping to the wrong bytes;
    /// overlapping hits; an ignored case toggle; a whitespace query matching.
    #[test]
    fn find_searches_the_complete_conversation_by_displayed_text() {
        let mut entries = vec![user(
            "oldest",
            "ZEPHYR-OLDEST-MARKER opens the conversation.",
        )];
        entries.extend(filler(240));
        entries.extend([
            assistant(
                "emphasis",
                "The **quick** brown fox; see [Release notes](https://hidden.example/token-XYZ).",
            ),
            assistant("blocks", "This is the end of alpha.\n\nOmega starts a new paragraph."),
            assistant("left", "This message ends with crossmsg-left"),
            user("right", "crossmsg-right begins this one."),
            assistant(
                "unicode",
                "\u{212A}ELVIN scale, GROẞE Straße, 🚀 launch, 検索テスト, İstanbul, ISTANBUL, ΟΔΟΣ.",
            ),
            assistant(
                "blocks2",
                concat!(
                    "Before code.\n\n",
                    "```rust\nfn main() {\n    println!(\"ic5\");\n}\n```\n\n",
                    "| Name | Value |\n| --- | --- |\n| row | CELL-MARKER |\n\n",
                    "> - quoted LIST-MARKER item\n\n",
                    "```mermaid\nflowchart LR\n  MERMAID-SOURCE-MARKER --> B\n```",
                ),
            ),
            assistant("repeat", "aaaaaa and Zulu zulu ZULU"),
            private_work("private"),
            entry(
                "failed",
                MessageRole::Assistant,
                vec![MessagePart::Error {
                    id: "e0".into(),
                    message: "Provider ERROR-MARKER: quota".into(),
                }],
            ),
        ]);
        let mut harness = Harness::new(Some(entries));
        // Another chat's unsent message lives in the same app state.
        harness.window.update(|root, _, cx| {
            root.state.update(cx, |state, cx| {
                state.push_echo("other-chat", user("other", "OTHER-CHAT-MARKER"));
                cx.notify();
            })
        });
        harness.open();

        // The oldest entry is far above the mounted rows: it is found, made
        // active and revealed by its row identity.
        harness.query("zephyr-oldest-marker");
        assert_eq!(
            harness.active(),
            Some(("oldest".into(), "ZEPHYR-OLDEST-MARKER".into()))
        );
        let (oldest_ix, _) = harness.row_of("oldest");
        let top = harness.transcript(|this, _, _| this.list.logical_scroll_top().item_ix);
        assert_eq!(top, oldest_ix, "the off-screen match is revealed");
        assert_eq!(harness.status(), "1 of 1");

        let cases: &[(&str, bool, &[(&str, &str)])] = &[
            ("quick brown", false, &[("emphasis", "quick brown")]),
            ("release notes", false, &[("emphasis", "Release notes")]),
            ("hidden.example", false, &[]),
            ("alpha. omega", false, &[]),
            ("crossmsg-left crossmsg-right", false, &[]),
            ("kelvin", false, &[("unicode", "\u{212A}ELVIN")]),
            ("große", false, &[("unicode", "GROẞE")]),
            ("strasse", false, &[("unicode", "Straße")]),
            ("🚀 launch", false, &[("unicode", "🚀 launch")]),
            ("検索テスト", false, &[("unicode", "検索テスト")]),
            (
                "istanbul",
                false,
                &[("unicode", "İstanbul"), ("unicode", "ISTANBUL")],
            ),
            ("οδος", false, &[("unicode", "ΟΔΟΣ")]),
            (
                "println!(\"ic5\")",
                false,
                &[("blocks2", "println!(\"ic5\")")],
            ),
            ("cell-marker", false, &[("blocks2", "CELL-MARKER")]),
            ("list-marker", false, &[("blocks2", "LIST-MARKER")]),
            ("aaa", false, &[("repeat", "aaa"), ("repeat", "aaa")]),
            ("Zulu", true, &[("repeat", "Zulu")]),
            (
                "Zulu",
                false,
                &[("repeat", "Zulu"), ("repeat", "zulu"), ("repeat", "ZULU")],
            ),
            ("error-marker", false, &[("failed", "ERROR-MARKER")]),
            ("visible reply", false, &[("private", "Visible reply")]),
            ("secret-reasoning-marker", false, &[]),
            ("private-tool-input", false, &[]),
            ("private-tool-output", false, &[]),
            ("mermaid-source-marker", false, &[]),
            ("other-chat-marker", false, &[]),
            ("   ", false, &[]),
        ];
        for (query, case_sensitive, expected) in cases {
            harness.transcript(|this, _, cx| {
                if this.find.case_sensitive != *case_sensitive {
                    this.toggle_find_case(cx);
                }
            });
            harness.query(query);
            assert_eq!(
                harness.hits(),
                owned(expected),
                "query {query:?}, case-sensitive {case_sensitive}"
            );
        }
        assert_eq!(harness.status(), "", "a blank query reports nothing");
    }

    /// S03 (F12). Regression caught: streamed text, prepended history or a
    /// rollback renumbering or losing the active match; a removed match
    /// leaving the bar on a dead row; an obsolete query's results landing.
    #[test]
    fn find_keeps_its_active_match_through_streaming_insertion_and_rollback() {
        let mut entries = filler(40);
        entries.insert(
            20,
            assistant("middle", "ANCHOR-STABLE-MARKER sits in the middle."),
        );
        let mut tail = assistant("tail", "Streaming reply begins.");
        tail.status = Some(MessageStatus::Streaming);
        entries.push(tail);
        let count = entries.len();
        let mut harness = Harness::new(Some(entries));
        harness.open();
        harness.query("anchor-stable-marker");
        let middle = Some(("middle".to_string(), "ANCHOR-STABLE-MARKER".to_string()));
        assert_eq!(harness.active(), middle);

        // Streamed tokens add a later match: identity and position hold.
        let appended = "\n\nA late ANCHOR-STABLE-MARKER copy.";
        harness.frame(TranscriptFrame::Delta {
            upsert: Vec::new(),
            append: vec![TextAppend {
                entry: "tail".into(),
                part: "p0".into(),
                text: appended.into(),
                len: "Streaming reply begins.".len() + appended.len(),
            }],
            remove: Vec::new(),
            count,
        });
        assert_eq!(harness.active(), middle);
        assert_eq!(harness.status(), "1 of 2");

        // Older history arriving above renumbers, but never moves, the match.
        harness.frame(TranscriptFrame::Delta {
            upsert: vec![luna_doc::TranscriptUpsert {
                after: None,
                entry: assistant("older", "An older ANCHOR-STABLE-MARKER from before."),
            }],
            append: Vec::new(),
            remove: Vec::new(),
            count: count + 1,
        });
        assert_eq!(harness.active(), middle);
        assert_eq!(harness.status(), "2 of 3");

        // Rolling back the active match's message selects the next survivor.
        harness.frame(TranscriptFrame::Delta {
            upsert: Vec::new(),
            append: Vec::new(),
            remove: vec!["middle".into()],
            count,
        });
        assert_eq!(
            harness.active(),
            Some(("tail".into(), "ANCHOR-STABLE-MARKER".into()))
        );
        assert_eq!(harness.status(), "2 of 2");
        assert_eq!(
            harness.hits(),
            owned(&[
                ("older", "ANCHOR-STABLE-MARKER"),
                ("tail", "ANCHOR-STABLE-MARKER")
            ])
        );

        // Two edits before any result: only the latest query's pass lands.
        harness.transcript(|this, _, cx| {
            let input = this.find.input.clone().unwrap();
            input.update(cx, |input, cx| input.set_text("weather", cx));
            this.find_query_changed(false, cx);
            input.update(cx, |input, cx| input.set_text("older anchor", cx));
            this.find_query_changed(false, cx);
        });
        harness.window.draw();
        assert_eq!(harness.hits(), owned(&[("older", "older ANCHOR")]));
        assert_eq!(harness.status(), "1 of 1");
    }

    /// S04 and the bar's keys. Regression caught: a match inside a collapsed
    /// prompt left clipped; Enter/Shift+Enter not wrapping; Escape leaving a
    /// search-opened fold or the search-moved viewport behind, dropping the
    /// composer's focus or draft; closing undoing the user's own changes.
    #[test]
    fn find_reveals_folded_matches_and_restores_only_its_own_changes() {
        let prompt = (0..12)
            .map(|line| {
                if line == 9 {
                    "line nine holds FOLDED-MARKER".to_string()
                } else {
                    format!("prompt line {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut entries = vec![user("long-prompt", &prompt)];
        entries.extend(filler(120));
        entries.push(assistant("second", "Another FOLDED-MARKER in a reply."));
        entries.push(assistant("third", "A third FOLDED-MARKER at the end."));
        let mut harness = Harness::new(Some(entries));
        let (prompt_ix, prompt_row) = harness.row_of("long-prompt");
        let fold = |harness: &mut Harness| {
            let row = prompt_row.clone();
            harness.transcript(move |this, _, _| this.user_folds.get(&row).and_then(|f| f.open))
        };
        harness.window.update(|root, window, cx| {
            root.composer
                .update(cx, |input, cx| input.set_text("my unsent draft", cx));
            window.focus(&root.composer.read(cx).focus_handle(cx), cx);
        });
        harness.window.draw();
        assert!(harness.transcript(|this, _, _| this.pinned));

        harness.open();
        // Typed into the focused bar (keystroke syntax reserves '-').
        harness.window.simulate_input("folded");
        harness.window.draw();
        assert_eq!(
            harness.active(),
            Some(("long-prompt".into(), "FOLDED".into()))
        );
        assert_eq!(
            fold(&mut harness),
            Some(true),
            "the hidden line's fold opens"
        );
        let top = harness.transcript(|this, _, _| this.list.logical_scroll_top().item_ix);
        assert_eq!(top, prompt_ix);
        assert_eq!(harness.status(), "1 of 3");

        let step = |harness: &mut Harness, keys: &str| {
            harness.window.simulate_keystrokes(keys);
            harness.window.draw();
            harness.active().map(|(entry, _)| entry)
        };
        assert_eq!(step(&mut harness, "enter").as_deref(), Some("second"));
        assert_eq!(fold(&mut harness), None, "moving on restores Find's fold");
        assert_eq!(step(&mut harness, "enter").as_deref(), Some("third"));
        assert_eq!(step(&mut harness, "enter").as_deref(), Some("long-prompt"));
        assert_eq!(step(&mut harness, "shift-enter").as_deref(), Some("third"));

        harness.window.simulate_keystrokes("escape");
        let (open, pinned, composer_focused, draft) = harness.window.update(|root, window, cx| {
            let composer = root.composer.read(cx);
            (
                root.transcript.read(cx).find.open,
                root.transcript.read(cx).pinned,
                composer.focus_handle(cx).is_focused(window),
                composer.text().to_owned(),
            )
        });
        assert!(!open);
        assert!(pinned, "the tail-following viewport Find moved is restored");
        assert!(composer_focused, "focus returns to the composer");
        assert_eq!(draft, "my unsent draft");

        // Reopen (the query is remembered), then change the fold and the
        // viewport by hand: closing keeps the user's changes.
        harness.open();
        assert_eq!(
            harness.active(),
            Some(("long-prompt".into(), "FOLDED".into()))
        );
        assert_eq!(fold(&mut harness), Some(true));
        let toggled = prompt_row.clone();
        harness.transcript(move |this, _, _| {
            this.toggle_user_fold(toggled, prompt_ix, 110.0, 264.0, true)
        });
        // A real wheel gesture over the list (hovered first, so the list's
        // hitbox owns the wheel), the path that hands the viewport back.
        let over_list = point(px(400.0), px(400.0));
        harness.window.simulate_mouse_move(over_list);
        harness.window.draw();
        harness
            .window
            .simulate_scroll(over_list, point(px(0.0), px(-60.0)));
        harness.window.draw();
        harness.window.simulate_keystrokes("escape");
        assert_eq!(fold(&mut harness), Some(false), "the user's fold survives");
        assert!(
            !harness.transcript(|this, _, _| this.pinned),
            "the user's scroll survives"
        );
    }

    /// S06. Regression caught: a not-yet-replayed history reported as a
    /// complete "No results", or its later arrival never searched.
    #[test]
    fn find_reports_partial_history_until_the_replay_lands() {
        let mut harness = Harness::new(None);
        harness.open();
        harness.query("history-marker");
        assert_eq!(harness.status(), "Partial history");
        assert!(harness.hits().is_empty());

        let mut entries = filler(30);
        entries.insert(3, user("early", "An early HISTORY-MARKER."));
        entries.push(assistant("late", "A late HISTORY-MARKER."));
        harness.frame(TranscriptFrame::reset(&entries));
        assert_eq!(
            harness.hits(),
            owned(&[("early", "HISTORY-MARKER"), ("late", "HISTORY-MARKER")])
        );
        assert_eq!(harness.status(), "1 of 2");
        assert_eq!(
            harness.active(),
            Some(("early".into(), "HISTORY-MARKER".into()))
        );
    }

    /// Main's chat column as the shell hosts it: Cmd+F routed through the
    /// shell's own predicate, and the shell's per-render focus recovery
    /// against the last rendered frame (`restore_mounted_focus`), which
    /// returns the keyboard to the composer while a just-opened bar is
    /// still unmounted.
    struct ChatColumn {
        root: gpui::FocusHandle,
        unfocused: gpui::FocusHandle,
        state: Entity<AppState>,
        composer: Entity<ComposerInput>,
        transcript: Entity<Transcript>,
    }

    impl Render for ChatColumn {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let root = self.root.clone();
            let unfocused = self.unfocused.clone();
            let preferred = self.composer.read(cx).focus_handle(cx);
            window.defer(cx, move |window, cx| {
                crate::shell::restore_mounted_focus(&root, &preferred, &unfocused, window, cx);
            });
            div()
                .track_focus(&self.root)
                .size_full()
                .flex()
                .flex_col()
                .on_action(cx.listener(|this, _: &FindInConversation, window, cx| {
                    let composer = this.composer.read(cx).focus_handle(cx);
                    if !open_find_from_composer(&this.transcript, &composer, window, cx) {
                        cx.propagate();
                    }
                }))
                .child(div().h(px(40.0)).child(self.composer.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .child(self.transcript.clone()),
                )
        }
    }

    /// S05, Main's route. Regression caught: Cmd+F from the composer shows
    /// the bar but leaves the keyboard in the composer, so the next typed
    /// text lands in the draft (and Enter would send it to the agent).
    #[test]
    fn find_shortcut_from_the_composer_moves_typing_into_the_bar() {
        let _selection = crate::markdown::selection::test_state_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path());
        let mut window = app.open_window(|_, cx| {
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.selected_chat = Some(CHAT.into());
                state
            });
            ChatColumn {
                root: cx.focus_handle(),
                unfocused: cx.focus_handle(),
                transcript: cx.new(|cx| Transcript::new(state.clone(), cx)),
                composer: cx.new(|cx| ComposerInput::new("Message", cx)),
                state,
            }
        });
        window.update(|root, window, cx| {
            root.state.update(cx, |state, cx| {
                let entries = [assistant("reply", "The **amber** lantern glows.")];
                state
                    .apply_transcript_frame(TranscriptFrame::reset(&entries))
                    .expect("frame applies");
                cx.notify();
            });
            root.composer
                .update(cx, |input, cx| input.set_text("my draft", cx));
            window.focus(&root.composer.read(cx).focus_handle(cx), cx);
        });
        window.draw();
        let keyboard = |window: &mut TestAppWindow<ChatColumn>| {
            window.update(|root, window, cx| {
                let input = root.transcript.read(cx).find.input.clone();
                (
                    input.is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window)),
                    root.composer.read(cx).focus_handle(cx).is_focused(window),
                )
            })
        };
        assert_eq!(keyboard(&mut window), (false, true));

        // GPUI redraws a dirty window before dispatching a key; that draw's
        // focus recovery then runs right after the shortcut, as in the app.
        window.update(|_, window, _| window.refresh());
        window.simulate_keystrokes(&crate::settings::platform_combo("mod-f"));
        assert_eq!(
            keyboard(&mut window),
            (true, false),
            "the bar owns the keyboard right after the shortcut"
        );

        // Typed key by key (keystroke syntax has no bare space or '-').
        window.simulate_input("amber");
        let (query, draft, status) = window.update(|root, _, cx| {
            let query = root
                .transcript
                .read(cx)
                .find
                .input
                .as_ref()
                .map(|input| input.read(cx).text().to_owned());
            let draft = root.composer.read(cx).text().to_owned();
            let status = root
                .transcript
                .update(cx, |transcript, _| transcript.find_status().to_string());
            (query, draft, status)
        });
        assert_eq!(query.as_deref(), Some("amber"));
        assert_eq!(draft, "my draft", "typing never reaches the composer");
        assert_eq!(status, "1 of 1");
        assert_eq!(keyboard(&mut window), (true, false));

        window.simulate_keystrokes("escape");
        assert_eq!(
            keyboard(&mut window),
            (false, true),
            "Escape returns the keyboard to the composer"
        );
    }
}
