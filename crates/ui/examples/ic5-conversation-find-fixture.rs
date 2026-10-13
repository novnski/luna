//! IC-5 native Find evidence (P01 plus S01-S04 captures): the production
//! `Transcript` rendered by the real headless GPUI platform (real text
//! system), over a representative conversation fed through the doc watch's
//! frame path (`AppState::apply_transcript_frame`).
//! Failure contract and criteria: docs/pending-tasks/interactive-chat-surfaces/ic-5/README.md.
//! Synthetic inputs; no engine, provider, network, keychain or user data.
//!
//!     scripts/cargo.sh build --locked --release -p luna-ui --features appshots-fixture \
//!       --example ic5-conversation-find-fixture
//!     target/release/examples/ic5-conversation-find-fixture <out-dir> \
//!       [--entries 10000] [--step-timeout-secs 120]
//!
//! The release run with the default 10,000 entries (~10 MiB) is the P01
//! measurement; a debug build checks correctness with e.g. `--entries 1000`.
//! Every step has a deadline: a step that misses it records
//! `TIMEOUT <step>` with what it was waiting for, the run continues and
//! exits non-zero. A progress line prints at least every 10 s while a step
//! waits. Frames are driven explicitly (draw, then the next-frame callbacks
//! the headless platform does not deliver on its own).
//!
//! Writes <out-dir>/result.json and one PNG per capture; exits non-zero when
//! a check fails or times out. Timings are measurements against the
//! provisional budget (first match <= 150 ms, no UI-thread merge or frame
//! over 16 ms), recorded with the hardware; they are not gates.
use gpui::{
    AnyWindowHandle, AppContext, AsyncApp, Bounds, Context, Entity, IntoElement, Render, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use luna_doc::{
    MessagePart, MessageRole, MessageStatus, SessionMessageEntry, TextAppend, TranscriptFrame,
};
use luna_ui::{
    appearance, icons, settings, state::AppState, theme, theme_library, transcript::Transcript,
    typography,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const CHAT: &str = "ic5-find-fixture";
const DEFAULT_ENTRIES: usize = 10_000;
/// ~10 MiB at the default 10,000 entries.
const ENTRY_BYTES: usize = 1_048;
const MIN_ENTRIES: usize = 64;
const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(120);
const PROGRESS_EVERY: Duration = Duration::from_secs(10);
/// While a pass runs, sample one frame this often (a frame per poll would
/// starve the merges the pass needs on the same thread).
const FRAME_SAMPLE_EVERY: Duration = Duration::from_millis(100);
const POLL: Duration = Duration::from_millis(5);
const FIRST_MATCH_BUDGET_MS: f64 = 150.0;
const FRAME_BUDGET_MS: f64 = 16.0;

struct Config {
    output: PathBuf,
    entries: usize,
    step_timeout: Duration,
}

fn config() -> anyhow::Result<Config> {
    let mut args = std::env::args().skip(1);
    let mut output = None;
    let mut entries = DEFAULT_ENTRIES;
    let mut step_timeout = DEFAULT_STEP_TIMEOUT;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--entries" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--entries needs N"))?;
                entries = value.parse::<usize>()?.max(MIN_ENTRIES);
            }
            "--step-timeout-secs" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--step-timeout-secs needs N"))?;
                step_timeout = Duration::from_secs(value.parse::<u64>()?.max(1));
            }
            _ if output.is_none() => output = Some(PathBuf::from(&arg)),
            other => anyhow::bail!("unexpected argument {other}"),
        }
    }
    Ok(Config {
        output: output.ok_or_else(|| {
            anyhow::anyhow!("usage: <out-dir> [--entries N] [--step-timeout-secs N]")
        })?,
        entries,
        step_timeout,
    })
}

