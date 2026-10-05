//! Failure-first E2E/performance acceptance for the `#` chat reference index
//! (`crates/ui/src/composer/chat_reference_search.rs`), written against the
//! real module before it is wired into the composer. Pure logic, no GPUI, no
//! engine: the module is included by path so this stays runnable without any
//! lib-crate wiring the coordinator hasn't added yet.
//!
//! Usage: chat-reference-search-bench ABSOLUTE_OUTPUT_DIR
//!
//! Writes `results.json` (every scenario's pass/fail + detail, plus 10k/50k
//! timings) and, only once every scenario passes, a
//! `chat-reference-search-bench.pass` marker. On any failure, `failure.txt`
//! names the first failing scenario and the process exits non-zero; this
//! script does not get run as part of this change — the coordinator reviews
//! and runs it.
//!
//! Corpus scenarios (docs/design/agent-delegation-ux/README.md's failure-first
//! table): bare `#`, duplicate titles across all three groups, child/fork
//! precedence over a higher-scoring unrelated match, rename/delete/archive,
//! Unicode titles, stable-id identity under a duplicate title, deterministic
//! cancellation-probe wiring, bounded per-group result count, three
//! pathological-scale correctness cases (a >600-deep related family hiding
//! an old match, a sparse Other group behind a >5000-row same-project flood,
//! and a >5000-row common-trigram flood hiding an old exact title), and
//! 10k/50k synthetic-corpus timings (reported, not asserted fast — see
//! `main`'s comment on the perf budgets).

#[path = "../src/composer/chat_reference_search.rs"]
mod chat_reference_search;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use chat_reference_search::{
    ChatReferenceGroup, ChatReferenceIndex, ChatReferenceKind, ChatReferenceRecord,
    DEFAULT_LIMIT_PER_GROUP,
};
use luna_proto::HarnessId;
use serde_json::{Value, json};

fn record(
    id: &str,
    title: &str,
    project: Option<&str>,
    parent: Option<&str>,
    source: Option<&str>,
    archived: bool,
    updated_at_ms: i64,
) -> ChatReferenceRecord {
    ChatReferenceRecord {
        id: id.to_owned(),
        title: title.to_owned(),
        project_id: project.map(str::to_owned),
        project_label: project.map(|project| format!("Project {project}")),
        harness: Some(HarnessId::Mock),
        kind: if source.is_some() {
            ChatReferenceKind::SideChat
        } else if parent.is_some() {
            ChatReferenceKind::NewThread
        } else {
            ChatReferenceKind::Thread
        },
        parent_id: parent.map(str::to_owned),
        source_id: source.map(str::to_owned),
        archived,
        updated_at_ms,
    }
}

/// Every match's `group` tag must agree with which bucket it was returned
/// in — the explicit per-match field is not just cosmetic.
fn groups_consistent(results: &chat_reference_search::ChatReferenceResults) -> bool {
    results
        .related
        .iter()
        .all(|m| m.group == ChatReferenceGroup::Related)
        && results
            .project
            .iter()
            .all(|m| m.group == ChatReferenceGroup::Project)
        && results
            .other
            .iter()
            .all(|m| m.group == ChatReferenceGroup::Other)
}

/// Every id any group's matches resolved to, used to assert "no duplicate
/// chat across groups" and "bounded per group" in one place.
fn all_ids(results: &chat_reference_search::ChatReferenceResults) -> Vec<String> {
    results
        .related
        .iter()
        .chain(&results.project)
        .chain(&results.other)
        .map(|m| m.record.id.clone())
        .collect()
}

struct Check {
    name: &'static str,
    passed: bool,
    detail: Value,
}

fn check(name: &'static str, passed: bool, detail: Value) -> Check {
    if !passed {
        eprintln!("FAILED {name}: {detail}");
    }
    Check {
        name,
        passed,
        detail,
    }
}

