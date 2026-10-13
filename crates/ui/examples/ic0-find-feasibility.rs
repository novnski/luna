//! IC-0 complete-history Find feasibility (no window, no engine). Failure
//! contract first: docs/pending-tasks/interactive-chat-surfaces/ic-0/README.md
//! ("prototype", F1-F13).
//!
//! Builds a representative 10,000-entry, ~10 MiB conversation in a real
//! `luna_doc::SessionDoc` through its ordinary writer, round-trips the doc
//! through its snapshot bytes (the host's persisted form) and reads it back
//! with `read_entries` + `join_continuation_entries`, the reader behind the
//! selected chat's `WatchDocMessages` reset that fills `AppState.transcript`.
//! It then projects display text with LUNA's own Markdown parser, matches
//! literal queries through a Unicode case-insensitive offset map, and records
//! each failure probe, streaming/rollback anchor stability and timings.
//!
//!     cargo run --release --locked -p luna-ui --example ic0-find-feasibility \
//!       --features browser-fixture -- <out-dir>
//!
//! Writes <out-dir>/result.json; exits non-zero when a correctness probe
//! fails. Timings are measurements against the provisional budget, not gates.

use anyhow::Result;
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionDoc, SessionMessageEntry};
use luna_ui::markdown::{Block, InlineRun, parse_full};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Instant};

const ENTRIES: usize = 10_000;
const TARGET_BYTES: usize = 10 * 1024 * 1024;
const CHUNK_SEGMENTS: usize = 250;
const FIRST_MATCH_BUDGET_MS: f64 = 150.;
const FRAME_BUDGET_MS: f64 = 16.;

/// A match identity: message, part and block path plus the byte range in
/// that block's display text. Never a list index.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Anchor {
    message: String,
    part: String,
    block: Vec<usize>,
    start: usize,
    end: usize,
}

/// Case-folded text plus, for every folded byte, the source byte offset of
/// the character it came from (and one trailing entry for the end).
struct Folded {
    text: String,
    map: Vec<usize>,
}

fn fold(source: &str) -> Folded {
    let mut text = String::with_capacity(source.len());
    let mut map = Vec::with_capacity(source.len() + 1);
    for (offset, ch) in source.char_indices() {
        for lower in ch.to_lowercase() {
            let start = text.len();
            text.push(lower);
            map.extend(std::iter::repeat_n(offset, text.len() - start));
        }
    }
    map.push(source.len());
    Folded { text, map }
}

/// One independent display block. Matches never span two segments.
struct Segment {
    message: String,
    part: String,
    block: Vec<usize>,
    text: String,
    folded: Folded,
}

fn inline_text(runs: &[InlineRun]) -> String {
    // Styling splits runs; joining them keeps a phrase across `**bold**` or a
    // link label intact. Link destinations live in `style.link`, not `text`.
    runs.iter().map(|run| run.text.as_str()).collect()
}

fn push(out: &mut Vec<Segment>, message: &str, part: &str, block: &[usize], text: String) {
    if text.trim().is_empty() {
        return;
    }
    out.push(Segment {
        message: message.to_owned(),
        part: part.to_owned(),
        block: block.to_vec(),
        folded: fold(&text),
        text,
    });
}

fn project_block(
    out: &mut Vec<Segment>,
    message: &str,
    part: &str,
    path: &mut Vec<usize>,
    block: &Block,
) {
    match block {
        Block::Paragraph { runs } | Block::Heading { runs, .. } => {
            push(out, message, part, path, inline_text(runs))
        }
        Block::CodeBlock { code, .. } => push(out, message, part, path, code.clone()),
        Block::BlockQuote { children } => {
            for (index, child) in children.iter().enumerate() {
                path.push(index);
                project_block(out, message, part, path, child);
                path.pop();
            }
        }
        Block::List { items, .. } => {
            for (item_index, item) in items.iter().enumerate() {
                for (index, child) in item.iter().enumerate() {
                    path.extend([item_index, index]);
                    project_block(out, message, part, path, child);
                    path.truncate(path.len() - 2);
                }
            }
        }
        Block::Table { header, rows, .. } => {
            for (row_index, row) in std::iter::once(header).chain(rows.iter()).enumerate() {
                for (column, cell) in row.iter().enumerate() {
                    path.extend([row_index, column]);
                    push(out, message, part, path, inline_text(cell));
                    path.truncate(path.len() - 2);
                }
            }
        }
        Block::Rule => {}
    }
}