struct Root {
    transcript: Entity<Transcript>,
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(typography::font_size(cx).pixels()));
        let theme = theme::Theme::of(cx);
        div()
            .size_full()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .child(self.transcript.clone())
    }
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
    let mut text = (0..words)
        .map(|_| rng.pick(WORDS))
        .collect::<Vec<_>>()
        .join(" ");
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
        let piece = match (index + block) % 6 {
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
                "```rust\nfn row_{index}() {{\n    let value = \"{}\";\n}}\n```",
                sentence(rng, 4)
            ),
            3 => format!(
                "| {} | {} |\n| --- | --- |\n| {} | {} |",
                rng.pick(WORDS),
                rng.pick(WORDS),
                sentence(rng, 3),
                sentence(rng, 3)
            ),
            4 => format!(
                "## {}\n\n{} [{}](https://example.invalid/{index}/{block}) {}",
                sentence(rng, 3),
                sentence(rng, 6),
                rng.pick(WORDS),
                sentence(rng, 6)
            ),
            _ => format!("> {} {}", sentence(rng, 10), sentence(rng, 10)),
        };
        text.push_str(&piece);
        block += 1;
    }
    text
}

fn middle(entries: usize) -> usize {
    entries / 2
}

/// Probes at fixed entries; the generator never produces their markers.
fn probe(index: usize, entries: usize) -> Option<&'static str> {
    match index {
        0 => Some("ZEPHYR-OLDEST-MARKER is in the very first message."),
        17 => Some(
            "The **quick** brown fox jumps over [Release notes](https://hidden.example/token-XYZ).",
        ),
        22 => Some("\u{212A}ELVIN scale readings and GROẞE Straße."),
        ix if ix == middle(entries) => Some("ANCHOR-STABLE-MARKER sits in the middle of history."),
        _ => None,
    }
}

fn entry(index: usize, entries: usize, rng: &mut Lcg) -> SessionMessageEntry {
    let role = if index.is_multiple_of(2) {
        MessageRole::User
    } else {
        MessageRole::Assistant
    };
    let mut text = generated(rng, index, ENTRY_BYTES);
    if let Some(probe) = probe(index, entries) {
        text = format!("{probe}\n\n{text}");
    }
    let mut parts = vec![MessagePart::Text {
        id: "p0".into(),
        text,
    }];
    if role == MessageRole::Assistant && index % 40 == 1 {
        parts.insert(
            0,
            MessagePart::Reasoning {
                id: "r0".into(),
                text: format!("SECRET-REASONING-MARKER {index}"),
            },
        );
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
    // The newest entry is an assistant reply still streaming (S03).
    let role = if index + 1 == entries {
        MessageRole::Assistant
    } else {
        role
    };
    SessionMessageEntry {
        id: format!("m{index:05}"),
        role,
        parts,
        created_at: 1_790_000_000_000 + index as i64,
        device_id: "local".into(),
        status: (role == MessageRole::Assistant).then_some(if index + 1 == entries {
            MessageStatus::Streaming
        } else {
            MessageStatus::Complete
        }),
        continuation_of: None,
    }
}

fn ms(started: Instant) -> f64 {
    (started.elapsed().as_secs_f64() * 100_000.0).round() / 100.0
}

/// One explicit frame: draw, then deliver the next-frame callbacks (the
/// Find bar's select-all and its post-layout reveal correction).
fn frame(window: AnyWindowHandle, cx: &mut AsyncApp) -> anyhow::Result<f64> {
    let started = Instant::now();
    window.update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window.simulate_next_frame(cx);
    })?;
    Ok(ms(started))
}

fn readback(transcript: &Entity<Transcript>, cx: &mut AsyncApp) -> Value {
    cx.update(|cx| transcript.update(cx, |transcript, _| transcript.fixture_find_readback()))
}

struct Run {
    window: AnyWindowHandle,
    transcript: Entity<Transcript>,
    output: PathBuf,
    step_timeout: Duration,
    checks: Vec<Value>,
}

impl Run {
    fn check(&mut self, id: &str, pass: bool, detail: Value) {
        eprintln!("IC-5 find {} {id}", if pass { "PASS" } else { "FAIL" });
        self.checks
            .push(json!({"id": id, "pass": pass, "detail": detail}));
    }