// ---------------------------------------------------------------------------
// Scenario: bare `#` groups and orders by recency, never scoring.
// ---------------------------------------------------------------------------
fn scenario_bare_hash() -> Check {
    let records = vec![
        record(
            "root",
            "Root conversation",
            Some("proj-a"),
            None,
            None,
            false,
            100,
        ),
        record(
            "child-1",
            "Older child",
            Some("proj-a"),
            Some("root"),
            None,
            false,
            50,
        ),
        record(
            "child-2",
            "Newer child",
            Some("proj-a"),
            Some("root"),
            None,
            false,
            90,
        ),
        record(
            "fork-1",
            "A fork of root",
            Some("proj-a"),
            None,
            Some("root"),
            false,
            80,
        ),
        record(
            "sibling-project",
            "Same project, not related",
            Some("proj-a"),
            None,
            None,
            false,
            70,
        ),
        record(
            "other-project",
            "Different project entirely",
            Some("proj-b"),
            None,
            None,
            false,
            60,
        ),
    ];
    let index = ChatReferenceIndex::new(records);
    let results = index.search(
        "",
        Some("root"),
        Some("proj-a"),
        DEFAULT_LIMIT_PER_GROUP,
        None,
    );
    let related_ids: Vec<&str> = results
        .related
        .iter()
        .map(|m| m.record.id.as_str())
        .collect();
    let project_ids: Vec<&str> = results
        .project
        .iter()
        .map(|m| m.record.id.as_str())
        .collect();
    let other_ids: Vec<&str> = results.other.iter().map(|m| m.record.id.as_str()).collect();
    // `fork-1` (parent-less but sourced from root), `child-1` and `child-2`
    // are all family members, so Related has three rows, newest first:
    // child-2 (90), fork-1 (80), child-1 (50).
    let related_ok = related_ids.len() == 3 && related_ids == ["child-2", "fork-1", "child-1"];
    let fork_in_related = related_ids.contains(&"fork-1");
    let project_ok = project_ids == ["sibling-project"];
    let other_ok = other_ids == ["other-project"];
    let no_self = !related_ids.contains(&"root")
        && !project_ids.contains(&"root")
        && !other_ids.contains(&"root");
    check(
        "bare_hash_groups_and_recency",
        related_ok
            && fork_in_related
            && project_ok
            && other_ok
            && no_self
            && groups_consistent(&results),
        json!({
            "related": related_ids, "project": project_ids, "other": other_ids,
        }),
    )
}

// ---------------------------------------------------------------------------
// Scenario: an identical title in all three groups must appear once in each
// eligible group, never collapse to a single global winner, never duplicate.
// ---------------------------------------------------------------------------
fn scenario_duplicate_titles_across_groups() -> Check {
    let title = "Status check";
    let records = vec![
        record("root", "Root", Some("proj-a"), None, None, false, 10),
        record(
            "related-dup",
            title,
            Some("proj-a"),
            Some("root"),
            None,
            false,
            10,
        ),
        record("project-dup", title, Some("proj-a"), None, None, false, 10),
        record("other-dup", title, Some("proj-b"), None, None, false, 10),
    ];
    let index = ChatReferenceIndex::new(records);
    let results = index.search(
        title,
        Some("root"),
        Some("proj-a"),
        DEFAULT_LIMIT_PER_GROUP,
        None,
    );
    let ids = all_ids(&results);
    let unique: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    let no_duplicates = unique.len() == ids.len();
    let each_group_has_its_own = results.related.iter().any(|m| m.record.id == "related-dup")
        && results.project.iter().any(|m| m.record.id == "project-dup")
        && results.other.iter().any(|m| m.record.id == "other-dup");
    check(
        "duplicate_title_present_once_per_eligible_group",
        no_duplicates && each_group_has_its_own && groups_consistent(&results),
        json!({"ids": ids}),
    )
}

