//! Bounded literal source-text search in one authorized checkout (ZI-05).
//!
//! The owning host resolves the chat or space through the same authorization
//! as every other Files operation, walks it with the Files tree's ignore
//! semantics, never follows a symlink, never enters `.git`, and stops at
//! explicit file, byte, elapsed and result budgets. Every stop is reported:
//! a partial answer is never presented as a complete one. Nothing is indexed,
//! cached or watched; the only retained state is the last generation of each
//! bounded number of search sessions, used to cancel superseded scans.
//!
//! Contract and measured budgets:
//! `docs/pending-tasks/zeron-upstream-improvements/zi-05/README.md`.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use luna_proto::{
    SearchWorkspaceTextRequest, WorkspaceTextMatch, WorkspaceTextSearchBudget,
    WorkspaceTextSearchFile, WorkspaceTextSearchLimit, WorkspaceTextSearchResult,
    WorkspaceTextSearchStatus,
};
use tokio_util::sync::CancellationToken;

use super::{
    MAX_PREVIEW_FILE_BYTES, MAX_SEARCH_QUERY_CHARS, WorkspaceFiles, WorkspaceFilesError,
    contains_git_component, hash_bytes, is_internal_temp_wire_path, lock, path_to_wire,
};

/// Matches reported when the caller does not ask for a limit.
pub const DEFAULT_TEXT_SEARCH_RESULTS: u16 = 200;
/// Hard ceiling on reported matches; larger `limit`s are clamped.
pub const MAX_TEXT_SEARCH_RESULTS: u16 = 500;
/// Matches reported per file before the file is marked truncated.
pub const MAX_TEXT_SEARCH_FILE_MATCHES: usize = 20;
/// Files opened per scan (binary and large files included).
pub const MAX_TEXT_SEARCH_FILES: u32 = 50_000;
/// Bytes read per scan.
pub const MAX_TEXT_SEARCH_BYTES: u64 = 256 * 1024 * 1024;
/// Wall-clock budget per request, measured from arrival (queueing included).
pub const MAX_TEXT_SEARCH_ELAPSED_MS: u32 = 5_000;
/// Text files above this size are skipped and reported. It equals the largest
/// file the Files surface can display, so every match can be opened.
pub const MAX_TEXT_SEARCH_FILE_BYTES: u64 = MAX_PREVIEW_FILE_BYTES;
/// RPC deadline: the elapsed budget plus resolution and serialization slack.
pub const TEXT_SEARCH_RPC_TIMEOUT: Duration =
    Duration::from_millis(MAX_TEXT_SEARCH_ELAPSED_MS as u64 + 3_000);
pub const MAX_TEXT_SEARCH_SESSION_BYTES: usize = 128;
/// Concurrent scans per host; further requests queue inside their budget.
pub(super) const TEXT_SEARCH_CONCURRENCY: usize = 2;
const MAX_TEXT_SEARCH_SESSIONS: usize = 256;
/// Keeps a reply far below the 1 MiB relay message limit.
const MAX_TEXT_SEARCH_RESPONSE_BYTES: usize = 512 * 1024;
const BINARY_PROBE_BYTES: u64 = 8 * 1024;
const PREVIEW_BEFORE_BYTES: usize = 48;
const PREVIEW_MATCH_BYTES: usize = 160;
const PREVIEW_AFTER_BYTES: usize = 96;
const UTF8_BOM: &[u8] = &[0xef, 0xbb, 0xbf];

/// The last generation one search session asked for, and its running scan.
pub(super) struct TextSearchSession {
    generation: u64,
    running: Option<CancellationToken>,
    touched: Instant,
}

pub(super) type TextSearchSessions = HashMap<String, TextSearchSession>;

#[derive(Debug, Clone)]
struct TextQuery {
    needle: String,
    case_sensitive: bool,
}