    /// Wait (bounded) for the current pass to finish, sampling frames while
    /// it runs, then drive a few frames for the reveal and paint. A missed
    /// deadline records `TIMEOUT <step>` and returns the last state.
    async fn settle(&mut self, step: &str, cx: &mut AsyncApp) -> anyhow::Result<Value> {
        let started = Instant::now();
        let mut last_progress = Instant::now();
        let mut last_frame: Option<Instant> = None;
        let mut max_frame_ms = 0f64;
        let mut frames = 0usize;
        // Attribution: the first frame drawn once an active match exists is
        // the reveal (scroll to its row, lay out rows never measured there).
        let mut reveal_frame_ms: Option<f64> = None;
        let mut max_step_frame_ms = 0f64;
        let mut state = readback(&self.transcript, cx);
        let mut timed_out = false;
        while state["searching"] == true {
            if started.elapsed() >= self.step_timeout {
                timed_out = true;
                break;
            }
            if last_frame.is_none_or(|at| at.elapsed() >= FRAME_SAMPLE_EVERY) {
                let had_active = !state["active"].is_null();
                let frame_ms = frame(self.window, cx)?;
                max_frame_ms = max_frame_ms.max(frame_ms);
                max_step_frame_ms = max_step_frame_ms.max(frame_ms);
                if had_active && reveal_frame_ms.is_none() {
                    reveal_frame_ms = Some(frame_ms);
                }
                frames += 1;
                last_frame = Some(Instant::now());
            }
            if last_progress.elapsed() >= PROGRESS_EVERY {
                eprintln!(
                    "IC-5 find waiting {step}: {:.0}s, searching, {} matches so far, {} chunks",
                    started.elapsed().as_secs_f64(),
                    state["total"],
                    state["stats"]["chunks"],
                );
                last_progress = Instant::now();
            }
            cx.background_executor().timer(POLL).await;
            state = readback(&self.transcript, cx);
        }
        for _ in 0..3 {
            let had_active = !readback(&self.transcript, cx)["active"].is_null();
            let frame_ms = frame(self.window, cx)?;
            max_step_frame_ms = max_step_frame_ms.max(frame_ms);
            if had_active && reveal_frame_ms.is_none() {
                reveal_frame_ms = Some(frame_ms);
            }
            cx.background_executor().timer(POLL).await;
        }
        let mut state = readback(&self.transcript, cx);
        if timed_out {
            let waited = started.elapsed().as_secs_f64();
            eprintln!("IC-5 find TIMEOUT {step} after {waited:.0}s");
            self.check(
                &format!("TIMEOUT {step}"),
                false,
                json!({
                    "waitingFor": "the Find pass to finish (searching == true)",
                    "elapsedS": waited,
                    "deadlineS": self.step_timeout.as_secs(),
                    "last": state.clone(),
                }),
            );
        }
        state["stepMs"] = json!(ms(started));
        state["framesWhileSearching"] = json!(frames);
        state["maxFrameMsWhileSearching"] = json!(max_frame_ms);
        // Every frame of the step, the settle and reveal frames included.
        state["maxFrameMs"] = json!(max_step_frame_ms);
        state["revealFrameMs"] = json!(reveal_frame_ms);
        state["timedOut"] = json!(timed_out);
        Ok(state)
    }

    async fn find(&mut self, step: &str, query: &str, cx: &mut AsyncApp) -> anyhow::Result<Value> {
        eprintln!("IC-5 find step {step}: {query:?}");
        let transcript = self.transcript.clone();
        let opened = self.window.update(cx, |_, window, cx| {
            transcript.update(cx, |transcript, cx| {
                transcript.fixture_find(query, window, cx)
            })
        })?;
        anyhow::ensure!(opened, "Find did not open for the selected chat");
        let mut state = self.settle(step, cx).await?;
        state["query"] = json!(query);
        Ok(state)
    }

    fn capture(&self, cx: &mut AsyncApp, label: &str) -> anyhow::Result<()> {
        let path = self.output.join(format!("{label}.png"));
        self.window.update(cx, |_, window, cx| {
            window.draw(cx).clear();
            window.render_to_image()?.save(&path)?;
            Ok::<(), anyhow::Error>(())
        })??;
        Ok(())
    }
}

fn active_text(state: &Value) -> Option<&str> {
    state["active"]["text"].as_str()
}

fn active_entry(state: &Value) -> Option<&str> {
    state["active"]["entryId"].as_str()
}