// ---------------------------------------------------------------------------
// Scenario: group precedence beats raw score. A same-project chat has an
// exact-title match (highest possible score); the related child only has a
// weak substring match. Related must still surface the child in its own
// bucket rather than being starved by the higher-scoring non-family rows.
// ---------------------------------------------------------------------------
fn scenario_group_precedence_over_score() -> Check {
    let query = "weekly";
    let mut records = vec![
        record("root", "Root", Some("proj-a"), None, None, false, 10),
        // Only a weak substring hit on `query`, but Related.
        record(
            "child-weak",
            "Agenda for the weekly standup notes",
            Some("proj-a"),
            Some("root"),
            None,
            false,
            10,
        ),
    ];
    // Flood same-project rows with a stronger (exact) match so a naive
    // single global top-K (computed before grouping) would drop the child.
    for i in 0..50 {
        records.push(record(
            &format!("project-exact-{i}"),
            query,
            Some("proj-a"),
            None,
            None,
            false,
            1000 + i as i64,
        ));
    }
    let index = ChatReferenceIndex::new(records);
    let results = index.search(
        query,
        Some("root"),
        Some("proj-a"),
        DEFAULT_LIMIT_PER_GROUP,
        None,
    );
    let related_has_child = results.related.iter().any(|m| m.record.id == "child-weak");
    let project_full = results.project.len() == DEFAULT_LIMIT_PER_GROUP;
    check(
        "related_not_starved_by_higher_scoring_project_flood",
        related_has_child && project_full,
        json!({
            "relatedIds": results.related.iter().map(|m| m.record.id.clone()).collect::<Vec<_>>(),
            "projectCount": results.project.len(),
        }),
    )
}

// ---------------------------------------------------------------------------
// Scenario: rename, delete and archive. A rebuilt index must reflect the
// current title (never the stale one) by the same stable id; a dangling
// parent reference left by a delete must not panic or leak into results;
// archived rows rank after active peers at equal score.
// ---------------------------------------------------------------------------
fn scenario_rename_delete_archive() -> Check {
    let before = ChatReferenceIndex::new(vec![record(
        "chat-1",
        "Alpha rename target",
        Some("proj-a"),
        None,
        None,
        false,
        10,
    )]);
    let before_alpha = before.search("alpha", None, None, DEFAULT_LIMIT_PER_GROUP, None);
    let before_beta = before.search("beta", None, None, DEFAULT_LIMIT_PER_GROUP, None);

    // Same id, new title: the caller's normal "rebuild index after a rename"
    // flow. The old title must no longer resolve; the new one must, under
    // the identical stable id.
    let after_rename = ChatReferenceIndex::new(vec![record(
        "chat-1",
        "Beta after rename",
        Some("proj-a"),
        None,
        None,
        false,
        20,
    )]);
    let after_alpha = after_rename.search("alpha", None, None, DEFAULT_LIMIT_PER_GROUP, None);
    let after_beta = after_rename.search("beta", None, None, DEFAULT_LIMIT_PER_GROUP, None);
    let stable_id_after_rename = after_beta
        .other
        .first()
        .is_some_and(|m| m.record.id == "chat-1");

    // Delete: chat-1 removed entirely, but chat-2 still names it as a parent
    // (a dangling reference, e.g. the deletion raced the caller's rebuild).
    // chat-3 is an unrelated sibling matching the same query, so searching
    // *as* chat-2 (which exercises `related_family`'s upward climb through
    // the dangling id) still has a non-self candidate to find. Must not
    // panic and must not fabricate a result for the deleted "chat-1".
    let after_delete = ChatReferenceIndex::new(vec![
        record(
            "chat-2",
            "Orphaned child of a deleted parent",
            Some("proj-a"),
            Some("chat-1"),
            None,
            false,
            30,
        ),
        record(
            "chat-3",
            "Another orphaned-sounding chat",
            Some("proj-a"),
            None,
            None,
            false,
            40,
        ),
    ]);
    let delete_search = after_delete.search(
        "orphaned",
        Some("chat-2"),
        Some("proj-a"),
        DEFAULT_LIMIT_PER_GROUP,
        None,
    );
    let delete_ids = all_ids(&delete_search);
    let no_dangling_result = delete_ids.contains(&"chat-3".to_string())
        && !delete_ids.contains(&"chat-1".to_string())
        && !delete_ids.contains(&"chat-2".to_string());

    // Archived ranks after an active peer with an equal (exact-title) score.
    let archive_order = ChatReferenceIndex::new(vec![
        record("active", "Gamma", Some("proj-a"), None, None, false, 5),
        record("archived", "Gamma", Some("proj-a"), None, None, true, 5000),
    ]);
    let archive_results =
        archive_order.search("gamma", None, Some("proj-a"), DEFAULT_LIMIT_PER_GROUP, None);
    let archived_ranks_after = archive_results.project.len() == 2
        && archive_results.project[0].record.id == "active"
        && archive_results.project[1].record.id == "archived";

    check(
        "rename_delete_archive",
        before_alpha.total_len() == 1
            && before_beta.total_len() == 0
            && after_alpha.total_len() == 0
            && after_beta.total_len() == 1
            && stable_id_after_rename
            && no_dangling_result
            && archived_ranks_after,
        json!({
            "beforeAlpha": before_alpha.total_len(),
            "beforeBeta": before_beta.total_len(),
            "afterAlpha": after_alpha.total_len(),
            "afterBeta": after_beta.total_len(),
            "archivedOrder": archive_results.project.iter().map(|m| m.record.id.clone()).collect::<Vec<_>>(),
        }),
    )
}