impl TextQuery {
    fn parse(query: &str) -> Result<Self, WorkspaceFilesError> {
        let bad = |message: &str| WorkspaceFilesError::BadParams(message.to_string());
        if query.is_empty() {
            return Err(bad("query must not be empty"));
        }
        if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
            return Err(WorkspaceFilesError::BadParams(format!(
                "query must not exceed {MAX_SEARCH_QUERY_CHARS} characters"
            )));
        }
        if query.contains(['\n', '\r']) {
            return Err(bad("query must be a single line"));
        }
        if query.contains('\0') {
            return Err(bad("query must not contain NUL"));
        }
        Ok(Self {
            needle: query.to_owned(),
            case_sensitive: query.chars().any(char::is_uppercase),
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct TextSearchBudget {
    max_results: usize,
    max_files: u32,
    max_bytes: u64,
    elapsed: Duration,
}

impl TextSearchBudget {
    fn resolve(limit: Option<u16>, requested: Option<&WorkspaceTextSearchBudget>) -> Self {
        let requested = requested.cloned().unwrap_or_default();
        let elapsed_ms = requested
            .max_elapsed_ms
            .map_or(MAX_TEXT_SEARCH_ELAPSED_MS, |ms| {
                ms.min(MAX_TEXT_SEARCH_ELAPSED_MS)
            });
        Self {
            max_results: usize::from(
                limit
                    .unwrap_or(DEFAULT_TEXT_SEARCH_RESULTS)
                    .min(MAX_TEXT_SEARCH_RESULTS),
            ),
            max_files: requested.max_files.map_or(MAX_TEXT_SEARCH_FILES, |files| {
                files.min(MAX_TEXT_SEARCH_FILES)
            }),
            max_bytes: requested.max_bytes.map_or(MAX_TEXT_SEARCH_BYTES, |bytes| {
                bytes.min(MAX_TEXT_SEARCH_BYTES)
            }),
            elapsed: Duration::from_millis(u64::from(elapsed_ms)),
        }
    }
}

#[derive(Debug, Default)]
struct TextScan {
    files: Vec<WorkspaceTextSearchFile>,
    match_count: usize,
    files_scanned: u32,
    bytes_scanned: u64,
    skipped_binary: u32,
    skipped_large: u32,
    skipped_unreadable: u32,
    response_bytes: usize,
    limits: Vec<WorkspaceTextSearchLimit>,
    cancelled: bool,
}

impl TextScan {
    fn limit(&mut self, limit: WorkspaceTextSearchLimit) {
        if !self.limits.contains(&limit) {
            self.limits.push(limit);
        }
    }

    fn unreadable(&mut self) {
        self.skipped_unreadable = self.skipped_unreadable.saturating_add(1);
        self.limit(WorkspaceTextSearchLimit::Unreadable);
    }

    fn into_result(
        self,
        checkout_id: String,
        request: &SearchWorkspaceTextRequest,
        case_sensitive: bool,
        status: WorkspaceTextSearchStatus,
        started: Instant,
    ) -> WorkspaceTextSearchResult {
        // A superseded answer is never a basis for results: report what was
        // spent, not what was found.
        let superseded = status == WorkspaceTextSearchStatus::Superseded;
        WorkspaceTextSearchResult {
            checkout_id,
            query: request.query.clone(),
            generation: request.generation,
            case_sensitive,
            status,
            limits: if superseded { Vec::new() } else { self.limits },
            match_count: if superseded {
                0
            } else {
                u32::try_from(self.match_count).unwrap_or(u32::MAX)
            },
            files: if superseded { Vec::new() } else { self.files },
            files_scanned: self.files_scanned,
            bytes_scanned: self.bytes_scanned,
            skipped_binary: self.skipped_binary,
            skipped_large: self.skipped_large,
            skipped_unreadable: self.skipped_unreadable,
            elapsed_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
        }
    }
}

impl WorkspaceFiles {
    /// Search literal text in the target's authorized checkout on this host.
    pub async fn search_text(
        &self,
        request: SearchWorkspaceTextRequest,
    ) -> Result<WorkspaceTextSearchResult, WorkspaceFilesError> {
        let started = Instant::now();
        let query = TextQuery::parse(&request.query)?;
        let session = validate_session(request.search_session.as_deref())?.map(str::to_owned);
        let budget = TextSearchBudget::resolve(request.limit, request.budget.as_ref());
        if self.inner.cancel.is_cancelled() {
            return Err(shutting_down());
        }
        let token = self.inner.cancel.child_token();
        if let Some(session) = session.as_deref()
            && !self.admit_text_search(session, request.generation, &token)
        {
            return Ok(TextScan::default().into_result(
                String::new(),
                &request,
                query.case_sensitive,
                WorkspaceTextSearchStatus::Superseded,
                started,
            ));
        }
        let result = self
            .run_text_search(&request, &query, budget, started, token)
            .await;
        if let Some(session) = session.as_deref() {
            self.finish_text_search(session, request.generation);
        }
        if let Ok(result) = &result {
            tracing::debug!(
                status = ?result.status,
                matches = result.match_count,
                files = result.files_scanned,
                bytes = result.bytes_scanned,
                elapsed_ms = result.elapsed_ms,
                query_chars = request.query.chars().count(),
                "workspace text search finished"
            );
        }
        result
    }

    async fn run_text_search(
        &self,
        request: &SearchWorkspaceTextRequest,
        query: &TextQuery,
        budget: TextSearchBudget,
        started: Instant,
        token: CancellationToken,
    ) -> Result<WorkspaceTextSearchResult, WorkspaceFilesError> {
        // Dropping this request (RPC timeout, closed connection) stops its scan.
        let guard = token.clone().drop_guard();
        let workspace = self.resolve_target(&request.target).await?;
        let deadline = tokio::time::Instant::from_std(started + budget.elapsed);
        let permit = tokio::select! {
            biased;
            _ = token.cancelled() => None,
            permit = self.inner.text_search_permits.clone().acquire_owned() => permit.ok(),
            _ = tokio::time::sleep_until(deadline) => None,
        };
        let scan = match permit {
            Some(permit) => {
                let root = workspace.root.clone();
                let query = query.clone();
                let scan_token = token.clone();
                let include_ignored = request.include_ignored;
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    scan_checkout(&root, &query, include_ignored, budget, started, &scan_token)
                })
                .await
                .map_err(|error| {
                    WorkspaceFilesError::Io(format!("text search worker failed: {error}"))
                })??
            }
            None if token.is_cancelled() => TextScan {
                cancelled: true,
                ..TextScan::default()
            },
            None => {
                let mut scan = TextScan::default();
                scan.limit(WorkspaceTextSearchLimit::Elapsed);
                scan
            }
        };
        guard.disarm();
        let status = if scan.cancelled {
            if self.inner.cancel.is_cancelled() {
                return Err(shutting_down());
            }
            WorkspaceTextSearchStatus::Superseded
        } else if scan.limits.is_empty() {
            WorkspaceTextSearchStatus::Complete
        } else {
            WorkspaceTextSearchStatus::Partial
        };
        Ok(scan.into_result(
            workspace.checkout_id,
            request,
            query.case_sensitive,
            status,
            started,
        ))
    }

    /// Record `generation` as the session's latest request and cancel the
    /// scan it replaces. An older generation arriving late is refused.
    fn admit_text_search(&self, session: &str, generation: u64, token: &CancellationToken) -> bool {
        let mut sessions = lock(&self.inner.text_searches);
        let now = Instant::now();
        if let Some(existing) = sessions.get_mut(session) {
            existing.touched = now;
            if existing.generation > generation {
                return false;
            }
            if let Some(running) = existing.running.take() {
                running.cancel();
            }
            existing.generation = generation;
            existing.running = Some(token.clone());
            return true;
        }
        if sessions.len() >= MAX_TEXT_SEARCH_SESSIONS
            && let Some(oldest) = sessions
                .iter()
                .filter(|(_, state)| {
                    state
                        .running
                        .as_ref()
                        .is_none_or(CancellationToken::is_cancelled)
                })
                .min_by_key(|(_, state)| state.touched)
                .map(|(key, _)| key.clone())
        {
            sessions.remove(&oldest);
        }
        sessions.insert(
            session.to_owned(),
            TextSearchSession {
                generation,
                running: Some(token.clone()),
                touched: now,
            },
        );
        true
    }

    fn finish_text_search(&self, session: &str, generation: u64) {
        let mut sessions = lock(&self.inner.text_searches);
        if let Some(existing) = sessions.get_mut(session)
            && existing.generation == generation
        {
            existing.running = None;
            existing.touched = Instant::now();
        }
    }
}

fn shutting_down() -> WorkspaceFilesError {
    WorkspaceFilesError::Io("workspace file service is shutting down".into())
}

fn validate_session(session: Option<&str>) -> Result<Option<&str>, WorkspaceFilesError> {
    match session {
        None => Ok(None),
        Some(value)
            if value.is_empty()
                || value.len() > MAX_TEXT_SEARCH_SESSION_BYTES
                || value.chars().any(char::is_control) =>
        {
            Err(WorkspaceFilesError::BadParams(format!(
                "searchSession must be 1-{MAX_TEXT_SEARCH_SESSION_BYTES} bytes without control characters"
            )))
        }
        Some(value) => Ok(Some(value)),
    }
}

fn scan_checkout(
    root: &Path,
    query: &TextQuery,
    include_ignored: bool,
    budget: TextSearchBudget,
    started: Instant,
    cancel: &CancellationToken,
) -> Result<TextScan, WorkspaceFilesError> {
    let mut scan = TextScan::default();
    if started.elapsed() >= budget.elapsed {
        scan.limit(WorkspaceTextSearchLimit::Elapsed);
        return Ok(scan);
    }
    let root =
        std::fs::canonicalize(root).map_err(|error| WorkspaceFilesError::Io(error.to_string()))?;
    let mut builder = ignore::WalkBuilder::new(&root);
    builder
        .follow_links(false)
        .hidden(false)
        // Deterministic order: partial answers and stable result identity
        // must not depend on directory-entry order.
        .sort_by_file_name(|left, right| left.cmp(right))
        .filter_entry(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        });
    if include_ignored {
        builder.standard_filters(false);
    }