fn worst(states: &[&Value], pick: impl Fn(&Value) -> Option<f64>) -> f64 {
    states
        .iter()
        .filter_map(|state| pick(state))
        .fold(0f64, f64::max)
}

async fn scenarios(
    run: &mut Run,
    state: &Entity<AppState>,
    entries_wanted: usize,
    cx: &mut AsyncApp,
) -> anyhow::Result<Value> {
    let mut rng = Lcg(0x1c5_f1d);
    let started = Instant::now();
    let entries: Vec<SessionMessageEntry> = (0..entries_wanted)
        .map(|ix| entry(ix, entries_wanted, &mut rng))
        .collect();
    let bytes: usize = entries
        .iter()
        .flat_map(|entry| entry.parts.iter().map(MessagePart::byte_len))
        .sum();
    let generate_ms = ms(started);
    eprintln!("IC-5 find setup: {entries_wanted} entries, {bytes} bytes in {generate_ms} ms");
    let started = Instant::now();
    cx.update(|cx| {
        state.update(cx, |state, cx| {
            state
                .apply_transcript_frame(TranscriptFrame::reset(&entries))
                .expect("reset applies");
            cx.notify();
        })
    });
    frame(run.window, cx)?;
    let replay_ms = ms(started);
    eprintln!("IC-5 find setup: replay and first frame in {replay_ms} ms");

    // P01 + S01: a rare marker in the oldest entry, far above the mounted rows.
    let rare = run.find("S01.oldest", "zephyr-oldest-marker", cx).await?;
    run.capture(cx, "s01-oldest-revealed")?;
    run.check(
        "S01.oldest-offscreen-found-and-revealed",
        rare["total"] == 1
            && active_entry(&rare) == Some("m00000")
            && active_text(&rare) == Some("ZEPHYR-OLDEST-MARKER")
            && rare["scrollTopItem"] == 0
            && rare["historyComplete"] == true,
        rare.clone(),
    );

    // P01: a common word, every row matched.
    let common = run.find("P01.common", "search", cx).await?;
    run.capture(cx, "p01-common-highlights")?;
    run.check(
        "P01.common-query-complete",
        common["total"]
            .as_u64()
            .is_some_and(|total| total as usize > entries_wanted / 10)
            && common["searching"] == false,
        json!({"total": common["total"], "stats": common["stats"]}),
    );

    // S02: a phrase across emphasis, a link label (not its destination), a
    // case fold that changes byte length, and private content.
    let phrase = run.find("S02.phrase", "quick brown", cx).await?;
    run.capture(cx, "s02-phrase-across-emphasis")?;
    let label = run.find("S02.label", "release notes", cx).await?;
    let destination = run.find("S02.destination", "hidden.example", cx).await?;
    let kelvin = run.find("S02.kelvin", "kelvin", cx).await?;
    run.capture(cx, "s02-kelvin-fold")?;
    let private = run.find("S02.private", "private-tool-output", cx).await?;
    let reasoning = run
        .find("S02.reasoning", "secret-reasoning-marker", cx)
        .await?;
    run.check(
        "S02.display-text-and-unicode-mapping",
        active_text(&phrase) == Some("quick brown")
            && active_text(&label) == Some("Release notes")
            && destination["total"] == 0
            && active_text(&kelvin) == Some("\u{212A}ELVIN")
            && private["total"] == 0
            && reasoning["total"] == 0,
        json!({
            "phrase": phrase["active"], "label": label["active"],
            "destination": destination["total"], "kelvin": kelvin["active"],
            "privateToolOutput": private["total"], "reasoning": reasoning["total"],
        }),
    );

    // S03: streaming appends a later match; the active one keeps identity.
    let mid = format!("m{:05}", middle(entries_wanted));
    let anchored = run.find("S03.anchor", "anchor-stable-marker", cx).await?;
    let tail = entries.last().expect("entries");
    let MessagePart::Text { text, .. } = &tail.parts[0] else {
        anyhow::bail!("the tail entry starts with text");
    };
    let appended = "\n\nA streamed ANCHOR-STABLE-MARKER copy arrives.";
    let len = text.len() + appended.len();
    let tail_id = tail.id.clone();
    cx.update(|cx| {
        state.update(cx, |state, cx| {
            state
                .apply_transcript_frame(TranscriptFrame::Delta {
                    upsert: Vec::new(),
                    append: vec![TextAppend {
                        entry: tail_id,
                        part: "p0".into(),
                        text: appended.into(),
                        len,
                    }],
                    remove: Vec::new(),
                    count: entries_wanted,
                })
                .expect("append applies");
            cx.notify();
        })
    });
    eprintln!("IC-5 find step S03.stream: appended a match to the streaming tail");
    let streamed = run.settle("S03.stream", cx).await?;
    run.capture(cx, "s03-stream-keeps-anchor")?;
    run.check(
        "S03.stream-keeps-active-identity",
        active_entry(&anchored) == Some(mid.as_str())
            && active_entry(&streamed) == Some(mid.as_str())
            && streamed["total"] == 2,
        json!({"before": anchored["active"], "after": streamed["active"], "total": streamed["total"]}),
    );

    // S04: closing restores the tail-following viewport Find moved.
    let transcript = run.transcript.clone();
    run.window.update(cx, |_, window, cx| {
        transcript.update(cx, |transcript, cx| {
            transcript.fixture_close_find(window, cx)
        })
    })?;
    for _ in 0..3 {
        frame(run.window, cx)?;
    }
    let closed = readback(&run.transcript, cx);
    run.capture(cx, "s04-closed-restored")?;
    run.check(
        "S04.close-restores-search-moved-viewport",
        closed["open"] == false && closed["pinned"] == true,
        json!({"pinned": closed["pinned"], "scrollTopItem": closed["scrollTopItem"]}),
    );

    let firsts = [&rare, &common, &phrase, &kelvin, &anchored];
    let passes = [&rare, &common, &phrase, &kelvin, &anchored, &streamed];
    let worst_first = worst(&firsts, |state| state["stats"]["firstMatchMs"].as_f64());
    let worst_merge = worst(&passes, |state| state["stats"]["maxApplyMs"].as_f64());
    let worst_frame = worst(&passes, |state| state["maxFrameMsWhileSearching"].as_f64());
    // Stricter than frames-while-searching: also the reveal and settle frames.
    let worst_step_frame = worst(&passes, |state| state["maxFrameMs"].as_f64());
    let worst_reveal = worst(&firsts, |state| state["revealFrameMs"].as_f64());
    let hardware = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.model", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .replace('\n', " / ")
        });
    let failed: Vec<Value> = run
        .checks
        .iter()
        .filter(|check| check["pass"] != true)
        .map(|check| check["id"].clone())
        .collect();
    Ok(json!({
        "evidenceKind": "ic5-conversation-find-fixture",
        "passed": failed.is_empty(),
        "failed": failed,
        "environment": {
            "hardware": hardware,
            "debugBuild": cfg!(debug_assertions),
            "os": std::env::consts::OS,
        },
        "fixture": {
            "entries": entries_wanted,
            "partBytes": bytes,
            "rows": common["rows"],
            "stepTimeoutS": run.step_timeout.as_secs(),
            "generateMs": generate_ms,
            "replayAndFirstFrameMs": replay_ms,
        },
        "performance": {
            "rare": rare["stats"],
            "common": common["stats"],
            "streamingUpdate": streamed["stats"],
            "stepMs": passes.iter().map(|state| json!({"query": state["query"], "ms": state["stepMs"]})).collect::<Vec<_>>(),
            "worstFirstMatchMs": worst_first,
            "worstMergeMs": worst_merge,
            "worstFrameMsWhileSearching": worst_frame,
            "worstStepFrameMs": worst_step_frame,
            "worstRevealFrameMs": worst_reveal,
            "phases": passes.iter().map(|state| json!({
                "query": state["query"],
                "prepareMs": state["stats"]["prepareMs"],
                "firstChunkMs": state["stats"]["firstChunkMs"],
                "firstMatchMs": state["stats"]["firstMatchMs"],
                "completeMs": state["stats"]["completeMs"],
                "backgroundMs": state["stats"]["backgroundMs"],
                "maxChunkMs": state["stats"]["maxChunkMs"],
                "maxMergeMs": state["stats"]["maxApplyMs"],
                "revealFrameMs": state["revealFrameMs"],
                "maxFrameMs": state["maxFrameMs"],
            })).collect::<Vec<_>>(),
            "budget": {
                "firstMatchMs": FIRST_MATCH_BUDGET_MS,
                "frameMs": FRAME_BUDGET_MS,
                "measuredAtRepresentativeSize": entries_wanted >= DEFAULT_ENTRIES && !cfg!(debug_assertions),
                "withinProvisionalBudget": worst_first <= FIRST_MATCH_BUDGET_MS
                    && worst_merge <= FRAME_BUDGET_MS
                    && worst_frame <= FRAME_BUDGET_MS
                    && worst_step_frame <= FRAME_BUDGET_MS,
            },
        },
        "checks": run.checks,
    }))
}