// ---------------------------------------------------------------------------
// Scenario: Unicode titles — multi-byte substrings/prefixes at non-ASCII
// char boundaries must neither panic nor silently fail to match.
// ---------------------------------------------------------------------------
fn scenario_unicode() -> Check {
    let records = vec![
        record(
            "cafe",
            "Café Résumé planning",
            Some("proj-a"),
            None,
            None,
            false,
            10,
        ),
        record(
            "emoji",
            "🚀 Launch plan review",
            Some("proj-a"),
            None,
            None,
            false,
            20,
        ),
        record(
            "cjk",
            "日本語 チャット計画",
            Some("proj-a"),
            None,
            None,
            false,
            30,
        ),
    ];
    let index = ChatReferenceIndex::new(records);
    // Case-insensitive across a non-ASCII letter.
    let cafe = index.search("CAFÉ", None, Some("proj-a"), DEFAULT_LIMIT_PER_GROUP, None);
    let resume_substring = index.search(
        "résumé",
        None,
        Some("proj-a"),
        DEFAULT_LIMIT_PER_GROUP,
        None,
    );
    let emoji_prefix = index.search("🚀", None, Some("proj-a"), DEFAULT_LIMIT_PER_GROUP, None);
    let cjk_substring = index.search(
        "チャット",
        None,
        Some("proj-a"),
        DEFAULT_LIMIT_PER_GROUP,
        None,
    );
    check(
        "unicode_case_and_multibyte_boundaries",
        cafe.total_len() == 1
            && cafe.project.first().is_some_and(|m| m.record.id == "cafe")
            && resume_substring.total_len() == 1
            && emoji_prefix.total_len() == 1
            && cjk_substring.total_len() == 1,
        json!({
            "cafe": cafe.total_len(),
            "resume": resume_substring.total_len(),
            "emoji": emoji_prefix.total_len(),
            "cjk": cjk_substring.total_len(),
        }),
    )
}

// ---------------------------------------------------------------------------
// Scenario: two records share a title; results must carry the caller's
// exact, distinct stable ids, never conflate them.
// ---------------------------------------------------------------------------
fn scenario_stable_ids_under_duplicate_title() -> Check {
    let records = vec![
        record(
            "id-a",
            "Shared title",
            Some("proj-a"),
            None,
            None,
            false,
            10,
        ),
        record(
            "id-b",
            "Shared title",
            Some("proj-a"),
            None,
            None,
            false,
            20,
        ),
    ];
    let index = ChatReferenceIndex::new(records);
    let results = index.search(
        "shared title",
        None,
        Some("proj-a"),
        DEFAULT_LIMIT_PER_GROUP,
        None,
    );
    let ids: std::collections::HashSet<String> = results
        .project
        .iter()
        .map(|m| m.record.id.clone())
        .collect();
    check(
        "stable_ids_distinguish_duplicate_titles",
        results.project.len() == 2 && ids.contains("id-a") && ids.contains("id-b"),
        json!({"ids": ids.into_iter().collect::<Vec<_>>()}),
    )
}