    let mut parents = ParentCheck::new(root.clone());
    let mut buffer = Vec::new();
    let mut lowered = String::new();
    let mut walked_root = false;
    for result in builder.build() {
        if cancel.is_cancelled() {
            scan.cancelled = true;
            return Ok(scan);
        }
        if started.elapsed() >= budget.elapsed {
            scan.limit(WorkspaceTextSearchLimit::Elapsed);
            break;
        }
        let entry = match result {
            Ok(entry) => entry,
            Err(error) if !walked_root => return Err(WorkspaceFilesError::Io(error.to_string())),
            Err(_) => {
                scan.unreadable();
                continue;
            }
        };
        walked_root = true;
        if entry.depth() == 0 || !entry.file_type().is_some_and(|kind| kind.is_file()) {
            // Directories are traversed, symlinks are never followed and
            // special files are not text.
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(&root) else {
            scan.unreadable();
            continue;
        };
        if contains_git_component(relative) {
            continue;
        }
        let Ok(path) = path_to_wire(relative) else {
            scan.unreadable();
            continue;
        };
        if is_internal_temp_wire_path(&path) {
            continue;
        }
        if scan.match_count >= budget.max_results {
            scan.limit(WorkspaceTextSearchLimit::Results);
            break;
        }
        if scan.files_scanned >= budget.max_files {
            scan.limit(WorkspaceTextSearchLimit::Files);
            break;
        }
        if scan.bytes_scanned >= budget.max_bytes {
            scan.limit(WorkspaceTextSearchLimit::Bytes);
            break;
        }
        if !parents.contains(entry.path()) {
            scan.unreadable();
            continue;
        }
        let remaining_bytes = budget.max_bytes - scan.bytes_scanned;
        let read = match read_candidate(entry.path(), remaining_bytes, &mut buffer) {
            Candidate::Vanished => continue,
            Candidate::Unreadable => {
                scan.unreadable();
                continue;
            }
            Candidate::OverBudget { read } => {
                scan.bytes_scanned += read;
                scan.limit(WorkspaceTextSearchLimit::Bytes);
                break;
            }
            Candidate::Binary { read } => {
                scan.files_scanned += 1;
                scan.bytes_scanned += read;
                scan.skipped_binary += 1;
                continue;
            }
            Candidate::Large { read } => {
                scan.files_scanned += 1;
                scan.bytes_scanned += read;
                scan.skipped_large += 1;
                scan.limit(WorkspaceTextSearchLimit::LargeFiles);
                continue;
            }
            Candidate::Text { read } => read,
        };
        scan.files_scanned += 1;
        scan.bytes_scanned += read;
        let body = buffer.strip_prefix(UTF8_BOM).unwrap_or(buffer.as_slice());
        let Ok(text) = std::str::from_utf8(body) else {
            // Not displayable as text by Files either: not a coverage gap.
            scan.skipped_binary += 1;
            continue;
        };
        let remaining = budget.max_results - scan.match_count;
        let cap = MAX_TEXT_SEARCH_FILE_MATCHES.min(remaining);
        let found = find_line_matches(text, query, &mut lowered, cap);
        if found.matches.is_empty() {
            continue;
        }
        let estimate = path.len()
            + 128
            + found
                .matches
                .iter()
                .map(|found| 2 * found.preview.len() + 192)
                .sum::<usize>();
        if scan.response_bytes + estimate > MAX_TEXT_SEARCH_RESPONSE_BYTES {
            scan.limit(WorkspaceTextSearchLimit::Results);
            break;
        }
        scan.response_bytes += estimate;
        if found.more {
            scan.limit(if cap == MAX_TEXT_SEARCH_FILE_MATCHES {
                WorkspaceTextSearchLimit::FileMatches
            } else {
                WorkspaceTextSearchLimit::Results
            });
        }
        scan.match_count += found.matches.len();
        scan.files.push(WorkspaceTextSearchFile {
            path,
            content_hash: hash_bytes(&buffer),
            matches: found.matches,
            matches_truncated: found.more,
        });
        if found.more && cap < MAX_TEXT_SEARCH_FILE_MATCHES {
            break;
        }
    }
    Ok(scan)
}

/// Re-verifies, per directory, that a walked file's parent still resolves
/// inside the root. The final component is opened with `O_NOFOLLOW`.
struct ParentCheck {
    root: PathBuf,
    verified: Option<PathBuf>,
}

impl ParentCheck {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            verified: None,
        }
    }

    fn contains(&mut self, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        if self.verified.as_deref() == Some(parent) {
            return true;
        }
        match std::fs::canonicalize(parent) {
            Ok(canonical) if canonical.starts_with(&self.root) => {
                self.verified = Some(parent.to_path_buf());
                true
            }
            _ => false,
        }
    }
}