fn main() -> anyhow::Result<()> {
    let config = config()?;
    std::fs::create_dir_all(&config.output)?;
    let profile = tempfile::tempdir()?;
    let data = profile.path().join("ui");
    std::fs::create_dir_all(&data)?;
    let failure = Arc::new(Mutex::new(None));
    let failure_for_ui = failure.clone();
    gpui_platform::headless()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let settings = settings::UiSettings::default();
            settings::init(settings.clone(), data.clone(), cx);
            let fonts = typography::register_fonts(cx);
            typography::init(
                settings.ui_font_family,
                settings.ui_font_size,
                settings.terminal_font_family,
                settings.terminal_font_size,
                settings.code_font_family,
                settings.code_font_size,
                fonts,
                cx,
            );
            theme_library::init(data, cx);
            appearance::init(
                appearance::AppearanceMode::Light,
                settings.theme_selection,
                settings.accent,
                settings.surface,
                cx,
            );
            luna_ui::composer::init(cx, settings.composer_send_behavior);
            let state = cx.new(|_| {
                let mut state = AppState::new();
                state.selected_chat = Some(CHAT.into());
                state
            });
            let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
            let root_transcript = transcript.clone();
            let window: AnyWindowHandle = cx
                .open_window(
                    WindowOptions {
                        window_background: gpui::WindowBackgroundAppearance::Opaque,
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(20.0), px(20.0)),
                            size(px(1100.0), px(900.0)),
                        ))),
                        show: false,
                        focus: false,
                        ..Default::default()
                    },
                    |_, cx| {
                        cx.new(|_| Root {
                            transcript: root_transcript,
                        })
                    },
                )
                .unwrap()
                .into();
            let Config {
                output,
                entries,
                step_timeout,
            } = config;
            cx.spawn(async move |cx| {
                let mut run = Run {
                    window,
                    transcript,
                    output: output.clone(),
                    step_timeout,
                    checks: Vec::new(),
                };
                let result = scenarios(&mut run, &state, entries, cx).await;
                match result {
                    Ok(record) => {
                        let passed = record["passed"] == true;
                        let path = output.join("result.json");
                        let written = serde_json::to_vec_pretty(&record)
                            .map_err(anyhow::Error::from)
                            .and_then(|bytes| std::fs::write(&path, bytes).map_err(Into::into));
                        if let Err(error) = written {
                            *failure_for_ui.lock().unwrap() = Some(error.to_string());
                        } else if !passed {
                            *failure_for_ui.lock().unwrap() =
                                Some(format!("IC-5 find checks failed: {}", record["failed"]));
                        }
                        println!("{}", json!({"passed": passed, "result": path}));
                    }
                    Err(error) => {
                        eprintln!("IC-5 find fixture failed: {error:#}");
                        // Keep whatever checks ran, so a failure still leaves evidence.
                        let partial = json!({
                            "evidenceKind": "ic5-conversation-find-fixture",
                            "passed": false,
                            "error": format!("{error:#}"),
                            "checks": run.checks,
                        });
                        let _ = serde_json::to_vec_pretty(&partial)
                            .map(|bytes| std::fs::write(output.join("result.json"), bytes));
                        *failure_for_ui.lock().unwrap() = Some(error.to_string());
                    }
                }
                let _ = window.update(cx, |_, window, _| window.remove_window());
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    if let Some(error) = failure.lock().unwrap().take() {
        anyhow::bail!(error);
    }
    Ok(())
}