// ---------------------------------------------------------------------------
// Scenario: bounded per-group result count under a flood of matches.
// ---------------------------------------------------------------------------
fn scenario_bounded_result_count() -> Check {
    let mut records = Vec::new();
    for i in 0..500 {
        records.push(record(
            &format!("flood-{i}"),
            "Weekly sync notes",
            Some("proj-a"),
            None,
            None,
            false,
            i as i64,
        ));
    }
    let index = ChatReferenceIndex::new(records);
    let limit = 5;
    let results = index.search("weekly", None, Some("proj-a"), limit, None);
    check(
        "bounded_per_group_result_count",
        results.project.len() == limit,
        json!({"projectCount": results.project.len(), "limit": limit}),
    )
}

// ---------------------------------------------------------------------------
// Scenario: cancellation. Deterministic probe/work-count bounds, not a
// relative-speed measurement (the previous timing-ratio assertion was
// flaky — a correctness-first scan can legitimately take longer now that
// results, not candidates, are what's bounded). An immediately-tripped
// probe and a probe that counts its own calls must both: actually get
// polled (proving the scan loop checks it, not just accepts the parameter),
// and always return a well-formed (never over-`limit_per_group`) result.
// ---------------------------------------------------------------------------
fn scenario_cancellation(big_records: &[ChatReferenceRecord]) -> Check {
    let index = ChatReferenceIndex::new(big_records.to_vec());
    let query = "sprint";

    let immediate = AtomicBool::new(true);
    let immediate_probe: &dyn Fn() -> bool = &|| immediate.load(Ordering::Relaxed);
    let immediate_results = index.search(
        query,
        None,
        Some("proj-0"),
        DEFAULT_LIMIT_PER_GROUP,
        Some(immediate_probe),
    );

    let counter = AtomicUsize::new(0);
    let budget = 10usize;
    let counting_probe: &dyn Fn() -> bool = &|| counter.fetch_add(1, Ordering::Relaxed) >= budget;
    let partial_results = index.search(
        query,
        None,
        Some("proj-0"),
        DEFAULT_LIMIT_PER_GROUP,
        Some(counting_probe),
    );
    let polls_observed = counter.load(Ordering::Relaxed);

    let well_formed = |results: &chat_reference_search::ChatReferenceResults| {
        results.related.len() <= DEFAULT_LIMIT_PER_GROUP
            && results.project.len() <= DEFAULT_LIMIT_PER_GROUP
            && results.other.len() <= DEFAULT_LIMIT_PER_GROUP
    };
    // The corpus/query is sized (see `main`) so a real scan crosses the
    // budget many times over; observing fewer polls than the budget would
    // mean the probe was never actually wired into the scan loop.
    let probe_is_wired = polls_observed >= budget;

    check(
        "cancellation_probe_is_polled_and_results_stay_well_formed",
        well_formed(&immediate_results) && well_formed(&partial_results) && probe_is_wired,
        json!({
            "immediateTotal": immediate_results.total_len(),
            "partialTotal": partial_results.total_len(),
            "pollsObserved": polls_observed,
            "pollBudget": budget,
        }),
    )
}

// ---------------------------------------------------------------------------
// Scenario: a related family deep enough (>600) that the old
// `FAMILY_SCAN_CAP` would have truncated it before reaching an old exact
// match at the far end of the chain. Must still be found, and still
// Related.
// ---------------------------------------------------------------------------
fn scenario_large_related_family_finds_old_match() -> Check {
    let mut records = vec![record(
        "root",
        "Root",
        Some("proj-a"),
        None,
        None,
        false,
        1_000_000,
    )];
    let mut previous = "root".to_string();
    for i in 0..650 {
        let id = format!("desc-{i}");
        records.push(record(
            &id,
            &format!("Routine update {i}"),
            Some("proj-a"),
            Some(previous.as_str()),
            None,
            false,
            1_000_000 - i as i64,
        ));
        previous = id;
    }
    // Buried at the far end of a 651-deep chain, and the oldest row by far.
    records.push(record(
        "old-target",
        "Quarterly roadmap review",
        Some("proj-a"),
        Some(previous.as_str()),
        None,
        false,
        1,
    ));
    let index = ChatReferenceIndex::new(records);
    let started = Instant::now();
    let results = index.search(
        "quarterly roadmap",
        Some("root"),
        Some("proj-a"),
        DEFAULT_LIMIT_PER_GROUP,
        None,
    );
    let elapsed_us = started.elapsed().as_secs_f64() * 1_000_000.0;
    let found = results.related.iter().any(|m| m.record.id == "old-target");
    check(
        "large_related_family_finds_old_exact_match",
        found && groups_consistent(&results),
        json!({
            "familyDepth": 651,
            "relatedIds": results.related.iter().map(|m| m.record.id.clone()).collect::<Vec<_>>(),
            "elapsedUs": elapsed_us,
        }),
    )
}