/// Visible text only: user/assistant Markdown and visible error text.
/// Reasoning, tool calls/results, questions/answers and images are excluded.
fn project_entry(out: &mut Vec<Segment>, entry: &SessionMessageEntry) {
    for part in &entry.parts {
        match part {
            MessagePart::Text { id, text } => {
                for (index, top) in parse_full(text).blocks.iter().enumerate() {
                    let mut path = vec![index];
                    project_block(out, &entry.id, id, &mut path, &top.block);
                }
            }
            MessagePart::Error { id, message } => push(out, &entry.id, id, &[0], message.clone()),
            MessagePart::Reasoning { .. }
            | MessagePart::Tool { .. }
            | MessagePart::Input { .. }
            | MessagePart::AsyncQuestion { .. }
            | MessagePart::Image { .. } => {}
        }
    }
}

fn project(entries: &[SessionMessageEntry]) -> Vec<Segment> {
    let mut out = Vec::new();
    for entry in entries {
        project_entry(&mut out, entry);
    }
    out
}

struct Query {
    text: String,
    folded: String,
    case_sensitive: bool,
}

impl Query {
    fn new(text: &str, case_sensitive: bool) -> Option<Self> {
        (!text.trim().is_empty()).then(|| Self {
            text: text.to_owned(),
            folded: fold(text).text,
            case_sensitive,
        })
    }
}

fn find_in(segment: &Segment, query: &Query, out: &mut Vec<Anchor>) {
    let anchor = |start: usize, end: usize| Anchor {
        message: segment.message.clone(),
        part: segment.part.clone(),
        block: segment.block.clone(),
        start,
        end,
    };
    if query.case_sensitive {
        for (start, matched) in segment.text.match_indices(query.text.as_str()) {
            out.push(anchor(start, start + matched.len()));
        }
        return;
    }
    let folded = &segment.folded;
    for (start, matched) in folded.text.match_indices(query.folded.as_str()) {
        let end = start + matched.len();
        // Map back through the offset table, widening to whole source chars.
        let source_start = folded.map[start];
        let last = folded.map[end - 1];
        let width = segment.text[last..]
            .chars()
            .next()
            .map_or(0, char::len_utf8);
        out.push(anchor(source_start, last + width));
    }
}

fn find(segments: &[Segment], query: Option<&Query>) -> Vec<Anchor> {
    let mut out = Vec::new();
    if let Some(query) = query {
        for segment in segments {
            find_in(segment, query, &mut out);
        }
    }
    out
}

fn slice<'a>(segments: &'a [Segment], anchor: &Anchor) -> Option<&'a str> {
    segments
        .iter()
        .find(|s| s.message == anchor.message && s.part == anchor.part && s.block == anchor.block)
        .and_then(|s| s.text.get(anchor.start..anchor.end))
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as usize
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.next() % items.len()]
    }
}

const WORDS: &[&str] = &[
    "search",
    "transcript",
    "virtualized",
    "rows",
    "anchor",
    "streaming",
    "markdown",
    "render",
    "Straße",
    "naïve",
    "café",
    "résumé",
    "検索",
    "東京",
    "🚀",
    "Ωmega",
    "provider",
    "session",
    "highlight",
    "history",
    "scroll",
    "native",
    "budget",
    "fold",
    "unicode",
    "offset",
    "range",
];

fn sentence(rng: &mut Lcg, words: usize) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(words);
    for _ in 0..words {
        out.push(rng.pick(WORDS));
    }
    let mut text = out.join(" ");
    text.push('.');
    text
}

