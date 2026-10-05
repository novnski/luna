//! Pure, GPUI-free title index for the composer's `#` chat reference
//! completion. No wiring to `AppState`/`Composer` lives here — the caller
//! builds [`ChatReferenceRecord`]s from whatever chat list it already has and
//! owns generation/staleness guards around [`ChatReferenceIndex::search`].
//!
//! Deliberately NOT fuzzy: matching is case-insensitive exact/prefix/word-
//! prefix/substring only (no typo tolerance), which keeps scoring cheap and
//! fully deterministic at 50k-chat scale without pulling in a matcher crate.
//!
//! Correctness contract: nothing in this module silently excludes a chat
//! that legitimately matches. Only the OUTPUT is bounded — each of
//! Related/Project/Other keeps at most `limit_per_group` entries, via a
//! small per-group min-heap ([`BoundedTop`]) rather than ever allocating a
//! scored vector sized to the whole catalog. What can vary is the amount of
//! *work* a call does:
//! - Titles are normalized exactly once, at construction
//!   ([`ChatReferenceIndex::new`]), never per query.
//! - Whole-word and trigram postings are COMPLETE (no size cap): a query's
//!   candidate set is the exact intersection of its grams' postings, so an
//!   old exact/prefix match is never dropped because some common gram's
//!   postings list "filled up" first.
//! - "Related" (this conversation's family: ancestors/descendants/forks) is
//!   the COMPLETE set reachable via parent/source adjacency, found with
//!   cycle protection (visited sets) but no size/depth cap — a large or
//!   deep family is never truncated.
//! - A bare `#` lists by recency per group: Related walks the (complete)
//!   family; Project walks a per-project recency order cached once at
//!   construction; Other performs a correct linear merge over the global
//!   recency order (also cached once), skipping family/current-project
//!   members, continuing until it has found `limit_per_group` eligible rows
//!   or the catalog is exhausted. A sparse Other group behind a large
//!   same-project or family flood is still found — it is simply not free.
//! - A query shorter than one trigram (<3 chars) cannot be narrowed by the
//!   gram index at all (see `short_query_matches`'s doc), so it scans every
//!   normalized title once, polling the cancellation probe periodically.
//!
//! Honest performance note: in a heavily skewed catalog (one group holding
//! almost everything) or against a query many titles share grams/words
//! with, a call can legitimately do work proportional to that flood/family/
//! catalog — the alternative (the previous design's fixed caps) is exactly
//! what let a real match go missing. Cancellation is the intended way to
//! bound *latency*: run `search` off the UI thread and pass a probe tied to
//! "the query has already changed", rather than relying on an internal cap.
//!
//! Other limitations:
//! - No diacritic folding beyond `char::to_lowercase`; visually-equivalent
//!   but code-point-different Unicode titles are not unified.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use luna_proto::HarnessId;

/// Suggested default for `limit_per_group`: enough that all three groups can
/// show something meaningful without the popup growing unbounded.
pub(crate) const DEFAULT_LIMIT_PER_GROUP: usize = 8;

/// How often the full-title-scan path (queries shorter than one trigram)
/// polls its cancellation probe. A pure cadence/latency knob, never a
/// correctness limit: the scan itself always covers every title.
const CANCEL_CHECK_STRIDE: usize = 256;

const EXACT_TITLE_SCORE: i32 = 1000;
const EXACT_TOKEN_SCORE: i32 = 900;
const PREFIX_SCORE: i32 = 800;
const WORD_PREFIX_SCORE: i32 = 600;
const SUBSTRING_SCORE: i32 = 400;
/// Score used for the bare-query (no text after `#`) recency listing, where
/// there is no match quality to compare — every candidate ties on score and
/// the recency/id tiebreak alone orders them.
const RECENCY_ONLY_SCORE: i32 = 0;

/// One chat available to be `#`-referenced. Immutable snapshot of whatever
/// the caller already has in its own chat list/state; this module never
/// mutates or re-fetches it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChatReferenceRecord {
    pub id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub project_label: Option<String>,
    pub harness: Option<HarnessId>,
    pub kind: ChatReferenceKind,
    /// Child-thread lineage (`ChildThread::parent_task_id`), when this chat
    /// is a child.
    pub parent_id: Option<String>,
    /// Fork/side-chat lineage (`fork_source_rows` / side-chat parent id),
    /// when this chat is a fork rather than a child.
    pub source_id: Option<String>,
    pub archived: bool,
    pub updated_at_ms: i64,
}