// ---------------------------------------------------------------------------
// Scenario: a sparse Other group behind a flood of >5000 same-project rows
// that are all newer than the (only) two Other-project chats. The old
// `DEFAULT_SCAN_CAP` global-recency walk would exhaust its budget on the
// flood and never reach the sparse Other rows.
// ---------------------------------------------------------------------------
fn scenario_sparse_other_group_behind_project_flood() -> Check {
    let mut records = vec![record(
        "root",
        "Root",
        Some("proj-a"),
        None,
        None,
        false,
        10_000_000,
    )];
    for i in 0..5_200 {
        records.push(record(
            &format!("flood-{i}"),
            &format!("Daily log {i}"),
            Some("proj-a"),
            None,
            None,
            false,
            9_000_000 + i as i64,
        ));
    }
    records.push(record(
        "other-1",
        "Old cross-project note",
        Some("proj-b"),
        None,
        None,
        false,
        10,
    ));
    records.push(record(
        "other-2",
        "Another old cross-project note",
        Some("proj-b"),
        None,
        None,
        false,
        5,
    ));
    let index = ChatReferenceIndex::new(records);
    let started = Instant::now();
    let results = index.search(
        "",
        Some("root"),
        Some("proj-a"),
        DEFAULT_LIMIT_PER_GROUP,
        None,
    );
    let elapsed_us = started.elapsed().as_secs_f64() * 1_000_000.0;
    let other_ids: Vec<&str> = results.other.iter().map(|m| m.record.id.as_str()).collect();
    check(
        "sparse_other_group_found_behind_project_flood",
        other_ids.len() == 2 && other_ids.contains(&"other-1") && other_ids.contains(&"other-2"),
        json!({"otherIds": other_ids, "projectFloodSize": 5_200, "elapsedUs": elapsed_us}),
    )
}

// ---------------------------------------------------------------------------
// Scenario: >5000 titles sharing common trigrams/words with the query, plus
// one much older title that is an EXACT match. The old `MAX_POSTING` cap
// could silently drop the old title from a flooded gram's postings before
// it was ever indexed. Must still be found, and must still outrank the
// flood by score.
// ---------------------------------------------------------------------------
fn scenario_common_trigram_flood_finds_old_exact_match() -> Check {
    let mut records = Vec::new();
    for i in 0..50_000 {
        records.push(record(
            &format!("common-{i}"),
            &format!("weekly status update number {i}"),
            Some("proj-a"),
            None,
            None,
            false,
            2_000_000 + i as i64,
        ));
    }
    records.push(record(
        "old-exact",
        "weekly status update",
        Some("proj-a"),
        None,
        None,
        false,
        1,
    ));
    let index = ChatReferenceIndex::new(records);
    let started = Instant::now();
    let results = index.search(
        "weekly status update",
        None,
        Some("proj-a"),
        DEFAULT_LIMIT_PER_GROUP,
        None,
    );
    let elapsed_us = started.elapsed().as_secs_f64() * 1_000_000.0;
    let found = results.project.iter().any(|m| m.record.id == "old-exact");
    check(
        "common_trigram_flood_finds_old_exact_title",
        found && elapsed_us < 250_000.0,
        json!({
            "floodSize": 50_000,
            "projectIds": results.project.iter().map(|m| m.record.id.clone()).collect::<Vec<_>>(),
            "elapsedUs": elapsed_us,
            "maxQueryUsBudget": 250_000,
        }),
    )
}