fn generated(rng: &mut Lcg, index: usize, target: usize) -> String {
    let mut text = String::new();
    let mut block = 0;
    while text.len() < target {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        let piece = match (index + block) % 7 {
            0 => format!(
                "{} **{}** _{}_ `{}` {}",
                sentence(rng, 6),
                rng.pick(WORDS),
                rng.pick(WORDS),
                rng.pick(WORDS),
                sentence(rng, 8)
            ),
            1 => format!(
                "- {}\n- {}\n- {}",
                sentence(rng, 5),
                sentence(rng, 5),
                sentence(rng, 5)
            ),
            2 => format!(
                "```rust\nfn row_{index}() {{\n    let {} = \"{}\";\n}}\n```",
                "value",
                sentence(rng, 4)
            ),
            3 => format!(
                "| {} | {} |\n| --- | --- |\n| {} | {} |",
                rng.pick(WORDS),
                rng.pick(WORDS),
                sentence(rng, 3),
                sentence(rng, 3)
            ),
            4 => format!("> {}", sentence(rng, 12)),
            5 => format!(
                "## {}\n\n{} [{}](https://example.invalid/{index}/{block}) {}",
                sentence(rng, 3),
                sentence(rng, 6),
                rng.pick(WORDS),
                sentence(rng, 6)
            ),
            _ => format!(
                "{} {} {}",
                sentence(rng, 10),
                sentence(rng, 10),
                sentence(rng, 10)
            ),
        };
        text.push_str(&piece);
        block += 1;
    }
    text
}

/// Probe fixtures at fixed entries; the generator never produces them.
fn probe_text(index: usize) -> Option<&'static str> {
    Some(match index {
        0 => "ZEPHYR-OLDEST-MARKER is in the very first message.",
        17 => "The **quick** brown fox jumps.",
        18 => "This is the end of alpha.\n\nOmega starts a new paragraph.",
        19 => "This message ends with crossmsg-left",
        20 => "crossmsg-right begins this message.",
        21 => "See [Release notes](https://hidden.example/token-XYZ) for details.",
        22 => "\u{212A}ELVIN scale readings.",
        23 => "GROẞE Straße and 🚀 launch and 検索テスト.",
        24 => "```rust\nfn main() {\n    println!(\"ic0\");\n}\n```",
        25 => "aaaaaa",
        26 => "Quick quick QUICK",
        27 => "İstanbul and ISTANBUL.",
        5000 => "ANCHOR-STABLE-MARKER sits in the middle of history.",
        _ => return None,
    })
}

fn entry(index: usize, rng: &mut Lcg) -> SessionMessageEntry {
    let role = if index % 2 == 0 {
        MessageRole::User
    } else {
        MessageRole::Assistant
    };
    let per_entry = TARGET_BYTES / ENTRIES;
    let mut text = generated(rng, index, per_entry);
    if let Some(probe) = probe_text(index) {
        text = format!("{probe}\n\n{text}");
    }
    let mut parts = vec![MessagePart::Text {
        id: "p0".into(),
        text,
    }];
    if role == MessageRole::Assistant && index % 50 == 1 {
        parts.insert(
            0,
            MessagePart::Reasoning {
                id: "r0".into(),
                text: format!("SECRET-REASONING-MARKER {index}"),
            },
        );
    }
    if role == MessageRole::Assistant && index % 40 == 1 {
        parts.push(MessagePart::Tool {
            id: "t0".into(),
            call: luna_proto::ToolCall::Mcp {
                server: "fixture".into(),
                tool: "lookup".into(),
                input: Some(json!({"secret": "PRIVATE-TOOL-INPUT"})),
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
        });
    }
    SessionMessageEntry {
        id: format!("m{index:05}"),
        role,
        parts,
        created_at: 1_788_900_000_000 + index as i64,
        device_id: "local".into(),
        status: (role == MessageRole::Assistant).then_some(MessageStatus::Complete),
        continuation_of: None,
    }
}

fn ms(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 1000. * 100.).round() / 100.
}

struct Checks(Vec<Value>);