/// Conversation origin, independent of which workbench currently calls it
/// Main. Generic history forks use the same Side chat term as workbench tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatReferenceKind {
    Thread,
    NewThread,
    SideChat,
}

impl ChatReferenceKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Thread => "Thread",
            Self::NewThread => "New thread",
            Self::SideChat => "Side chat",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Thread => "Independent conversation",
            Self::NewThread => "Fresh conversation; parent context is not copied",
            Self::SideChat => {
                "Fork or captured source context; later source messages are not copied"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatReferenceGroup {
    /// A descendant child/fork of the current conversation, or another
    /// member of its root family (sibling/ancestor reached the same way).
    Related,
    /// Same project as the current conversation, not already Related.
    Project,
    /// Everything else: other projects in the app.
    Other,
}

#[derive(Clone)]
pub(crate) struct ChatReferenceMatch {
    pub record: Arc<ChatReferenceRecord>,
    pub group: ChatReferenceGroup,
}

/// Bounded, grouped, already-ordered-within-group results. Each inner `Vec`
/// is at most `limit_per_group` long; a caller rendering section separators
/// can test `related.is_empty()` etc. directly instead of re-filtering a
/// flat list.
#[derive(Default)]
pub(crate) struct ChatReferenceResults {
    pub related: Vec<ChatReferenceMatch>,
    pub project: Vec<ChatReferenceMatch>,
    pub other: Vec<ChatReferenceMatch>,
}

impl ChatReferenceResults {
    #[cfg(feature = "appshots-fixture")]
    pub(crate) fn total_len(&self) -> usize {
        self.related.len() + self.project.len() + self.other.len()
    }
}

fn normalize(text: &str) -> String {
    text.trim().chars().flat_map(char::to_lowercase).collect()
}

fn tokenize(normalized_title: &str) -> impl Iterator<Item = &str> {
    normalized_title
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
}

/// Char-based (never byte-based: titles are arbitrary Unicode) sliding
/// 3-gram windows. A query/title shorter than 3 chars degrades to itself as
/// one pseudo-gram; see [`ChatReferenceIndex::short_query_matches`] for why
/// that pseudo-gram is not relied on for a short QUERY's narrowing.
fn trigrams(normalized: &str) -> Vec<String> {
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() < 3 {
        return if chars.is_empty() {
            Vec::new()
        } else {
            vec![normalized.to_owned()]
        };
    }
    (0..=chars.len() - 3)
        .map(|start| chars[start..start + 3].iter().collect())
        .collect()
}

/// Whether `query` occurs in `title` starting exactly at a word boundary
/// (string start or right after a non-alphanumeric char). Computed directly
/// against the stored normalized title instead of a second per-title token
/// list, trading a little query-time string work for materially less
/// index memory at 50k+ titles.
fn word_prefix_match(title: &str, query: &str) -> bool {
    title.match_indices(query).any(|(start, _)| {
        start == 0
            || title[..start]
                .chars()
                .next_back()
                .is_some_and(|ch| !ch.is_alphanumeric())
    })
}

/// The best applicable title-level score for `query` against `title`, or
/// `None` when it doesn't match at all. Shared by both the trigram-narrowed
/// path (>=3 chars) and the full-scan short-query path (<3 chars).
fn score_title(title: &str, query: &str) -> Option<i32> {
    if title == query {
        Some(EXACT_TITLE_SCORE)
    } else if title.starts_with(query) {
        Some(PREFIX_SCORE)
    } else if word_prefix_match(title, query) {
        Some(WORD_PREFIX_SCORE)
    } else if title.contains(query) {
        Some(SUBSTRING_SCORE)
    } else {
        None
    }
}

/// Keep the higher of any two scores a candidate earns from more than one
/// signal (for example an exact-token postings hit AND a title-level exact
/// match) — without this, the same chat could be offered to a group's
/// bounded heap twice, at two different scores, and appear twice in results.
fn bump_score(scores: &mut HashMap<u32, i32>, index: u32, score: i32) {
    let entry = scores.entry(index).or_insert(i32::MIN);
    if score > *entry {
        *entry = score;
    }
}

/// Recency/stability ordering used wherever there is no score to compare:
/// active before archived, newest first, id as the last stable tiebreak so
/// output is deterministic across equal timestamps.
fn recency_cmp(a: &ChatReferenceRecord, b: &ChatReferenceRecord) -> std::cmp::Ordering {
    a.archived
        .cmp(&b.archived)
        .then_with(|| b.updated_at_ms.cmp(&a.updated_at_ms))
        .then_with(|| a.id.cmp(&b.id))
}

/// One scored candidate, ordered so that *greater* means *more preferred*:
/// higher score, then not-archived, then more recent, then smaller id.
/// [`BoundedTop`] uses this to keep only the best `limit` per group without
/// ever sorting (or holding) more than `limit` entries at a time.
#[derive(Clone)]
struct RankedCandidate {
    score: i32,
    record: Arc<ChatReferenceRecord>,
}

impl RankedCandidate {
    fn rank_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score
            .cmp(&other.score)
            .then_with(|| other.record.archived.cmp(&self.record.archived))
            .then_with(|| self.record.updated_at_ms.cmp(&other.record.updated_at_ms))
            .then_with(|| other.record.id.cmp(&self.record.id))
    }
}
impl PartialEq for RankedCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.rank_cmp(other) == std::cmp::Ordering::Equal
    }
}
impl Eq for RankedCandidate {}
impl PartialOrd for RankedCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RankedCandidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank_cmp(other)
    }
}