// ---------------------------------------------------------------------------
// Synthetic corpus generation (no `rand` dependency: a tiny deterministic
// LCG keeps the corpus, and therefore every timing run, reproducible).
// ---------------------------------------------------------------------------
struct Lcg(u64);
impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn range(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

const WORDS: &[&str] = &[
    "alpha",
    "beta",
    "release",
    "notes",
    "refactor",
    "bugfix",
    "design",
    "review",
    "sprint",
    "planning",
    "retro",
    "incident",
    "postmortem",
    "onboarding",
    "migration",
    "rollout",
    "hotfix",
    "feature",
    "spike",
    "research",
    "weekly",
    "status",
    "sync",
    "roadmap",
    "launch",
];

fn synth_title(rng: &mut Lcg) -> String {
    let word_count = 2 + rng.range(3);
    (0..word_count)
        .map(|_| WORDS[rng.range(WORDS.len())])
        .collect::<Vec<_>>()
        .join(" ")
}

/// A mixed corpus: a handful of real child/fork families off `proj-0`'s
/// `ROOT`, ~30% of the remainder sharing `proj-0`, the rest spread across 50
/// other synthetic projects. Deterministic for a given `size`/`seed`.
fn synth_corpus(size: usize, seed: u64) -> Vec<ChatReferenceRecord> {
    let mut rng = Lcg(seed);
    let mut records = Vec::with_capacity(size);
    records.push(record(
        "root",
        "Root conversation",
        Some("proj-0"),
        None,
        None,
        false,
        0,
    ));
    let family_size = (size / 200).clamp(5, 200);
    let mut previous = "root".to_string();
    for i in 0..family_size {
        let id = format!("family-{i}");
        let is_fork = i % 7 == 0;
        records.push(record(
            &id,
            &format!("{} {}", synth_title(&mut rng), i),
            Some("proj-0"),
            (!is_fork).then_some(previous.as_str()),
            is_fork.then_some(previous.as_str()),
            i % 11 == 0,
            i as i64,
        ));
        previous = id;
    }
    for i in 0..(size - records.len()) {
        let same_project = rng.range(100) < 30;
        let project = if same_project {
            "proj-0".to_string()
        } else {
            format!("proj-{}", 1 + rng.range(50))
        };
        records.push(record(
            &format!("bulk-{i}"),
            &synth_title(&mut rng),
            Some(&project),
            None,
            None,
            rng.range(100) < 5,
            1_000 + i as i64,
        ));
    }
    records
}

struct Timing {
    label: &'static str,
    mean_us: f64,
    max_us: f64,
}

fn time_query(
    index: &ChatReferenceIndex,
    label: &'static str,
    query: &str,
    iterations: usize,
) -> Timing {
    let mut durations = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        let _ = index.search(
            query,
            Some("root"),
            Some("proj-0"),
            DEFAULT_LIMIT_PER_GROUP,
            None,
        );
        durations.push(started.elapsed().as_secs_f64() * 1_000_000.0);
    }
    let mean_us = durations.iter().sum::<f64>() / durations.len() as f64;
    let max_us = durations.iter().copied().fold(0.0, f64::max);
    Timing {
        label,
        mean_us,
        max_us,
    }
}