enum Candidate {
    Text {
        read: u64,
    },
    Binary {
        read: u64,
    },
    Large {
        read: u64,
    },
    OverBudget {
        read: u64,
    },
    /// Deleted or replaced by a non-file after the walk listed it.
    Vanished,
    Unreadable,
}

fn read_candidate(path: &Path, remaining: u64, buffer: &mut Vec<u8>) -> Candidate {
    buffer.clear();
    let mut file = match open_without_following(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Candidate::Vanished,
        // Includes ELOOP: the entry was replaced by a symlink after the walk.
        Err(_) => return Candidate::Unreadable,
    };
    let Ok(metadata) = file.metadata() else {
        return Candidate::Unreadable;
    };
    if !metadata.is_file() {
        return Candidate::Vanished;
    }
    let size = metadata.len();
    let probe = size.min(BINARY_PROBE_BYTES);
    if probe > remaining {
        return Candidate::OverBudget { read: 0 };
    }
    if (&mut file).take(probe).read_to_end(buffer).is_err() {
        return Candidate::Unreadable;
    }
    let probed = buffer.len();
    if buffer.contains(&0) {
        return Candidate::Binary {
            read: probed as u64,
        };
    }
    if size > MAX_TEXT_SEARCH_FILE_BYTES {
        return Candidate::Large {
            read: probed as u64,
        };
    }
    if size > remaining {
        return Candidate::OverBudget {
            read: probed as u64,
        };
    }
    // One byte past the cap detects a file that grew while it was read.
    let rest = (MAX_TEXT_SEARCH_FILE_BYTES + 1).saturating_sub(probed as u64);
    if (&mut file).take(rest).read_to_end(buffer).is_err() {
        return Candidate::Unreadable;
    }
    let read = buffer.len() as u64;
    if read > MAX_TEXT_SEARCH_FILE_BYTES {
        return Candidate::Large { read };
    }
    if buffer[probed..].contains(&0) {
        return Candidate::Binary { read };
    }
    Candidate::Text { read }
}