/// A fixed-capacity "keep the best `limit`" structure: a min-heap (by
/// [`RankedCandidate`]'s preference order) of at most `limit` items, so
/// ranking and memory for ONE group's output is `O(limit)` regardless of how
/// many candidates are offered to it.
struct BoundedTop {
    limit: usize,
    heap: BinaryHeap<Reverse<RankedCandidate>>,
}

impl BoundedTop {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: BinaryHeap::with_capacity(limit),
        }
    }

    fn len(&self) -> usize {
        self.heap.len()
    }

    fn offer(&mut self, candidate: RankedCandidate) {
        if self.heap.len() < self.limit {
            self.heap.push(Reverse(candidate));
            return;
        }
        if let Some(Reverse(worst)) = self.heap.peek()
            && candidate > *worst
        {
            self.heap.pop();
            self.heap.push(Reverse(candidate));
        }
    }

    fn into_sorted_vec(self, group: ChatReferenceGroup) -> Vec<ChatReferenceMatch> {
        let mut items: Vec<RankedCandidate> = self.heap.into_iter().map(|Reverse(c)| c).collect();
        items.sort_by(|a, b| b.cmp(a));
        items
            .into_iter()
            .map(|candidate| ChatReferenceMatch {
                record: candidate.record,
                group,
            })
            .collect()
    }
}

pub(crate) struct ChatReferenceIndex {
    records: Vec<Arc<ChatReferenceRecord>>,
    norm_titles: Vec<String>,
    by_id: HashMap<String, u32>,
    children_of: HashMap<String, Vec<u32>>,
    forks_of: HashMap<String, Vec<u32>>,
    token_postings: HashMap<String, Vec<u32>>,
    trigram_postings: HashMap<String, Vec<u32>>,
    /// Per-project recency order, sorted once by [`recency_cmp`] at
    /// construction — the "cached per-project recency group" the bare-query
    /// Project listing reads directly instead of scanning the catalog.
    project_recency: HashMap<String, Vec<u32>>,
    /// Global recency order, sorted once by [`recency_cmp`]. The bare-query
    /// Other listing walks this (skipping family/current-project members)
    /// to perform a correct recency merge; a text query never reads it.
    recency_order: Vec<u32>,
}