/// Builds the corpus, times construction, then times a representative
/// per-keystroke query mix. `max_query_us` is a generous regression guard —
/// loose on purpose since this runs on unknown, possibly shared, CI hardware.
fn perf_scenario(size: usize, max_query_us: f64) -> (Value, bool) {
    let records = synth_corpus(size, 0xC0FFEE ^ size as u64);
    let build_started = Instant::now();
    let index = ChatReferenceIndex::new(records);
    let build_us = build_started.elapsed().as_secs_f64() * 1_000_000.0;

    let queries: Vec<Timing> = vec![
        time_query(&index, "bare_hash", "", 30),
        time_query(&index, "short_common", "al", 30),
        time_query(&index, "common_word", "sprint", 30),
        time_query(&index, "rare_exact_title", "root conversation", 30),
        time_query(&index, "no_match", "zzzzznomatch", 30),
    ];
    let slowest = queries.iter().map(|t| t.max_us).fold(0.0, f64::max);
    let passed = slowest <= max_query_us;
    let json = json!({
        "corpusSize": index.len(),
        "buildUs": build_us,
        "queries": queries.iter().map(|t| json!({
            "label": t.label, "meanUs": t.mean_us, "maxUs": t.max_us,
        })).collect::<Vec<_>>(),
        "slowestQueryUs": slowest,
        "maxQueryUsBudget": max_query_us,
        "passed": passed,
    });
    (json, passed)
}

fn write_json(path: PathBuf, value: &Value) {
    let _ = std::fs::write(path, serde_json::to_vec_pretty(value).unwrap_or_default());
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
        anyhow::anyhow!("usage: chat-reference-search-bench ABSOLUTE_OUTPUT_DIR")
    })?);
    anyhow::ensure!(output.is_absolute(), "output directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in [
        "results.json",
        "failure.txt",
        "chat-reference-search-bench.pass",
    ] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    eprintln!("chat-reference-search-bench pid={}", std::process::id());

    // Sized so "sprint" (one of 25 equally-likely words) comfortably produces
    // far more than the cancellation scenario's poll budget before a natural
    // finish — otherwise "polls observed >= budget" could pass or fail by
    // corpus-statistics luck instead of actually proving the probe is wired
    // into the scan loop.
    let cancellation_corpus = synth_corpus(20_000, 0xA5A5);
    let mut checks = vec![
        scenario_bare_hash(),
        scenario_duplicate_titles_across_groups(),
        scenario_group_precedence_over_score(),
        scenario_rename_delete_archive(),
        scenario_unicode(),
        scenario_stable_ids_under_duplicate_title(),
        scenario_bounded_result_count(),
        scenario_cancellation(&cancellation_corpus),
        scenario_large_related_family_finds_old_match(),
        scenario_sparse_other_group_behind_project_flood(),
        scenario_common_trigram_flood_finds_old_exact_match(),
    ];

    // These budgets are intentionally generous and are NOT a latency SLA.
    // The index was corrected to never let a postings cap, a family-size cap
    // or a bare-query scan cap silently exclude a valid match — see the
    // three pathological scenarios above. That correctness fix means a
    // genuinely common query (many titles sharing grams/words) or a heavily
    // skewed catalog (one group holding almost everything) can now legally
    // do work proportional to the flood, not a fixed small constant, in an
    // unoptimized build. These thresholds exist only to catch an accidental
    // quadratic-or-worse regression (which shows up as orders of magnitude
    // slower, not a close call) — they must not be read as a performance
    // guarantee, and the measured numbers below are reported for the
    // coordinator to judge, not asserted as fast.
    let (ten_k, ten_k_passed) = perf_scenario(10_000, 50_000.0);
    let (fifty_k, fifty_k_passed) = perf_scenario(50_000, 200_000.0);
    checks.push(check("perf_10k_bounded", ten_k_passed, ten_k.clone()));
    checks.push(check("perf_50k_bounded", fifty_k_passed, fifty_k.clone()));

    let failed: Vec<&str> = checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| c.name)
        .collect();
    let results = json!({
        "pid": std::process::id(),
        "executable": std::env::current_exe().ok(),
        "checks": checks.iter().map(|c| json!({
            "name": c.name, "passed": c.passed, "detail": c.detail,
        })).collect::<Vec<_>>(),
        "failedChecks": failed,
        "perf10k": ten_k,
        "perf50k": fifty_k,
    });
    write_json(output.join("results.json"), &results);

    if !failed.is_empty() {
        let message = format!("failed checks: {failed:?}\n");
        let _ = std::fs::write(output.join("failure.txt"), &message);
        anyhow::bail!(message);
    }
    std::fs::write(
        output.join("chat-reference-search-bench.pass"),
        format!("{} checks passed\n", checks.len()),
    )?;
    Ok(())
}