impl Checks {
    fn check(&mut self, id: &str, pass: bool, detail: Value) {
        eprintln!("IC-0 find {} {id}", if pass { "PASS" } else { "FAIL" });
        self.0
            .push(json!({"id": id, "pass": pass, "detail": detail}));
    }
}

fn in_message<'a>(anchors: &'a [Anchor], message: &str) -> Vec<&'a Anchor> {
    anchors.iter().filter(|a| a.message == message).collect()
}

fn main() -> Result<()> {
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "/tmp/luna-ic0-find".into()),
    );
    std::fs::create_dir_all(&output)?;
    let mut checks = Checks(Vec::new());
    let mut timings = serde_json::Map::new();

    // Representative history through the doc's ordinary writer.
    let mut rng = Lcg(0x1c0_f1d);
    let started = Instant::now();
    let doc = SessionDoc::init("ic0-find")?;
    let mut text_bytes = 0usize;
    for index in 0..ENTRIES {
        let entry = entry(index, &mut rng);
        text_bytes += entry.parts.iter().map(MessagePart::byte_len).sum::<usize>();
        doc.push_message(&entry)?;
    }
    timings.insert("buildDocMs".into(), json!(ms(started)));
    let started = Instant::now();
    let snapshot = doc.export_snapshot()?;
    let reopened = SessionDoc::from_snapshot(&snapshot)?;
    let entries = luna_doc::join_continuation_entries(reopened.read_entries()?);
    timings.insert("reopenAndReadMs".into(), json!(ms(started)));
    checks.check(
        "history.complete",
        entries.len() == ENTRIES
            && entries.first().is_some_and(|e| e.id == "m00000")
            && entries
                .last()
                .is_some_and(|e| e.id == format!("m{:05}", ENTRIES - 1)),
        json!({"entries": entries.len(), "partBytes": text_bytes, "snapshotBytes": snapshot.len()}),
    );

    let started = Instant::now();
    let mut segments = project(&entries);
    timings.insert("projectionMs".into(), json!(ms(started)));
    let index_bytes: usize = segments
        .iter()
        .map(|s| {
            s.text.len()
                + s.folded.text.len()
                + s.folded.map.len() * std::mem::size_of::<usize>()
                + s.message.len()
                + s.part.len()
        })
        .sum();

    let q = |text: &str| Query::new(text, false);
    // F1 phrase across emphasis runs.
    let hits = find(&segments, q("quick brown").as_ref());
    checks.check(
        "F1.phrase-across-emphasis",
        in_message(&hits, "m00017").len() == 1,
        json!(hits.len()),
    );
    // F2 no match across blocks or messages.
    let across_blocks = find(&segments, q("alpha. omega").as_ref());
    let across_messages = find(&segments, q("crossmsg-left crossmsg-right").as_ref());
    checks.check(
        "F2.no-cross-block-or-message",
        across_blocks.is_empty() && across_messages.is_empty(),
        json!({"blocks": across_blocks.len(), "messages": across_messages.len()}),
    );
    // F3 link destinations are not searchable; labels are.
    let destination = find(&segments, q("hidden.example").as_ref());
    let label = find(&segments, q("release notes").as_ref());
    checks.check(
        "F3.link-destination-excluded",
        destination.is_empty() && in_message(&label, "m00021").len() == 1,
        json!({"destination": destination.len(), "label": label.len()}),
    );
    // F4 offsets map back through a folding that changes byte length.
    let kelvin = find(&segments, q("kelvin").as_ref());
    let kelvin_slice = kelvin
        .first()
        .and_then(|a| slice(&segments, a))
        .map(str::to_owned);
    let naive = kelvin.first().and_then(|a| {
        let segment = segments
            .iter()
            .find(|s| s.message == a.message && s.block == a.block)?;
        let lowered = segment.text.to_lowercase();
        let start = lowered.find("kelvin")?;
        segment.text.get(start..start + 6).map(str::to_owned)
    });
    checks.check(
        "F4.unicode-offset-map",
        kelvin_slice.as_deref() == Some("\u{212A}ELVIN"),
        json!({"mapped": kelvin_slice, "naiveLowercaseOffsets": naive}),
    );
    // F5 Unicode text: sharp s capital, emoji, CJK.
    let sharp = find(&segments, q("große").as_ref());
    let sharp_slice = in_message(&sharp, "m00023")
        .first()
        .and_then(|a| slice(&segments, a))
        .map(str::to_owned);
    let emoji = in_message(&find(&segments, q("🚀 launch").as_ref()), "m00023").len();
    let cjk = in_message(&find(&segments, q("検索テスト").as_ref()), "m00023").len();
    checks.check(
        "F5.unicode-sharp-s-emoji-cjk",
        sharp_slice.as_deref() == Some("GROẞE") && emoji == 1 && cjk == 1,
        json!({"sharpS": sharp_slice, "emoji": emoji, "cjk": cjk}),
    );
    // F6 multiline code inside one code block.
    let code = in_message(&find(&segments, q("{\n    println").as_ref()), "m00024").len();
    checks.check("F6.multiline-code", code == 1, json!(code));
    // F7 private and hidden content is excluded.
    let private: Vec<usize> = [
        "SECRET-REASONING-MARKER",
        "PRIVATE-TOOL-INPUT",
        "PRIVATE-TOOL-OUTPUT",
    ]
    .iter()
    .map(|marker| find(&segments, q(marker).as_ref()).len())
    .collect();
    checks.check(
        "F7.private-content-excluded",
        private.iter().all(|count| *count == 0),
        json!(private),
    );
    // F8 whitespace-only queries match nothing.
    checks.check(
        "F8.whitespace-query",
        find(&segments, q("   ").as_ref()).is_empty() && q(" \n\t").is_none(),
        Value::Null,
    );
    // F9 the oldest entry is reachable by stable identity.
    let oldest = find(&segments, q("ZEPHYR-OLDEST-MARKER").as_ref());
    checks.check(
        "F9.oldest-entry-reachable",
        oldest.len() == 1 && oldest[0].message == "m00000",
        json!(oldest.first().map(|a| (&a.message, &a.part, &a.block))),
    );
    // F10 literal non-overlapping matches.
    let overlap = in_message(&find(&segments, q("aaa").as_ref()), "m00025").len();
    checks.check("F10.non-overlapping", overlap == 2, json!(overlap));
    // F11 case-sensitive toggle.
    let sensitive = in_message(
        &find(&segments, Query::new("Quick", true).as_ref()),
        "m00026",
    )
    .len();
    let insensitive = in_message(&find(&segments, q("Quick").as_ref()), "m00026").len();
    checks.check(
        "F11.case-toggle",
        sensitive == 1 && insensitive == 3,
        json!({"sensitive": sensitive, "insensitive": insensitive}),
    );

    // Timings over the complete history (release build recommended).
    let rare = q("ZEPHYR-OLDEST-MARKER");
    let started = Instant::now();
    let _ = find(&segments, rare.as_ref());
    timings.insert("rareFullScanMs".into(), json!(ms(started)));
    let started = Instant::now();
    let mut newest_first = None;
    for segment in segments.iter().rev() {
        let mut out = Vec::new();
        find_in(segment, rare.as_ref().expect("query"), &mut out);
        if !out.is_empty() {
            newest_first = Some(ms(started));
            break;
        }
    }
    timings.insert(
        "rareOldestFirstMatchNewestFirstScanMs".into(),
        json!(newest_first),
    );
    let common = q("search");
    let started = Instant::now();
    let mut first_common = None;
    let mut total = 0usize;
    let mut max_chunk = 0f64;
    for chunk in segments.chunks(CHUNK_SEGMENTS) {
        let chunk_started = Instant::now();
        let mut out = Vec::new();
        for segment in chunk {
            find_in(segment, common.as_ref().expect("query"), &mut out);
        }
        if first_common.is_none() && !out.is_empty() {
            first_common = Some(ms(started));
        }
        total += out.len();
        max_chunk = max_chunk.max(ms(chunk_started));
    }
    timings.insert("commonFirstMatchMs".into(), json!(first_common));
    timings.insert("commonFullCountMs".into(), json!(ms(started)));
    timings.insert("commonMatches".into(), json!(total));
    timings.insert("maxChunkMs".into(), json!(max_chunk));
    timings.insert("chunkSegments".into(), json!(CHUNK_SEGMENTS));
    let worst_first = newest_first
        .unwrap_or(f64::MAX)
        .max(first_common.unwrap_or(f64::MAX));

    // F12 streaming and rollback keep the active match's identity.
    let active = find(&segments, q("ANCHOR-STABLE-MARKER").as_ref());
    let mut tail = entries.last().cloned().expect("entries");
    let mut max_update = 0f64;
    for chunk in 0..20 {
        if let Some(MessagePart::Text { text, .. }) = tail
            .parts
            .iter_mut()
            .find(|p| matches!(p, MessagePart::Text { .. }))
        {
            text.push_str(&format!(
                "\n\nSTREAM-TAIL chunk {chunk} {}",
                "streamed words ".repeat(8)
            ));
        }
        let started = Instant::now();
        segments.retain(|s| s.message != tail.id);
        project_entry(&mut segments, &tail);
        max_update = max_update.max(ms(started));
    }
    let streamed = find(&segments, q("STREAM-TAIL").as_ref()).len();
    let after_stream = find(&segments, q("ANCHOR-STABLE-MARKER").as_ref());
    let removed: Vec<String> = entries
        .iter()
        .rev()
        .take(10)
        .map(|e| e.id.clone())
        .collect();
    segments.retain(|s| !removed.contains(&s.message));
    let after_rollback = find(&segments, q("ANCHOR-STABLE-MARKER").as_ref());
    let rolled_back = find(&segments, q("STREAM-TAIL").as_ref()).len();
    checks.check("F12.stream-and-rollback-stable-anchor",
        active.len() == 1 && streamed == 20 && after_stream == active && after_rollback == active && rolled_back == 0,
        json!({"active": active.first().map(|a| (&a.message, &a.block, a.start)), "streamed": streamed, "afterRollback": rolled_back}));
    timings.insert("maxStreamingUpdateMs".into(), json!(max_update));

    // F13 observation: lowercase folding does not equate İ with I (Turkish
    // dotted I folds to i + U+0307); IC-5 decides whether that is wanted.
    let dotted = in_message(&find(&segments, q("istanbul").as_ref()), "m00027").len();

    let hardware = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.model", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .replace('\n', " / ")
        });
    let failed: Vec<String> = checks
        .0
        .iter()
        .filter(|c| c["pass"] != json!(true))
        .filter_map(|c| c["id"].as_str().map(str::to_owned))
        .collect();
    let record = json!({
        "evidenceKind": "ic0-find-feasibility",
        "passed": failed.is_empty(),
        "failed": failed,
        "environment": {"hardware": hardware, "debugBuild": cfg!(debug_assertions), "os": std::env::consts::OS},
        "fixture": {"entries": ENTRIES, "targetBytes": TARGET_BYTES, "segments": segments.len(), "indexBytes": index_bytes},
        "timings": timings,
        "budget": {"firstMatchMs": FIRST_MATCH_BUDGET_MS, "frameMs": FRAME_BUDGET_MS,
                   "worstFirstMatchMs": worst_first, "withinProvisionalBudget": worst_first <= FIRST_MATCH_BUDGET_MS && max_chunk <= FRAME_BUDGET_MS},
        "observations": {"turkishDottedIMatchesForLowercaseQuery": dotted},
        "checks": checks.0,
    });
    std::fs::write(
        output.join("result.json"),
        serde_json::to_string_pretty(&record)?,
    )?;
    let passed = record["passed"] == json!(true);
    println!(
        "{}",
        json!({"passed": passed, "result": output.join("result.json")})
    );
    anyhow::ensure!(
        passed,
        "find feasibility failure probes failed: {}",
        record["failed"]
    );
    Ok(())
}