impl ChatReferenceIndex {
    pub(crate) fn new(records: Vec<ChatReferenceRecord>) -> Self {
        let records: Vec<Arc<ChatReferenceRecord>> = records.into_iter().map(Arc::new).collect();
        let norm_titles: Vec<String> = records
            .iter()
            .map(|record| normalize(&record.title))
            .collect();
        let mut by_id = HashMap::with_capacity(records.len());
        let mut children_of: HashMap<String, Vec<u32>> = HashMap::new();
        let mut forks_of: HashMap<String, Vec<u32>> = HashMap::new();
        let mut token_postings: HashMap<String, Vec<u32>> = HashMap::new();
        let mut trigram_postings: HashMap<String, Vec<u32>> = HashMap::new();
        let mut project_recency: HashMap<String, Vec<u32>> = HashMap::new();
        for (index, record) in records.iter().enumerate() {
            let index = index as u32;
            // First record wins a duplicate id; a caller handing duplicate
            // ids a bounded index is a caller bug, not this module's to fix.
            by_id.entry(record.id.clone()).or_insert(index);
            if let Some(parent) = record.parent_id.as_ref().filter(|id| **id != record.id) {
                children_of.entry(parent.clone()).or_default().push(index);
            }
            if let Some(source) = record.source_id.as_ref().filter(|id| **id != record.id) {
                forks_of.entry(source.clone()).or_default().push(index);
            }
            if let Some(project_id) = &record.project_id {
                project_recency
                    .entry(project_id.clone())
                    .or_default()
                    .push(index);
            }
            let title = &norm_titles[index as usize];
            for token in tokenize(title) {
                token_postings
                    .entry(token.to_owned())
                    .or_default()
                    .push(index);
            }
            for gram in trigrams(title) {
                trigram_postings.entry(gram).or_default().push(index);
            }
        }
        // Construction visits rows in ascending order. Repeated words/grams
        // within one title must not inflate its sorted posting list.
        for postings in token_postings
            .values_mut()
            .chain(trigram_postings.values_mut())
        {
            postings.dedup();
        }
        for order in project_recency.values_mut() {
            order.sort_by(|&a, &b| recency_cmp(&records[a as usize], &records[b as usize]));
        }
        let mut recency_order: Vec<u32> = (0..records.len() as u32).collect();
        recency_order.sort_by(|&a, &b| recency_cmp(&records[a as usize], &records[b as usize]));
        Self {
            records,
            norm_titles,
            by_id,
            children_of,
            forks_of,
            token_postings,
            trigram_postings,
            project_recency,
            recency_order,
        }
    }

    #[cfg(feature = "appshots-fixture")]
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    /// This conversation's complete root family: walk the parent/source
    /// chain up from `chat_id` to its root, then every id reachable back
    /// down through `children_of`/`forks_of` from that root — so a sibling
    /// fork and the current chat's own ancestor both count as Related, not
    /// just direct descendants. Cycle-safe (visited sets) with no size/depth
    /// cap: a large or deep family is never truncated.
    fn related_family(&self, chat_id: &str, cancelled: &dyn Fn() -> bool) -> HashSet<u32> {
        let mut root = chat_id.to_owned();
        let mut climbed: HashSet<String> = HashSet::new();
        while climbed.insert(root.clone()) {
            if cancelled() {
                return HashSet::new();
            }
            let Some(&index) = self.by_id.get(&root) else {
                break;
            };
            let record = &self.records[index as usize];
            match record
                .parent_id
                .clone()
                .or_else(|| record.source_id.clone())
            {
                Some(next) if !climbed.contains(&next) => root = next,
                _ => break,
            }
        }
        let mut family = HashSet::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(root);
        while let Some(id) = queue.pop_front() {
            if cancelled() {
                return HashSet::new();
            }
            if !visited.insert(id.clone()) {
                continue;
            }
            if let Some(&index) = self.by_id.get(&id) {
                family.insert(index);
                for &child in self.children_of.get(&id).into_iter().flatten() {
                    queue.push_back(self.records[child as usize].id.clone());
                }
                for &fork in self.forks_of.get(&id).into_iter().flatten() {
                    queue.push_back(self.records[fork as usize].id.clone());
                }
            }
        }
        family
    }