#[cfg(unix)]
fn open_without_following(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NONBLOCK keeps a FIFO swapped in after the walk from blocking the
    // scan; it does not change reads from regular files.
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_without_following(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

struct FoundMatches {
    matches: Vec<WorkspaceTextMatch>,
    /// Another match exists beyond the ones reported.
    more: bool,
}

/// Non-overlapping, leftmost literal matches with 1-based lines and byte
/// ranges inside each line's text (terminator excluded).
fn find_line_matches(
    text: &str,
    query: &TextQuery,
    lowered: &mut String,
    cap: usize,
) -> FoundMatches {
    let haystack = if query.case_sensitive {
        text
    } else {
        // ASCII folding keeps every byte offset identical to `text`.
        lowered.clear();
        lowered.push_str(text);
        lowered.make_ascii_lowercase();
        lowered.as_str()
    };
    let bytes = text.as_bytes();
    let mut matches = Vec::new();
    let mut line = 1u32;
    let mut line_start = 0usize;
    let mut counted_to = 0usize;
    let mut more = false;
    for (start, _) in haystack.match_indices(query.needle.as_str()) {
        if matches.len() == cap {
            more = true;
            break;
        }
        for (offset, byte) in bytes[counted_to..start].iter().enumerate() {
            if *byte == b'\n' {
                line = line.saturating_add(1);
                line_start = counted_to + offset + 1;
            }
        }
        counted_to = start;
        let end = start + query.needle.len();
        let line_end = text[end..]
            .find('\n')
            .map_or(text.len(), |index| end + index);
        let content_end = if line_end > line_start && bytes[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };
        matches.push(line_match(
            line,
            &text[line_start..content_end],
            start - line_start,
            end - line_start,
        ));
    }
    FoundMatches { matches, more }
}

fn line_match(line: u32, text: &str, start: usize, end: usize) -> WorkspaceTextMatch {
    let content_start = (text.len() - text.trim_start_matches([' ', '\t']).len()).min(start);
    let content_end = text.trim_end_matches([' ', '\t']).len().max(end);
    let mut window_start = start
        .saturating_sub(PREVIEW_BEFORE_BYTES)
        .max(content_start);
    while !text.is_char_boundary(window_start) {
        window_start -= 1;
    }
    let mut shown_end = end.min(start + PREVIEW_MATCH_BYTES);
    while !text.is_char_boundary(shown_end) {
        shown_end += 1;
    }
    let mut window_end = (shown_end + PREVIEW_AFTER_BYTES).min(content_end);
    while !text.is_char_boundary(window_end) {
        window_end += 1;
    }
    let as_u32 = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    WorkspaceTextMatch {
        line,
        column: as_u32(text[..start].chars().count() + 1),
        byte_start: as_u32(start),
        byte_end: as_u32(end),
        preview: text[window_start..window_end].to_string(),
        preview_match_start: as_u32(start - window_start),
        preview_match_end: as_u32(shown_end - window_start),
        preview_truncated_before: window_start > content_start,
        preview_truncated_after: window_end < content_end || shown_end < end,
    }
}