    /// Complete, deduplicated matches for a query of 3+ chars: the exact
    /// intersection of every query gram's (complete) postings, each scored
    /// by [`score_title`], deduped against an exact-token postings hit via
    /// [`bump_score`]. Only records that actually match end up as entries —
    /// this is never sized to the whole catalog, only to the match count.
    fn indexed_query_matches(
        &self,
        query: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> HashMap<u32, i32> {
        let mut matches: HashMap<u32, i32> = HashMap::new();
        if cancelled() {
            return matches;
        }
        if let Some(postings) = self.token_postings.get(query) {
            for (position, &index) in postings.iter().enumerate() {
                if position % CANCEL_CHECK_STRIDE == 0 && cancelled() {
                    return matches;
                }
                bump_score(&mut matches, index, EXACT_TOKEN_SCORE);
            }
        }
        let mut grams = trigrams(query);
        grams.sort_unstable();
        grams.dedup();
        let mut lists = Vec::with_capacity(grams.len());
        for gram in &grams {
            let Some(postings) = self.trigram_postings.get(gram) else {
                return matches;
            };
            lists.push(postings);
        }
        lists.sort_unstable_by_key(|postings| postings.len());
        let Some(first) = lists.first() else {
            return matches;
        };
        let mut narrowed = (*first).clone();
        // Intersect sorted IDs in linear time, starting with the rarest gram.
        // This avoids rebuilding a large hash set for every common gram.
        for postings in lists.into_iter().skip(1) {
            if cancelled() {
                return matches;
            }
            let mut intersection = Vec::with_capacity(narrowed.len().min(postings.len()));
            let (mut left, mut right) = (0, 0);
            while left < narrowed.len() && right < postings.len() {
                if (left + right) % CANCEL_CHECK_STRIDE == 0 && cancelled() {
                    return matches;
                }
                match narrowed[left].cmp(&postings[right]) {
                    std::cmp::Ordering::Less => left += 1,
                    std::cmp::Ordering::Greater => right += 1,
                    std::cmp::Ordering::Equal => {
                        intersection.push(narrowed[left]);
                        left += 1;
                        right += 1;
                    }
                }
            }
            narrowed = intersection;
            if narrowed.is_empty() {
                break;
            }
        }
        for index in narrowed {
            if cancelled() {
                return matches;
            }
            let title = &self.norm_titles[index as usize];
            if let Some(score) = score_title(title, query) {
                bump_score(&mut matches, index, score);
            }
        }
        matches
    }

    /// Below one trigram (<3 chars), gram-postings narrowing is unreliable:
    /// a title only produces a pseudo-gram equal to its own full (short)
    /// text, so keying postings on the short QUERY almost never hits a
    /// longer title that legitimately contains it as a substring. The only
    /// correctness-preserving option is a full scan of every normalized
    /// title, so this polls `cancelled` every [`CANCEL_CHECK_STRIDE`] rows —
    /// the caller is expected to run this off the UI thread.
    fn short_query_matches(&self, query: &str, cancelled: &dyn Fn() -> bool) -> HashMap<u32, i32> {
        let mut matches: HashMap<u32, i32> = HashMap::new();
        if cancelled() {
            return matches;
        }
        if let Some(postings) = self.token_postings.get(query) {
            for (position, &index) in postings.iter().enumerate() {
                if position % CANCEL_CHECK_STRIDE == 0 && cancelled() {
                    return matches;
                }
                bump_score(&mut matches, index, EXACT_TOKEN_SCORE);
            }
        }
        for (index, title) in self.norm_titles.iter().enumerate() {
            if index % CANCEL_CHECK_STRIDE == 0 && cancelled() {
                return matches;
            }
            if let Some(score) = score_title(title, query) {
                bump_score(&mut matches, index as u32, score);
            }
        }
        matches
    }

    /// Bare `#`: recency per group, no scoring. Related walks the complete
    /// `family`; Project walks the cached per-project recency order; Other
    /// walks the cached global recency order, skipping family/current-
    /// project members, so a sparse Other group behind a large flood is
    /// still found rather than cut off after a fixed number of rows.
    #[allow(clippy::too_many_arguments)]
    fn bare_query_fill(
        &self,
        related: &mut BoundedTop,
        project: &mut BoundedTop,
        other: &mut BoundedTop,
        self_index: Option<u32>,
        current_project_id: Option<&str>,
        family: &HashSet<u32>,
        limit: usize,
        cancelled: &dyn Fn() -> bool,
    ) {
        let mut seen: HashSet<u32> = HashSet::new();
        for &index in family {
            if cancelled() {
                return;
            }
            if Some(index) == self_index || !seen.insert(index) {
                continue;
            }
            related.offer(RankedCandidate {
                score: RECENCY_ONLY_SCORE,
                record: self.records[index as usize].clone(),
            });
        }
        if let Some(order) = current_project_id.and_then(|id| self.project_recency.get(id)) {
            for &index in order {
                if project.len() >= limit {
                    break;
                }
                if cancelled() {
                    return;
                }
                if Some(index) == self_index || family.contains(&index) || !seen.insert(index) {
                    continue;
                }
                project.offer(RankedCandidate {
                    score: RECENCY_ONLY_SCORE,
                    record: self.records[index as usize].clone(),
                });
            }
        }
        for &index in &self.recency_order {
            if other.len() >= limit {
                break;
            }
            if cancelled() {
                return;
            }
            if Some(index) == self_index || family.contains(&index) || !seen.insert(index) {
                continue;
            }
            if current_project_id.is_some()
                && self.records[index as usize].project_id.as_deref() == current_project_id
            {
                continue;
            }
            other.offer(RankedCandidate {
                score: RECENCY_ONLY_SCORE,
                record: self.records[index as usize].clone(),
            });
        }
    }

    /// Bounded, grouped lookup. `query` empty/whitespace-only is the bare
    /// `#` case (see [`Self::bare_query_fill`]). `limit_per_group` is the max
    /// rows returned for each of Related/Project/Other —
    /// [`DEFAULT_LIMIT_PER_GROUP`] is the recommended value. `cancelled`, if
    /// given, is polled periodically so a superseded query (the composer has
    /// since typed further) can bail out early instead of finishing scoring
    /// work nobody will read; the caller still owns discarding the result
    /// via its own generation check — this is a cooperative speed-up, not a
    /// correctness guarantee on its own. See the module doc for why a scan's
    /// cost can legitimately scale with a flood/family/catalog now that
    /// results, not candidates, are what is bounded.
    pub(crate) fn search(
        &self,
        query: &str,
        current_chat_id: Option<&str>,
        current_project_id: Option<&str>,
        limit_per_group: usize,
        cancelled: Option<&dyn Fn() -> bool>,
    ) -> ChatReferenceResults {
        let limit = limit_per_group.max(1);
        // A named local (not a temporary) so the fallback probe outlives
        // every use of `cancelled` below.
        let no_cancel = || false;
        let cancelled = cancelled.unwrap_or(&no_cancel);
        if cancelled() {
            return ChatReferenceResults::default();
        }
        let self_index = current_chat_id.and_then(|id| self.by_id.get(id).copied());
        let family = current_chat_id
            .map(|id| self.related_family(id, cancelled))
            .unwrap_or_default();

        let mut related = BoundedTop::new(limit);
        let mut project = BoundedTop::new(limit);
        let mut other = BoundedTop::new(limit);

        let query_norm = normalize(query);
        if query_norm.is_empty() {
            self.bare_query_fill(
                &mut related,
                &mut project,
                &mut other,
                self_index,
                current_project_id,
                &family,
                limit,
                cancelled,
            );
        } else {
            let matches = if query_norm.chars().count() < 3 {
                self.short_query_matches(&query_norm, cancelled)
            } else {
                self.indexed_query_matches(&query_norm, cancelled)
            };
            for (position, (index, score)) in matches.into_iter().enumerate() {
                if position % CANCEL_CHECK_STRIDE == 0 && cancelled() {
                    break;
                }
                if Some(index) == self_index {
                    continue;
                }
                let record = self.records[index as usize].clone();
                let candidate = RankedCandidate { score, record };
                if family.contains(&index) {
                    related.offer(candidate);
                } else if current_project_id.is_some()
                    && candidate.record.project_id.as_deref() == current_project_id
                {
                    project.offer(candidate);
                } else {
                    other.offer(candidate);
                }
            }
        }

        ChatReferenceResults {
            related: related.into_sorted_vec(ChatReferenceGroup::Related),
            project: project.into_sorted_vec(ChatReferenceGroup::Project),
            other: other.into_sorted_vec(ChatReferenceGroup::Other),
        }
    }
}
