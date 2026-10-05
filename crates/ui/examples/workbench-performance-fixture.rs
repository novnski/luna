//! Native workbench performance baseline (harness evolution slice 6).
//!
//! A real macOS window (`gpui_platform::application()`, Metal) running the
//! production `Shell` against an isolated in-process `EngineCore` reached over
//! its real IPC socket (`serve_ipc` + `EngineHandle::bootstrap`). Mock provider
//! only, temp provider homes, in-memory auth. The fixture drives itself from
//! inside the app through GPUI event dispatch (typing uses
//! `dispatch_keystroke` with simulated IME) and quits; it is not external UI
//! automation. Counters come from `luna_ui::perf_counters`
//! (`LUNA_PERF_COUNTERS=1`, set here). See
//! docs/design/harness-evolution/slice-6-ui.md.
//!
//! Usage: workbench-performance-fixture ABSOLUTE_OUTPUT_DIR [ENTRIES]
//! (ENTRIES defaults to 1200). `LUNA_PERF_MODE=candidate` makes any failed
//! correctness check fail the run at the end; the default `baseline` mode
//! records failures and keeps measuring.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, AppContext, AsyncApp, Bounds, Keystroke, WindowBounds, WindowHandle,
    WindowOptions, px, size,
};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SegmentWriter, SessionMessageEntry};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
    run_journal::RunJournal,
};
use luna_harness::mock::MockHarness;
use luna_proto::{AgentEvent, DoneStatus, HarnessId};
use luna_ui::*;
use serde_json::{Value, json};

type Window = WindowHandle<shell::Shell>;

// Match the shipped macOS app; system malloc retains transient serialization
// pages and would measure an allocator the user never runs (memory-plan.md).
#[cfg(target_os = "macos")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

const PROJECT: &str = "perf-project";
/// The large chat every phase measures (200 large tool outputs).
const MAIN: &str = "a0a0a0a0-0000-4000-8000-000000000001";
/// Streams while hidden.
const HIDDEN: &str = "a0a0a0a0-0000-4000-8000-000000000002";
/// Pane chats (B also receives the ownership send).
const PANE_B: &str = "a0a0a0a0-0000-4000-8000-000000000003";
const PANE_C: &str = "a0a0a0a0-0000-4000-8000-000000000004";
const PANE_D: &str = "a0a0a0a0-0000-4000-8000-000000000005";
/// Same entry ids as Main, different (same-length) tail text.
const FORK: &str = "a0a0a0a0-0000-4000-8000-000000000006";
/// A small chat for navigation away from Main.
const OTHER: &str = "a0a0a0a0-0000-4000-8000-000000000007";
/// Child doc the spawn chip indexes.
const CHILD: &str = "a0a0a0a0-0000-4000-8000-000000000008";

const DEVICE_TAG: &str = "workbench-perf";
/// A valid 1x1 RGBA PNG: the disposable file for the drop-ownership case
/// (external drops stage image formats only).
const DROP_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
const OWN_PROMPT: &str = "perf ownership send";
const WIDTH: f32 = 1600.0;
const HEIGHT: f32 = 1000.0;

/// Root's history template (`workbench_performance_e2e::text_entry`).
fn text_entry(index: usize) -> SessionMessageEntry {
    SessionMessageEntry {
        id: format!("settled-{index:05}"),
        role: MessageRole::Assistant,
        parts: vec![MessagePart::Text {
            id: "text".into(),
            text: format!("History {index}: {}", "settled history text ".repeat(40)),
        }],
        created_at: index as i64,
        device_id: DEVICE_TAG.into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    }
}

fn assistant(id: &str, parts: Vec<MessagePart>, at: i64) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role: MessageRole::Assistant,
        parts,
        created_at: at,
        device_id: DEVICE_TAG.into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    }
}

fn part(value: Value) -> MessagePart {
    serde_json::from_value(value).expect("fixture part")
}

fn tool_part(id: &str, output: &str) -> MessagePart {
    part(json!({
        "kind": "tool", "id": id,
        "call": {"kind": "exec", "command": "fixture tail tool"},
        "resolved": true, "output": output,
    }))
}

fn spawn_part(id: &str, status: &str, result: &str, tokens: u64) -> MessagePart {
    part(json!({
        "kind": "tool", "id": id,
        "call": {"kind": "unknown", "name": "Agent", "input": {"description": "fixture child"}},
        "resolved": true,
        "subagentRef": CHILD,
        "subagentStatus": status,
        "subagentActivity": {"result": result, "outputTokens": tokens},
    }))
}

fn text_part(text: &str) -> MessagePart {
    MessagePart::Text {
        id: "text".into(),
        text: text.into(),
    }
}

/// The visible tail every check reads: a tool, a spawn chip and a text row,
/// each carrying an 8-letter marker a rewrite replaces at the same length.
fn tail(marker: &str, at: i64) -> Vec<SessionMessageEntry> {
    vec![
        assistant(
            "tail-tool",
            vec![tool_part("tail-tool-part", &format!("TOOL-{marker}"))],
            at,
        ),
        assistant(
            "tail-spawn",
            vec![spawn_part(
                "tail-spawn-part",
                "done",
                &format!("CHILD-{marker}"),
                7,
            )],
            at + 1,
        ),
        assistant(
            "tail-text",
            vec![text_part(&format!("TEXT-{marker} closes the history."))],
            at + 2,
        ),
    ]
}

/// The production rewind boundary: edit a detached copy, then import it into
/// the live doc in one step (never row-by-row on a watched doc).
fn replace_atomically(
    doc: &luna_doc::SessionDoc,
    entries: &[SessionMessageEntry],
) -> anyhow::Result<()> {
    let detached = luna_doc::SessionDoc::from_snapshot(&doc.export_snapshot()?)?;
    detached.replace_conversation_entries(entries, &HashMap::new())?;
    doc.doc().import(&detached.export_snapshot()?)?;
    Ok(())
}

/// The fixture's Mock provider: unchanged except the ownership send, which
/// reports `SessionStarted` like a real native peer so its acceptance is
/// durably correlated.
struct FixtureMock {
    inner: MockHarness,
}

#[async_trait::async_trait]
impl luna_harness::Harness for FixtureMock {
    fn id(&self) -> HarnessId {
        self.inner.id()
    }
    fn display_name(&self) -> &str {
        self.inner.display_name()
    }
    fn supports_steering(&self) -> bool {
        self.inner.supports_steering()
    }
    fn steering_mode(&self) -> luna_proto::SteeringMode {
        self.inner.steering_mode()
    }
    fn reasoning_levels(&self) -> &[luna_proto::ReasoningLevel] {
        self.inner.reasoning_levels()
    }
    fn supports_safe_recovery_continuation(&self) -> bool {
        self.inner.supports_safe_recovery_continuation()
    }
    async fn models(&self) -> Result<Vec<luna_proto::Model>, luna_harness::HarnessError> {
        self.inner.models().await
    }
    async fn compact_context(
        &self,
        request: luna_harness::ContextCompactRequest,
    ) -> Result<(), luna_harness::HarnessError> {
        self.inner.compact_context(request).await
    }
    async fn run_title(
        &self,
        request: luna_proto::RunRequest,
        controls: luna_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<AgentEvent, luna_harness::HarnessError>>,
        luna_harness::HarnessError,
    > {
        self.inner.run_title(request, controls).await
    }
    async fn run(
        &self,
        request: luna_proto::RunRequest,
        controls: luna_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<AgentEvent, luna_harness::HarnessError>>,
        luna_harness::HarnessError,
    > {
        if !request.prompt.contains(OWN_PROMPT) {
            return self.inner.run(request, controls).await;
        }
        let session_id = "perf-native-session".to_string();
        let peer = MockHarness {
            script: vec![
                AgentEvent::SessionStarted {
                    harness: HarnessId::Mock,
                    model: request.model.clone().unwrap_or_else(|| "mock-1".into()),
                    tools: Vec::new(),
                    cwd: request.cwd.clone(),
                    session_id: session_id.clone(),
                    assistant_message_id: "perf-assistant-message".into(),
                },
                AgentEvent::TextDelta {
                    text: "Ownership send received.".into(),
                },
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: Some(session_id),
                },
            ],
        };
        peer.run(request, controls).await
    }
}

// ---- timing on the native frame loop ----

/// `on_next_frame` runs at the start of a platform frame, before `draw`; a
/// task spawned from it runs after that frame's draw and present return.
#[derive(Default, Clone, Copy)]
struct FrameMarks {
    start: Option<Instant>,
    presented: Option<Instant>,
    active: Option<bool>,
    /// The window's content size at that frame.
    viewport: Option<(f32, f32)>,
}

/// While the original measured phases run, every timed sample must be at
/// the measured window size; off-size samples are counted here (a user or
/// window manager resizing mid-benchmark must not contaminate results).
static MEASURING: AtomicBool = AtomicBool::new(true);
static OFF_SIZE_SAMPLES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn off_size(viewport: Option<(f32, f32)>) -> bool {
    viewport.is_none_or(|(w, h)| (w - WIDTH).abs() > 0.5 || (h - HEIGHT).abs() > 0.5)
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// Run `act` in the window, then time the next frame: input-to-frame-start,
/// frame-start-to-presented and input-to-presented, in milliseconds. Input
/// timings never force a redraw (`refresh: false`): only what the input
/// itself invalidates is drawn. Repaint probes refresh the whole window.
async fn timed(
    window: Window,
    cx: &mut AsyncApp,
    refresh: bool,
    act: impl FnOnce(&mut gpui::Window, &mut gpui::App) + 'static,
) -> anyhow::Result<Value> {
    let marks = Rc::new(RefCell::new(FrameMarks::default()));
    let probe = marks.clone();
    let t0 = Instant::now();
    AnyWindowHandle::from(window).update(cx, move |_, window, cx| {
        act(window, cx);
        if refresh {
            window.refresh();
        }
        window.on_next_frame(move |window, cx| {
            probe.borrow_mut().start = Some(Instant::now());
            probe.borrow_mut().active = Some(window.is_window_active());
            let content = window.viewport_size();
            probe.borrow_mut().viewport =
                Some((f32::from(content.width), f32::from(content.height)));
            perf_counters::next_frame();
            let done = probe.clone();
            cx.spawn(async move |_| {
                done.borrow_mut().presented = Some(Instant::now());
            })
            .detach();
        });
    })?;
    for _ in 0..2000 {
        if marks.borrow().presented.is_some() {
            break;
        }
        settle(cx, 1).await;
    }
    let marks = *marks.borrow();
    if MEASURING.load(Ordering::Relaxed) && off_size(marks.viewport) {
        OFF_SIZE_SAMPLES.fetch_add(1, Ordering::Relaxed);
    }
    let ms = |from: Instant, to: Option<Instant>| {
        to.map(|to| to.duration_since(from).as_secs_f64() * 1000.0)
    };
    Ok(json!({
        "inputToFrameStartMs": ms(t0, marks.start),
        "frameStartToPresentedMs": marks.start.and_then(|start| ms(start, marks.presented)),
        "inputToPresentedMs": ms(t0, marks.presented),
        "windowActive": marks.active,
        "viewport": marks.viewport.map(|(w, h)| [w, h]),
    }))
}

/// Frame-to-frame intervals of the native loop while a phase runs.
#[derive(Clone)]
struct FrameRecorder {
    active: Rc<RefCell<bool>>,
    intervals: Rc<RefCell<Vec<f64>>>,
}

impl FrameRecorder {
    fn start(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Self> {
        let recorder = Self {
            active: Rc::new(RefCell::new(true)),
            intervals: Rc::default(),
        };
        let this = recorder.clone();
        AnyWindowHandle::from(window).update(cx, move |_, window, _| {
            this.arm(window, None);
        })?;
        Ok(recorder)
    }

    fn arm(&self, window: &mut gpui::Window, last: Option<Instant>) {
        let this = self.clone();
        window.on_next_frame(move |window, _| {
            if !*this.active.borrow() {
                return;
            }
            let now = Instant::now();
            if let Some(last) = last {
                this.intervals
                    .borrow_mut()
                    .push(now.duration_since(last).as_secs_f64() * 1000.0);
            }
            this.arm(window, Some(now));
        });
    }

    fn stop(&self) -> Value {
        *self.active.borrow_mut() = false;
        summary(self.intervals.borrow().clone())
    }
}

fn summary(mut samples: Vec<f64>) -> Value {
    samples.retain(|v| v.is_finite());
    if samples.is_empty() {
        return json!({"count": 0});
    }
    samples.sort_by(|a, b| a.total_cmp(b));
    let pick = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    json!({
        "count": samples.len(),
        "p50": pick(0.5),
        "p95": pick(0.95),
        "max": samples[samples.len() - 1],
        "mean": samples.iter().sum::<f64>() / samples.len() as f64,
    })
}

fn timing_summary(samples: &[Value]) -> Value {
    let field = |name: &str| summary(samples.iter().filter_map(|v| v[name].as_f64()).collect());
    json!({
        "windowActiveSamples":samples.iter().filter(|v| v["windowActive"]==true).count(),
        "inactiveWindowSamples":samples.iter().filter(|v| v["windowActive"]!=true).count(),
        "missingFrameSamples":samples.iter().filter(|v| !v["inputToPresentedMs"].is_number()).count(),
        "offSizeSamples": samples
            .iter()
            .filter(|v| {
                off_size(v["viewport"].as_array().and_then(|size| {
                    Some((size.first()?.as_f64()? as f32, size.get(1)?.as_f64()? as f32))
                }))
            })
            .count(),
        "inputToFrameStartMs": field("inputToFrameStartMs"),
        "frameStartToPresentedMs": field("frameStartToPresentedMs"),
        "inputToPresentedMs": field("inputToPresentedMs"),
    })
}

// ---- readback ----

fn frame(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    window.update(cx, |shell, _, cx| shell.fixture_frame_readback(cx))
}

fn wb(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    window.update(cx, |shell, _, cx| shell.fixture_workbench_readback(cx))
}

fn panes(node: &Value, out: &mut Vec<Value>) {
    match node["kind"].as_str() {
        Some("pane") => out.push(node.clone()),
        Some("split") => {
            panes(&node["first"], out);
            panes(&node["second"], out);
        }
        _ => {}
    }
}

fn all_panes(rb: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    panes(&rb["root"], &mut out);
    out
}

async fn wait_wb(
    window: Window,
    cx: &mut AsyncApp,
    what: &str,
    done: impl Fn(&Value) -> bool,
) -> anyhow::Result<Value> {
    let mut last = Value::Null;
    for _ in 0..200 {
        last = wb(window, cx)?;
        if done(&last) && last["pendingMutations"] == 0 {
            return Ok(last);
        }
        settle(cx, 50).await;
    }
    anyhow::bail!("timed out waiting for {what}: {last}")
}

/// Rows painted for `scope` since the last reset.
fn painted(scope: &str) -> Vec<Value> {
    perf_counters::rendered_rows()[scope]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The painted text of rows belonging to entry `entry` (row ids are
/// `{entry}#...`).
fn painted_entry(scope: &str, entry: &str) -> Option<String> {
    let prefix = format!("{entry}#");
    let rows: Vec<String> = painted(scope)
        .into_iter()
        .filter(|row| {
            row["id"]
                .as_str()
                .is_some_and(|id| id == entry || id.starts_with(&prefix))
        })
        .filter_map(|row| row["text"].as_str().map(str::to_string))
        .collect();
    (!rows.is_empty()).then(|| rows.join("\n"))
}

/// Repaint until `entry`'s painted text satisfies `ok` (or time out).
/// Returns the last painted text and the repaint timings.
async fn wait_painted(
    window: Window,
    cx: &mut AsyncApp,
    scope: &str,
    entry: &str,
    ok: impl Fn(&str) -> bool,
) -> anyhow::Result<(Option<String>, Vec<Value>)> {
    let mut timings = Vec::new();
    let mut last = None;
    for _ in 0..60 {
        perf_counters::reset_rows();
        timings.push(timed(window, cx, true, |_, _| {}).await?);
        last = painted_entry(scope, entry);
        if last.as_deref().is_some_and(&ok) {
            break;
        }
        settle(cx, 50).await;
    }
    Ok((last, timings))
}

fn shot(window: Window, cx: &mut AsyncApp, path: PathBuf) -> anyhow::Result<()> {
    // The scene of the last presented frame; no extra draw.
    AnyWindowHandle::from(window).update(cx, |_, window, _| -> anyhow::Result<()> {
        window.render_to_image()?.save(path)?;
        Ok(())
    })??;
    Ok(())
}

/// A fresh probe capture: forget every probe, force every probed view to
/// re-render, and draw until a frame painted them (Main's composer always
/// paints one). Only rectangles painted after this call are read back.
async fn capture_probes(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    for _ in 0..6 {
        window.update(cx, |shell, window, cx| {
            shell.fixture_repaint_probes(window, cx)
        })?;
        timed(window, cx, true, |_, _| {}).await?;
        settle(cx, 30).await;
        if !perf_counters::probes("composer:main:").is_empty() {
            break;
        }
    }
    Ok(())
}

/// A real pointer click (move, a presented frame, press, release) at a
/// probed point, so hit testing uses the painted hitboxes; then a fresh
/// probe capture of the result.
async fn click_at(window: Window, cx: &mut AsyncApp, x: f32, y: f32) -> anyhow::Result<()> {
    let at = gpui::point(px(x), px(y));
    let dispatch = |event: gpui::PlatformInput, cx: &mut AsyncApp| -> anyhow::Result<()> {
        AnyWindowHandle::from(window).update(cx, move |_, window, cx| {
            window.dispatch_event(event, cx);
        })?;
        Ok(())
    };
    dispatch(
        gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position: at,
            pressed_button: None,
            modifiers: Default::default(),
        }),
        cx,
    )?;
    timed(window, cx, true, |_, _| {}).await?;
    dispatch(
        gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    )?;
    dispatch(
        gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: at,
            modifiers: Default::default(),
            click_count: 1,
        }),
        cx,
    )?;
    settle(cx, 300).await;
    capture_probes(window, cx).await
}

/// One correctness check: recorded, never aborting a baseline run.
struct Checks {
    list: Vec<Value>,
}

impl Checks {
    fn check(&mut self, name: &str, pass: bool, detail: Value) {
        self.list
            .push(json!({"name": name, "pass": pass, "detail": detail}));
    }
    fn failed(&self) -> Vec<String> {
        self.list
            .iter()
            .filter(|c| c["pass"] == false)
            .filter_map(|c| c["name"].as_str().map(str::to_string))
            .collect()
    }
}

/// A phase boundary for external RSS/footprint sampling: wall-clock ms on
/// stderr and in `phase-marks.jsonl`.
fn mark(output: &std::path::Path, phase: &str, edge: &str) {
    use std::io::Write as _;
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    eprintln!("workbench-performance phase={phase} edge={edge} unixMs={at}");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(output.join("phase-marks.jsonl"))
    {
        let _ = writeln!(
            file,
            "{}",
            json!({"phase": phase, "edge": edge, "unixMs": at as u64})
        );
    }
}

fn write_json(path: PathBuf, value: &Value) {
    let _ = std::fs::write(path, serde_json::to_vec_pretty(value).unwrap_or_default());
}

fn main() -> anyhow::Result<()> {
    // SAFETY: first thing in `main`, before the Tokio runtime, the GPUI
    // platform or any other thread exists; never changed afterwards.
    unsafe {
        std::env::set_var("LUNA_PERF_COUNTERS", "1");
        std::env::set_var("LUNA_MOCK_DELAY_MS", "20");
        std::env::set_var("LUNA_HARNESS", "mock");
    }
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let output = PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
        anyhow::anyhow!("usage: workbench-performance-fixture ABSOLUTE_OUTPUT_DIR [ENTRIES]")
    })?);
    anyhow::ensure!(output.is_absolute(), "output directory must be absolute");
    let entries: usize = std::env::args()
        .nth(2)
        .map(|n| n.parse())
        .transpose()?
        .unwrap_or(1200);
    anyhow::ensure!(
        entries >= 200,
        "at least 200 entries (200 carry large outputs)"
    );
    let candidate = std::env::var("LUNA_PERF_MODE").is_ok_and(|mode| mode == "candidate");
    std::fs::create_dir_all(&output)?;
    eprintln!("workbench-performance-fixture pid={}", std::process::id());
    for stale in [
        "results.json",
        "failure.txt",
        "workbench-performance.pass",
        "phase-marks.jsonl",
    ] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let root = tempfile::tempdir()?;
    write_json(
        output.join("isolation.json"),
        &json!({"temporaryRoot":root.path(),"pid":std::process::id()}),
    );
    let engine_data = root.path().join("engine");
    let ui_data = root.path().join("ui");
    let project = root.path().join("project");
    std::fs::create_dir_all(&engine_data)?;
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(&project)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(FixtureMock {
        inner: MockHarness {
            script: vec![
                AgentEvent::TextDelta {
                    text: "Noted.".into(),
                },
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    result: None,
                    error: None,
                    session_id: None,
                },
            ],
        },
    }));
    let profile = EngineProfile::development(&engine_data, "workbench-perf", "workbench-perf");
    let journal_dir = profile.store_root().join("journals");
    let accounts = AgentAccountsConfig {
        data_dir: root.path().join("accounts"),
        claude_config_dir: root.path().join("claude"),
        claude_config_file: root.path().join("claude.json"),
        codex_home: root.path().join("codex"),
    };
    accounts.disable_claude_keychain_fallback()?;
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            profile,
            Arc::new(registry),
            HarnessId::Mock,
            None,
            accounts,
        )
    })?;
    let mut auth = AuthConfig::new("http://127.0.0.1:1", root.path().join("auth"));
    auth.dev_user_id = "workbench-perf".into();
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let device = core.device_id.clone();

    // Seed through the public RPC, then write rows before anything watches.
    let seed_started = Instant::now();
    let journal = RunJournal::open(&journal_dir)?;
    let full_output = format!("FULL_OUTPUT_MARKER\n{}", "x".repeat(256 * 1024));
    runtime.block_on(async {
        let client = luna_rpc::memory_client(core.rpc_service());
        client
            .call(
                "Mutate",
                json!({"op": "createSpace", "spaceId": PROJECT, "deviceId": device, "path": project}),
            )
            .await?;
        let titles = [
            (MAIN, "Perf main"),
            (HIDDEN, "Perf hidden stream"),
            (PANE_B, "Perf pane B"),
            (PANE_C, "Perf pane C"),
            (PANE_D, "Perf pane D"),
            (FORK, "Perf fork"),
            (OTHER, "Perf other"),
        ];
        for (chat, title) in titles {
            client
                .call(
                    "Mutate",
                    json!({
                        "op": "createChat", "chatId": chat, "spaceId": PROJECT, "cwd": project,
                        "config": {"harness": "mock", "model": "fixture", "permissionMode": "ask", "sandbox": "workspace-write"},
                    }),
                )
                .await?;
            core.workspace.rename_chat(chat, title)?;
        }
        anyhow::Ok(())
    })?;
    let history: Vec<SessionMessageEntry> = (0..entries).map(text_entry).collect();
    let mut main_rows = history.clone();
    for (index, entry) in main_rows.iter_mut().take(200).enumerate() {
        let id = format!("large-output-{index}");
        journal.append(
            MAIN,
            &AgentEvent::ToolResult {
                id: id.clone(),
                is_error: false,
                output: Some(full_output.clone()),
                diff: None,
            },
        )?;
        entry.parts.push(part(json!({
            "kind": "tool", "id": id,
            "call": {"kind": "exec", "command": "fixture only"},
            "resolved": true,
            "output": luna_doc::summarize_tool_output(&full_output),
            "outputRef": format!("{MAIN}/{id}"),
            "outputBytes": full_output.len(),
        })));
    }
    let at = entries as i64 + 10;
    main_rows.extend(tail("AAAAAAAA", at));
    let mut fork_rows = main_rows.clone();
    fork_rows.truncate(fork_rows.len() - 3);
    fork_rows.extend(tail("FORKFORK", at));
    let mut pane_rows = history.clone();
    pane_rows.extend(tail("PANEPANE", at));
    let seed_runtime = runtime.enter();
    for (chat, rows) in [
        (MAIN, &main_rows),
        (HIDDEN, &pane_rows),
        (PANE_B, &pane_rows),
        (PANE_C, &pane_rows),
        (PANE_D, &pane_rows),
        (FORK, &fork_rows),
    ] {
        core.doc_host
            .open(chat)?
            .doc()
            .replace_conversation_entries(rows, &HashMap::new())?;
    }
    core.doc_host
        .open(OTHER)?
        .doc()
        .replace_conversation_entries(&tail("OTHEROTH", 1), &HashMap::new())?;
    core.doc_host
        .open(CHILD)?
        .doc()
        .replace_conversation_entries(
            &[assistant("child-0", vec![text_part("Child working.")], 1)],
            &HashMap::new(),
        )?;
    drop(seed_runtime);
    let seed_ms = seed_started.elapsed().as_secs_f64() * 1000.0;

    let ipc_port = std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port();
    let _ipc = runtime.block_on(luna_engine::serve_ipc(
        ipc_port,
        core.rpc_service(),
        &engine_data,
    ))?;
    write_json(
        output.join("runtime.json"),
        &json!({
            "pid":std::process::id(), "ipcPort":ipc_port,
            "temporaryRoot":root.path(), "engineData":engine_data,
            "uiData":ui_data, "project":project,
            "allocator":"mimalloc-v2 on macOS", "entries":entries,
        }),
    );
    let boot = state::EngineBootConfig {
        data_dir: engine_data.clone(),
        ipc_port,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let handle = runtime.block_on(state::EngineHandle::bootstrap(boot.clone()))?;
    let chats = core.workspace.read_chats()?;
    let spaces = core.workspace.read_spaces()?;
    let doc_host = core.doc_host.clone();
    // The durable command ledger, read for exact targets and receipts.
    let ledger = {
        let store = core.store.clone();
        let sends = core.store.clone();
        Ledger {
            sequence: Box::new(move |task| {
                Ok(store.task_command_snapshot(task)?.snapshot.task_sequence)
            }),
            receipts: Box::new(move |task, after| {
                Ok(sends
                    .read_task_commands(task, after, 100)?
                    .events
                    .into_iter()
                    .map(|event| event.receipt)
                    .collect())
            }),
        }
    };
    let reader = {
        let _entered = runtime.enter();
        Arc::new(luna_rpc::memory_client(core.rpc_service()))
    };
    let full_len = full_output.len();
    let outcome: Rc<RefCell<Option<anyhow::Result<()>>>> = Rc::default();
    let outcome_app = outcome.clone();
    let app_output = output.clone();

    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let settings = settings::UiSettings::load(&ui_data);
            settings::init(settings.clone(), ui_data.clone(), cx);
            let fonts = typography::register_fonts(cx);
            typography::init(
                settings.ui_font_family.clone(),
                settings.ui_font_size,
                settings.terminal_font_family.clone(),
                settings.terminal_font_size,
                settings.code_font_family.clone(),
                settings.code_font_size,
                fonts,
                cx,
            );
            theme_library::init(ui_data.clone(), cx);
            appearance::init(
                appearance::AppearanceMode::Dark,
                settings.theme_selection,
                settings.accent,
                settings.surface,
                cx,
            );
            history::init(
                settings.git_history_columns,
                settings.git_history_column_widths,
                settings.git_history_column_order,
                settings.git_history_author_display,
                cx,
            );
            composer::init(cx, settings.composer_send_behavior);
            terminal::panel::init(cx);
            app_menus::init(cx);
            let device_name = device.clone();
            let state = cx.new(|cx| {
                let mut state = state::AppState::new();
                state.local_device_id = Some(device_name.clone());
                state.devices = vec![
                    serde_json::from_value(json!({
                        "id": device_name, "name": "Perf Mac", "platform": "macos", "lastSeenAt": null,
                    }))
                    .unwrap(),
                ];
                state.chats = chats;
                state.spaces = spaces;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.fixture_attach_engine(handle, cx);
                state
            });
            let window: Window = cx
                .open_window(
                    WindowOptions {
                        window_background: theme::Theme::of(cx).window_background_appearance(),
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(40.), px(40.)),
                            size(px(WIDTH), px(HEIGHT)),
                        ))),
                        ..Default::default()
                    },
                    |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
                )
                .unwrap();
            cx.activate(true);
            let run = Run {
                window,
                state,
                output: app_output.clone(),
                entries,
                doc_host,
                ledger,
                reader,
                full_len,
                device: device.clone(),
                seed_ms,
                candidate,
            };
            let failure = app_output.join("failure.txt");
            cx.spawn(async move |cx| {
                let result = run.drive(cx).await;
                if let Err(error) = &result {
                    let _ = std::fs::write(&failure, format!("{error:#}\n"));
                }
                *outcome_app.borrow_mut() = Some(result);
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    drop(journal);
    runtime.block_on(core.shutdown());
    let result = outcome
        .borrow_mut()
        .take()
        .unwrap_or_else(|| Err(anyhow::anyhow!("the fixture never finished")));
    if let Err(error) = &result {
        let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
    }
    result
}

/// Reads of the host's durable task-command ledger.
struct Ledger {
    sequence: Box<dyn Fn(&str) -> anyhow::Result<u64>>,
    receipts: Box<dyn Fn(&str, u64) -> anyhow::Result<Vec<luna_proto::CommandReceipt>>>,
}

fn is_send(receipt: &luna_proto::CommandReceipt) -> bool {
    matches!(receipt.intent.as_str(), "run" | "session" | "sendQueuedNow")
}

struct Run {
    window: Window,
    state: gpui::Entity<state::AppState>,
    output: PathBuf,
    entries: usize,
    doc_host: luna_engine::DocHost,
    ledger: Ledger,
    reader: Arc<luna_rpc::RpcClient>,
    full_len: usize,
    device: String,
    seed_ms: f64,
    candidate: bool,
}

impl Run {
    fn select(&self, cx: &mut AsyncApp, chat: &str) {
        let chat = chat.to_string();
        cx.update(|cx| {
            self.state
                .update(cx, |state, cx| state.select_chat(Some(chat), cx))
        });
    }

    fn phase_end(&self, name: &str, cx: &mut AsyncApp) -> anyhow::Result<Value> {
        let counters = perf_counters::snapshot();
        let _ = shot(self.window, cx, self.output.join(format!("{name}.png")));
        Ok(counters)
    }

    /// Select `chat` as Main and time until its tail text is painted.
    async fn open_main(&self, cx: &mut AsyncApp, chat: &str) -> anyhow::Result<Value> {
        perf_counters::reset();
        let t0 = Instant::now();
        self.select(cx, chat);
        let scope = format!("main:{chat}");
        let mut timings = Vec::new();
        let mut painted_at = None;
        for _ in 0..400 {
            timings.push(timed(self.window, cx, true, |_, _| {}).await?);
            if painted_entry(&scope, "tail-text").is_some() {
                painted_at = Some(t0.elapsed().as_secs_f64() * 1000.0);
                break;
            }
            settle(cx, 5).await;
        }
        Ok(json!({
            "selectToTailPaintedMs": painted_at,
            "frames": timing_summary(&timings),
            "counters": perf_counters::snapshot(),
        }))
    }

    async fn drive(self, cx: &mut AsyncApp) -> anyhow::Result<()> {
        let window = self.window;
        let mut checks = Checks { list: Vec::new() };
        let mut phases = serde_json::Map::new();
        // Layout probes stay off through the measured phases (the baseline
        // never painted them) unless the diagnostic override keeps them on.
        let layout_probes_while_measuring = perf_counters::probes_enabled();
        settle(cx, 1500).await;
        // Activating the standalone app does not always make its window key.
        // Establish the same foreground precondition as the accepted baseline
        // before starting any clock; never collect a background benchmark.
        let startup = Instant::now();
        let active_before =
            AnyWindowHandle::from(window).update(cx, |_, window, _| window.is_window_active())?;
        if !active_before {
            AnyWindowHandle::from(window).update(cx, |_, window, cx| {
                cx.activate(true);
                window.activate_window();
            })?;
        }
        let mut active = active_before;
        while !active && startup.elapsed() < Duration::from_secs(3) {
            settle(cx, 50).await;
            active = AnyWindowHandle::from(window)
                .update(cx, |_, window, _| window.is_window_active())?;
        }
        let startup_activation = json!({
            "activeBefore": active_before,
            "activated": !active_before,
            "active": active,
            "waitedMs": startup.elapsed().as_secs_f64() * 1000.0,
        });
        std::fs::write(
            self.output.join("startup-activation.json"),
            serde_json::to_vec_pretty(&startup_activation)?,
        )?;
        anyhow::ensure!(active, "the native benchmark window never became active");
        // `LUNA_PERF_PHASES=resize`: only the resize repro (same window and
        // engine); default runs every phase, the resize one last.
        let resize_only = std::env::var("LUNA_PERF_PHASES").is_ok_and(|phases| phases == "resize");
        if !resize_only {
            // 1. cold-open
            mark(&self.output, "cold-open", "start");
            let cold = self.open_main(cx, MAIN).await?;
            checks.check(
                "cold-open paints Main's tail",
                !cold["selectToTailPaintedMs"].is_null(),
                cold["selectToTailPaintedMs"].clone(),
            );
            wait_wb(window, cx, "Main's layout", |rb| {
                rb["loaded"] == true && rb["main"] == MAIN
            })
            .await?;
            let cold_counters = self.phase_end("01-cold-open", cx)?;
            phases.insert(
                "coldOpen".into(),
                json!({"open": cold, "after": cold_counters}),
            );
            let blob: Value = self
                .reader
                .call(
                    "FetchToolBlob",
                    json!({"blobRef": format!("{MAIN}/large-output-199")}),
                )
                .await?;
            let full_ok = blob["text"].as_str().is_some_and(|text| {
                text.len() == self.full_len && text.starts_with("FULL_OUTPUT_MARKER")
            });
            checks.check(
                "large outputs stay durable behind their preview",
                full_ok,
                json!({"bytes": blob["text"].as_str().map(str::len)}),
            );

            mark(&self.output, "cold-open", "end");
            // 2. reopen: 20 cycles away (small chat) and back.
            mark(&self.output, "reopen", "start");
            let mut cycles = Vec::new();
            for _ in 0..20 {
                let away = self.open_main(cx, OTHER).await?;
                let back = self.open_main(cx, MAIN).await?;
                cycles.push(json!({
                    "awayMs": away["selectToTailPaintedMs"],
                    "backMs": back["selectToTailPaintedMs"],
                    "awayFrames": away["frames"],
                    "backFrames": back["frames"],
                    "backCounters": back["counters"],
                }));
            }
            let back_ms: Vec<f64> = cycles.iter().filter_map(|c| c["backMs"].as_f64()).collect();
            checks.check(
                "every reopen paints Main",
                back_ms.len() == 20,
                json!({"painted": back_ms.len()}),
            );
            if self.candidate {
                let confirmed = cycles
                    .iter()
                    .filter(|cycle| {
                        cycle["backCounters"]["state.resume_confirmed"]
                            .as_object()
                            .is_some_and(|scopes| {
                                scopes
                                    .values()
                                    .map(|value| value["calls"].as_u64().unwrap_or(0))
                                    .sum::<u64>()
                                    == 1
                            })
                    })
                    .count();
                checks.check(
                    "every warm reopen reuses only host-confirmed history",
                    confirmed == 20,
                    json!({"confirmed":confirmed}),
                );
            }
            let reopen_counters = self.phase_end("02-reopen", cx)?;
            phases.insert(
                "reopen".into(),
                json!({"backMs": summary(back_ms), "cycles": cycles, "last": reopen_counters}),
            );
            wait_wb(window, cx, "Main again", |rb| {
                rb["loaded"] == true && rb["main"] == MAIN
            })
            .await?;

            mark(&self.output, "reopen", "end");
            // 3-4. panes (1, 2, 4) and hidden streaming while typing.
            window.update(cx, |shell, _, cx| shell.fixture_workbench_open(HIDDEN, cx))?;
            wait_wb(window, cx, "the hidden chat's tab", |rb| {
                all_panes(rb).iter().any(|pane| pane["active"] == HIDDEN)
            })
            .await?;
            window.update(cx, |shell, _, cx| shell.fixture_workbench_open(MAIN, cx))?;
            wait_wb(window, cx, "Main active, the stream chat hidden", |rb| {
                all_panes(rb).iter().any(|pane| pane["active"] == MAIN)
                    && all_panes(rb).iter().all(|pane| pane["active"] != HIDDEN)
            })
            .await?;
            for (count, extra) in [(1usize, None), (2, Some(PANE_B)), (4, Some(PANE_C))] {
                if let Some(chat) = extra {
                    // Two groups: B beside Main's. Four: C beside Main's, then D
                    // beside the last group; one layout mutation at a time.
                    let mut adds = vec![(chat, 0usize)];
                    if count == 4 {
                        adds.push((PANE_D, usize::MAX));
                    }
                    for (add, at) in adds {
                        let rb = wb(window, cx)?;
                        let groups = all_panes(&rb);
                        let target = groups[at.min(groups.len() - 1)]["id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                        let before = groups.len();
                        let opened = window.update(cx, |shell, _, cx| {
                            shell.fixture_workbench_open_beside(add, &target, cx)
                        })?;
                        checks.check(
                            &format!("panes-{count}: {add} opens beside"),
                            opened,
                            Value::Null,
                        );
                        wait_wb(window, cx, "the pane layout", |rb| {
                            all_panes(rb).len() == before + 1
                        })
                        .await?;
                    }
                }
                settle(cx, 800).await;
                mark(&self.output, &format!("panes-{count}"), "start");
                // Static cost of this layout: 30 idle repaints.
                perf_counters::reset();
                let mut idle = Vec::new();
                for _ in 0..30 {
                    idle.push(timed(window, cx, true, |_, _| {}).await?);
                }
                let idle_counters = perf_counters::snapshot();
                // Hidden streaming while Main's composer takes 60 keystrokes.
                let focused = window.update(cx, |shell, window, cx| {
                    shell.fixture_workbench_focus_input(MAIN, window, cx)
                })?;
                checks.check(
                    &format!("panes-{count}: Main's composer focused"),
                    focused,
                    Value::Null,
                );
                let stop = Arc::new(AtomicBool::new(false));
                let streamer = {
                    let doc_host = self.doc_host.clone();
                    let stop = stop.clone();
                    let device = self.device.clone();
                    let entry = format!("hidden-stream-{count}");
                    std::thread::spawn(move || -> anyhow::Result<usize> {
                        let handle = doc_host.open(HIDDEN)?;
                        let doc = handle.doc();
                        let mut writer = SegmentWriter::begin(doc, &entry, &device, 1_000_000)?;
                        let mut text = String::new();
                        let mut ticks = 0;
                        while !stop.load(Ordering::Relaxed) && ticks < 400 {
                            ticks += 1;
                            text.push_str("streamed words while hidden ");
                            let mut parts = vec![text_part(&text)];
                            // Every 10th tick a tool part changes: a non-text
                            // frame, the path that notifies every observer.
                            for tool in 0..ticks / 10 {
                                parts.push(tool_part(
                                    &format!("hidden-tool-{tool}"),
                                    &format!("hidden tool {tool}"),
                                ));
                            }
                            writer.sync(&parts)?;
                            std::thread::sleep(Duration::from_millis(40));
                        }
                        writer.finish(&[text_part(&text)], MessageStatus::Complete)?;
                        Ok(ticks)
                    })
                };
                perf_counters::reset();
                let recorder = FrameRecorder::start(window, cx)?;
                let mut keys = Vec::new();
                let letters = "abcdefghijklmnopqrstuvwxyz";
                for index in 0..60 {
                    let key = if index % 6 == 5 {
                        "space".to_string()
                    } else {
                        letters[index % 26..index % 26 + 1].to_string()
                    };
                    let keystroke = Keystroke::parse(&key)?;
                    keys.push(
                        timed(window, cx, false, move |window, cx| {
                            window.dispatch_keystroke(keystroke, cx);
                        })
                        .await?,
                    );
                    settle(cx, 30).await;
                }
                let intervals = recorder.stop();
                stop.store(true, Ordering::Relaxed);
                let ticks = streamer
                    .join()
                    .map_err(|_| anyhow::anyhow!("hidden streamer panicked"))??;
                let typing_counters = perf_counters::snapshot();
                let draft =
                    window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))?;
                checks.check(
                    &format!("panes-{count}: 60 keystrokes reach Main's draft"),
                    draft.as_deref().is_some_and(|d| d.trim_end().len() >= 50),
                    json!({"draftLen": draft.as_deref().map(str::len)}),
                );
                window.update(cx, |shell, _, cx| {
                    shell.fixture_workbench_type(MAIN, "", false, cx)
                })?;
                let _ = shot(
                    window,
                    cx,
                    self.output.join(format!("03-panes-{count}.png")),
                );
                mark(&self.output, &format!("panes-{count}"), "end");
                phases.insert(
                    format!("panes{count}"),
                    json!({
                        "idleRepaint": timing_summary(&idle),
                        "idleCounters": idle_counters,
                        "typing": timing_summary(&keys),
                        "frameIntervalsMs": intervals,
                        "hiddenTicks": ticks,
                        "typingCounters": typing_counters,
                    }),
                );
            }

            // 5. 20 nested child updates on a live spawn chip in Main.
            mark(&self.output, "child-updates", "start");
            perf_counters::reset();
            let child_scope = format!("main:{MAIN}");
            let mut child_updates = Vec::new();
            {
                let handle = self.doc_host.open(MAIN)?;
                let doc = handle.doc();
                let mut writer =
                    SegmentWriter::begin(doc, "child-stream", &self.device, 2_000_000)?;
                for update in 0..20u64 {
                    let marker = format!("child-update-{update:02}");
                    writer.sync(&[spawn_part("child-stream-part", "running", &marker, update)])?;
                    let t0 = Instant::now();
                    let (seen, timings) =
                        wait_painted(window, cx, &child_scope, "child-stream", |text| {
                            text.contains(&marker)
                        })
                        .await?;
                    child_updates.push(json!({
                        "update": update,
                        "painted": seen.as_deref().is_some_and(|t| t.contains(&marker)),
                        "updateToPaintedMs": t0.elapsed().as_secs_f64() * 1000.0,
                        "repaints": timings.len(),
                    }));
                }
                writer.finish(
                    &[spawn_part(
                        "child-stream-part",
                        "done",
                        "child-update-done",
                        20,
                    )],
                    MessageStatus::Complete,
                )?;
            }
            let child_painted = child_updates
                .iter()
                .filter(|u| u["painted"] == true)
                .count();
            checks.check(
                "every child update is painted",
                child_painted == 20,
                json!({"painted": child_painted}),
            );
            let child_counters = self.phase_end("05-child-updates", cx)?;
            phases.insert(
            "childUpdates".into(),
            json!({
                "updateToPaintedMs": summary(child_updates.iter().filter_map(|u| u["updateToPaintedMs"].as_f64()).collect()),
                "updates": child_updates,
                "counters": child_counters,
            }),
        );

            mark(&self.output, "child-updates", "end");
            mark(&self.output, "rewrites", "start");
            // 6. Rewrites. Each check reads the PAINTED row (and records the row
            // projection beside it); the model alone proves nothing visual.
            let main_scope = format!("main:{MAIN}");
            let mut rewrites = serde_json::Map::new();
            let main_doc = self.doc_host.open(MAIN)?;
            let current =
                |doc_host: &luna_engine::DocHost| -> anyhow::Result<Vec<SessionMessageEntry>> {
                    Ok(doc_host.open(MAIN)?.doc().read_entries()?)
                };
            for (name, entry, from, to) in [
                ("text", "tail-text", "TEXT-AAAAAAAA", "TEXT-BBBBBBBB"),
                ("tool", "tail-tool", "TOOL-AAAAAAAA", "TOOL-BBBBBBBB"),
                (
                    "childResult",
                    "tail-spawn",
                    "CHILD-AAAAAAAA",
                    "CHILD-BBBBBBBB",
                ),
            ] {
                let mut rows = current(&self.doc_host)?;
                for row in rows.iter_mut().filter(|row| row.id == entry) {
                    let json = serde_json::to_string(&row.parts)?.replace(from, to);
                    row.parts = serde_json::from_str(&json)?;
                }
                perf_counters::reset();
                replace_atomically(main_doc.doc(), &rows)?;
                settle(cx, 400).await;
                let (seen, _) = wait_painted(window, cx, &main_scope, entry, |text| {
                    text.contains(to) && !text.contains(from)
                })
                .await?;
                let projection =
                    window.update(cx, |shell, _, cx| shell.fixture_transcript_rows(MAIN, cx))?;
                let pass = seen
                    .as_deref()
                    .is_some_and(|t| t.contains(to) && !t.contains(from));
                checks.check(
                    &format!("same-id same-length {name} rewrite is painted"),
                    pass,
                    json!({"painted": seen}),
                );
                rewrites.insert(
                    name.into(),
                    json!({
                        "pass": pass,
                        "painted": seen,
                        "projection": projection
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|row| row["entryId"] == entry)
                            .cloned()
                            .collect::<Vec<_>>(),
                        "counters": perf_counters::snapshot(),
                    }),
                );
                let _ = shot(
                    window,
                    cx,
                    self.output.join(format!("06-rewrite-{name}.png")),
                );
            }
            // Compaction: the first 100 rows become one summary entry.
            let mut rows = current(&self.doc_host)?;
            rows.drain(0..100);
            rows.insert(
                0,
                assistant(
                    "compaction-summary",
                    vec![text_part(
                        "COMPACTION-SUMMARY of the first hundred entries.",
                    )],
                    0,
                ),
            );
            perf_counters::reset();
            replace_atomically(main_doc.doc(), &rows)?;
            settle(cx, 600).await;
            timed(window, cx, true, |_, _| {}).await?;
            let projection =
                window.update(cx, |shell, _, cx| shell.fixture_transcript_rows(MAIN, cx))?;
            let entry_ids: BTreeSet<String> = projection
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| row["entryId"].as_str().map(str::to_string))
                .collect();
            let compacted = entry_ids.contains("compaction-summary")
                && !(0..100).any(|i| entry_ids.contains(&format!("settled-{i:05}")));
            checks.check(
                "compaction replaces the range in the row projection",
                compacted,
                json!({"entries": entry_ids.len()}),
            );
            // The same compaction, painted: scroll Main to its first rows; the
            // summary is painted and no compacted entry is.
            window.update(cx, |shell, _, cx| {
                shell.fixture_main_transcript_edge(true, cx)
            })?;
            let (top_seen, _) =
                wait_painted(window, cx, &main_scope, "compaction-summary", |text| {
                    text.contains("COMPACTION-SUMMARY")
                })
                .await?;
            let top_rows: Vec<String> = painted(&main_scope)
                .iter()
                .filter_map(|row| row["id"].as_str().map(str::to_string))
                .collect();
            let compacted_painted = top_rows
                .iter()
                .any(|id| (0..100).any(|i| id.starts_with(&format!("settled-{i:05}#"))));
            let painted_ok = top_seen
                .as_deref()
                .is_some_and(|t| t.contains("COMPACTION-SUMMARY"))
                && !compacted_painted;
            checks.check(
            "compaction is painted: the summary shows and no compacted row does",
            painted_ok,
            json!({"summary": top_seen, "paintedRows": top_rows.len(), "compactedPainted": compacted_painted}),
        );
            let _ = shot(window, cx, self.output.join("06-compaction-top.png"));
            window.update(cx, |shell, _, cx| {
                shell.fixture_main_transcript_edge(false, cx)
            })?;
            settle(cx, 400).await;
            rewrites.insert(
                "compaction".into(),
                json!({
                    "pass": compacted && painted_ok,
                    "projectionEntries": entry_ids.len(),
                    "painted": painted_ok,
                    "counters": perf_counters::snapshot(),
                }),
            );
            // Fork beside Main: same ids, its own text.
            let rb = wb(window, cx)?;
            let first = all_panes(&rb)[0]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            window.update(cx, |shell, _, cx| {
                shell.fixture_workbench_open_beside(FORK, &first, cx)
            })?;
            settle(cx, 800).await;
            let fork_scope = format!("pane:{FORK}");
            let (fork_seen, _) = wait_painted(window, cx, &fork_scope, "tail-text", |text| {
                text.contains("TEXT-FORKFORK")
            })
            .await?;
            let (main_seen, _) = wait_painted(window, cx, &main_scope, "tail-text", |text| {
                text.contains("TEXT-BBBBBBBB")
            })
            .await?;
            let fork_ok = fork_seen
                .as_deref()
                .is_some_and(|t| t.contains("TEXT-FORKFORK"))
                && main_seen
                    .as_deref()
                    .is_some_and(|t| t.contains("TEXT-BBBBBBBB"));
            checks.check(
                "a fork with the same ids paints its own rows beside Main",
                fork_ok,
                json!({"fork": fork_seen, "main": main_seen}),
            );
            rewrites.insert(
                "fork".into(),
                json!({"pass": fork_ok, "fork": fork_seen, "main": main_seen}),
            );
            let _ = shot(window, cx, self.output.join("06-fork-beside.png"));
            // Binding: Main switched to the fork (same ids, different content).
            let bound = self.open_main(cx, FORK).await?;
            let bound_seen = painted_entry(&format!("main:{FORK}"), "tail-text");
            let bound_ok = bound_seen
                .as_deref()
                .is_some_and(|t| t.contains("TEXT-FORKFORK"));
            checks.check(
                "rebinding Main to the fork paints the fork's rows",
                bound_ok,
                json!({"painted": bound_seen}),
            );
            rewrites.insert(
                "binding".into(),
                json!({"pass": bound_ok, "painted": bound_seen, "open": bound}),
            );
            self.open_main(cx, MAIN).await?;
            wait_wb(window, cx, "Main after the fork", |rb| {
                rb["loaded"] == true && rb["main"] == MAIN
            })
            .await?;
            // Rewind: the visible tail is removed.
            let mut rows = current(&self.doc_host)?;
            rows.retain(|row| !row.id.starts_with("tail-") && row.id != "child-stream");
            perf_counters::reset();
            replace_atomically(main_doc.doc(), &rows)?;
            settle(cx, 600).await;
            let mut rewound_rows = Vec::new();
            for _ in 0..20 {
                perf_counters::reset_rows();
                timed(window, cx, true, |_, _| {}).await?;
                rewound_rows = painted(&main_scope);
                if !rewound_rows.is_empty()
                    && rewound_rows.iter().all(|row| {
                        row["id"]
                            .as_str()
                            .is_some_and(|id| !id.starts_with("tail-"))
                    })
                {
                    break;
                }
                settle(cx, 50).await;
            }
            let rewind_ok = !rewound_rows.is_empty()
                && rewound_rows.iter().all(|row| {
                    row["id"].as_str().is_some_and(|id| {
                        !id.starts_with("tail-") && !id.starts_with("child-stream")
                    })
                });
            checks.check(
            "a rewind leaves no removed row painted",
            rewind_ok,
            json!({"painted": rewound_rows.iter().filter_map(|r| r["id"].as_str()).collect::<Vec<_>>()}),
        );
            rewrites.insert(
                "rewind".into(),
                json!({"pass": rewind_ok, "counters": perf_counters::snapshot()}),
            );
            let _ = shot(window, cx, self.output.join("06-rewind.png"));
            phases.insert("rewrites".into(), Value::Object(rewrites));

            mark(&self.output, "rewrites", "end");
            mark(&self.output, "ownership", "start");
            // 7. Ownership in two panes (Main + B): keyboard send, model picker,
            // queued-row edit.
            let mut ownership = serde_json::Map::new();
            let main_entries_before = self.doc_host.open(MAIN)?.doc().read_entries()?.len();
            let main_sequence = (self.ledger.sequence)(MAIN)?;
            let b_sequence = (self.ledger.sequence)(PANE_B)?;
            window.update(cx, |shell, _, cx| {
                shell.fixture_workbench_type(MAIN, "main draft stays", false, cx)
            })?;
            let focused = window.update(cx, |shell, window, cx| {
                shell.fixture_workbench_focus_input(PANE_B, window, cx)
            })?;
            let keyboard_chat = window.update(cx, |shell, window, cx| {
                shell.fixture_workbench_keyboard_chat(window, cx)
            })?;
            checks.check(
                "pane B's composer owns the keyboard",
                focused && keyboard_chat.as_deref() == Some(PANE_B),
                json!({"keyboardChat": keyboard_chat}),
            );
            let mut send_keys = Vec::new();
            for ch in OWN_PROMPT.chars() {
                let key = if ch == ' ' {
                    "space".to_string()
                } else {
                    ch.to_string()
                };
                let keystroke = Keystroke::parse(&key)?;
                send_keys.push(
                    timed(window, cx, false, move |window, cx| {
                        window.dispatch_keystroke(keystroke, cx);
                    })
                    .await?,
                );
            }
            let enter = Keystroke::parse("enter")?;
            let send_timing = timed(window, cx, false, move |window, cx| {
                window.dispatch_keystroke(enter, cx);
            })
            .await?;
            let mut receipt = None;
            let mut b_texts = Vec::new();
            for _ in 0..200 {
                settle(cx, 100).await;
                receipt = (self.ledger.receipts)(PANE_B, b_sequence)?
                    .into_iter()
                    .filter(is_send)
                    .find(|r| r.status == luna_proto::CommandReceiptStatus::Applied);
                b_texts = self
                    .doc_host
                    .open(PANE_B)?
                    .doc()
                    .read_entries()?
                    .into_iter()
                    .filter(|e| e.role == MessageRole::User)
                    .flat_map(|e| e.parts)
                    .filter_map(|p| match p {
                        MessagePart::Text { text, .. } => Some(text),
                        _ => None,
                    })
                    .collect();
                if receipt.is_some() && b_texts.iter().any(|t| t.contains(OWN_PROMPT)) {
                    break;
                }
            }
            let main_sends = (self.ledger.receipts)(MAIN, main_sequence)?
                .iter()
                .filter(|r| is_send(r))
                .count();
            let main_draft =
                window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))?;
            let main_entries_after = self.doc_host.open(MAIN)?.doc().read_entries()?.len();
            let sent_once = b_texts.iter().filter(|t| t.contains(OWN_PROMPT)).count() == 1;
            let send_ok = receipt.is_some()
                && sent_once
                && main_sends == 0
                && main_draft.as_deref() == Some("main draft stays")
                && main_entries_after == main_entries_before;
            checks.check(
            "Enter in pane B sends once to B with an applied receipt; Main untouched",
            send_ok,
            json!({
                "receipt": receipt.as_ref().map(|r| json!({"commandId": r.command_id, "status": format!("{:?}", r.status), "intent": r.intent})),
                "sentToB": b_texts.iter().filter(|t| t.contains(OWN_PROMPT)).count(),
                "mainSends": main_sends,
                "mainDraft": main_draft,
                "mainEntries": [main_entries_before, main_entries_after],
            }),
        );
            ownership.insert(
                "send".into(),
                json!({"pass": send_ok, "keys": timing_summary(&send_keys), "enter": send_timing}),
            );
            // Model picker shortcut with B's composer focused.
            window.update(cx, |shell, window, cx| {
                shell.fixture_workbench_focus_input(PANE_B, window, cx)
            })?;
            let picker_key = Keystroke::parse("cmd-/")?;
            let picker_timing = timed(window, cx, false, move |window, cx| {
                window.dispatch_keystroke(picker_key, cx);
            })
            .await?;
            settle(cx, 200).await;
            let models = window.update(cx, |shell, _, cx| {
                (
                    shell.fixture_workbench_model(MAIN, cx),
                    shell.fixture_workbench_model(PANE_B, cx),
                )
            })?;
            let picker_ok = models.1.as_ref().is_some_and(|(_, open)| *open)
                && models.0.as_ref().is_none_or(|(_, open)| !*open);
            checks.check(
                "the model shortcut opens only pane B's picker",
                picker_ok,
                json!({"main": models.0.map(|m| m.1), "paneB": models.1.map(|m| m.1)}),
            );
            let _ = shot(window, cx, self.output.join("07-model-picker-b.png"));
            let escape = Keystroke::parse("escape")?;
            timed(window, cx, false, move |window, cx| {
                window.dispatch_keystroke(escape, cx);
            })
            .await?;
            ownership.insert(
                "modelPicker".into(),
                json!({"pass": picker_ok, "timing": picker_timing}),
            );
            // A held queued row in B: its edit begins only in B's composer.
            let held_id = "perf-held-row";
            let held = luna_doc::QueuedMessage {
                input_items: Vec::new(),
                id: held_id.into(),
                text: "perf held row".into(),
                attachments: Vec::new(),
                hold_for_turn_end: false,
                follow_up_behavior: luna_proto::FollowUpBehavior::Queue,
                hold_reason: Some(luna_doc::QueueHoldReason::ExecutionUnavailable),
                issued_by: self.device.clone(),
                issued_at: chrono::Utc::now().timestamp_millis(),
                edited_at: None,
                revision: 1,
                execution: None,
                status: luna_doc::QueueMessageStatus::Held,
                delivery_gate: None,
            };
            self.doc_host.open(PANE_B)?.doc().push_queued(&held)?;
            let composer = |chat: &str, cx: &mut AsyncApp| {
                let chat = chat.to_string();
                window.update(cx, move |shell, _, cx| {
                    shell.fixture_workbench_composer(&chat, cx)
                })
            };
            let mut row_seen = false;
            for _ in 0..60 {
                settle(cx, 100).await;
                if let Some(b) = composer(PANE_B, cx)? {
                    let rows = cx.update(|cx| b.read(cx).fixture_queue_rows(cx));
                    row_seen = rows
                        .as_array()
                        .is_some_and(|rows| rows.iter().any(|r| r["id"] == held_id));
                    if row_seen {
                        break;
                    }
                }
            }
            let (mut b_edit, mut main_edit) = (Value::Null, Value::Null);
            if let (Some(b), Some(main)) = (composer(PANE_B, cx)?, composer(MAIN, cx)?) {
                cx.update(|cx| b.update(cx, |b, cx| b.fixture_queue_edit(held_id, cx)));
                for _ in 0..60 {
                    settle(cx, 100).await;
                    b_edit = cx.update(|cx| b.read(cx).fixture_queue_edit_state(cx));
                    if b_edit["editing"] == held_id {
                        break;
                    }
                }
                main_edit = cx.update(|cx| main.read(cx).fixture_queue_edit_state(cx));
                let _ = shot(window, cx, self.output.join("07-queued-edit-b.png"));
                cx.update(|cx| b.update(cx, |b, cx| b.fixture_queue_edit_cancel(cx)));
            }
            let edit_ok =
                row_seen && b_edit["editing"] == held_id && main_edit["editing"].is_null();
            checks.check(
                "a queued-row edit begins only in pane B's composer",
                edit_ok,
                json!({"rowSeen": row_seen, "paneB": b_edit, "main": main_edit}),
            );
            ownership.insert("queuedEdit".into(), json!({"pass": edit_ok}));
            phases.insert("ownership".into(), Value::Object(ownership));

            mark(&self.output, "ownership", "end");
            // The measured phases end here: every timed sample above must have
            // been at the measured window size.
            MEASURING.store(false, Ordering::Relaxed);
            let off = OFF_SIZE_SAMPLES.load(Ordering::Relaxed);
            checks.check(
                "every measured timing sample ran at the measured window size",
                off == 0,
                json!({"offSizeSamples": off, "expected": [WIDTH, HEIGHT]}),
            );

            // 8. Post-measurement: visibility and cache admission (failure cases
            // 4 and 1). Nothing above changes; the window is unchanged.
            mark(&self.output, "cache-visibility", "start");
            let mut visibility = serde_json::Map::new();
            // An idle visible pane paints its next update with no input: no
            // frame for over 500 ms, then a host change, then only waiting.
            window.update(cx, |_, window, _| window.blur())?;
            // Wait (bounded) for a REAL quiet interval; record what kept the
            // window painting meanwhile (frame requesters and transcript drivers).
            perf_counters::reset();
            let mut idle_age = None;
            for _ in 0..80 {
                settle(cx, 100).await;
                idle_age = window.update(cx, |shell, _, cx| {
                    shell.fixture_tab_painted_age_ms(PANE_B, cx)
                })?;
                if idle_age.is_some_and(|age| age > 700.0) {
                    break;
                }
            }
            let quiet_wait = perf_counters::snapshot();
            let idle_reached = idle_age.is_some_and(|age| age > 500.0);
            // If the window never went quiet, SIMULATE the idle classification
            // (disclosed in the readback): age B's paint stamp right before the
            // update. The real update below is observed either way.
            if !idle_reached {
                window.update(cx, |shell, _, cx| {
                    shell.fixture_age_tab_paint(PANE_B, 1000, cx)
                })?;
            }
            let b_doc = self.doc_host.open(PANE_B)?;
            let mut b_rows = b_doc.doc().read_entries()?;
            b_rows.push(assistant(
                "idle-marker",
                vec![text_part("IDLE-MARKER arrived while the pane was idle.")],
                9_000_000,
            ));
            perf_counters::reset();
            let injected = Instant::now();
            replace_atomically(b_doc.doc(), &b_rows)?;
            let b_scope = format!("pane:{PANE_B}");
            let mut idle_painted = None;
            for _ in 0..60 {
                settle(cx, 50).await;
                if painted_entry(&b_scope, "idle-marker").is_some_and(|t| t.contains("IDLE-MARKER"))
                {
                    idle_painted = Some(injected.elapsed().as_secs_f64() * 1000.0);
                    break;
                }
            }
            let after_update = perf_counters::snapshot();
            let deferred = after_update["transcript.sync_deferred"][b_scope.as_str()]["calls"]
                .as_u64()
                .unwrap_or(0);
            checks.check(
                "a visible pane classified idle paints its next update without input",
                idle_painted.is_some(),
                json!({
                    "realIdleReached": idle_reached,
                    "simulatedIdle": !idle_reached,
                    "idleAgeMs": idle_age,
                    "deferredSyncs": deferred,
                    "paintedAfterMs": idle_painted,
                }),
            );
            visibility.insert(
                "idleVisible".into(),
                json!({
                    "realIdleReached": idle_reached,
                    "simulatedIdle": !idle_reached,
                    "idleAgeMs": idle_age,
                    "paintedAfterMs": idle_painted,
                    "deferredSyncs": deferred,
                    "quietWaitCounters": quiet_wait,
                    "afterUpdateCounters": after_update,
                }),
            );
            // A hidden pane changed while hidden: no sync until it is revealed,
            // then its new row is painted.
            let hidden_doc = self.doc_host.open(HIDDEN)?;
            let mut hidden_rows = hidden_doc.doc().read_entries()?;
            hidden_rows.push(assistant(
                "reveal-marker",
                vec![text_part("REVEAL-MARKER written while hidden.")],
                9_000_000,
            ));
            perf_counters::reset();
            replace_atomically(hidden_doc.doc(), &hidden_rows)?;
            settle(cx, 800).await;
            let hidden_scope = format!("pane:{HIDDEN}");
            let while_hidden = perf_counters::snapshot();
            let hidden_syncs = while_hidden["transcript.sync"][hidden_scope.as_str()]["calls"]
                .as_u64()
                .unwrap_or(0);
            window.update(cx, |shell, _, cx| shell.fixture_workbench_open(HIDDEN, cx))?;
            let (revealed, _) = wait_painted(window, cx, &hidden_scope, "reveal-marker", |text| {
                text.contains("REVEAL-MARKER")
            })
            .await?;
            let reveal_ok = hidden_syncs == 0
                && revealed
                    .as_deref()
                    .is_some_and(|t| t.contains("REVEAL-MARKER"));
            checks.check(
                "a hidden pane defers its sync and paints its change when revealed",
                reveal_ok,
                json!({"syncsWhileHidden": hidden_syncs, "painted": revealed}),
            );
            let _ = shot(window, cx, self.output.join("08-hidden-revealed.png"));
            visibility.insert(
                "hiddenReveal".into(),
                json!({"pass": reveal_ok, "whileHidden": while_hidden}),
            );
            window.update(cx, |shell, _, cx| shell.fixture_workbench_open(MAIN, cx))?;
            settle(cx, 300).await;
            // Cache admission: lowered ceilings reject oversize chats (a 4,200
            // entry chat by bytes, and ONE entry carrying a 2 MiB child result,
            // whose count is tiny); default ceilings admit, and the next switch
            // replaces the one kept chat.
            let other_doc = self.doc_host.open(OTHER)?;
            let mut other_rows = other_doc.doc().read_entries()?;
            other_rows.push(assistant(
                "huge-child",
                vec![spawn_part(
                    "huge-child-part",
                    "done",
                    &"R".repeat(2 * 1024 * 1024),
                    1,
                )],
                8_000_000,
            ));
            replace_atomically(other_doc.doc(), &other_rows)?;
            let small_limits = (10_000usize, 40_000usize, 1024 * 1024usize);
            let set_limits =
                |limits: Option<(usize, usize, usize)>, cx: &mut AsyncApp| -> anyhow::Result<()> {
                    cx.update(|cx| {
                        self.state
                            .update(cx, |state, _| state.fixture_set_recent_limits(limits))
                    });
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_set_main_retained_limits(limits, cx)
                    })?;
                    Ok(())
                };
            let kept = |cx: &mut AsyncApp| -> anyhow::Result<Value> {
                let recent = cx.update(|cx| self.state.read(cx).fixture_recent_transcript());
                let rows = window.update(cx, |shell, _, cx| shell.fixture_main_retained(cx))?;
                Ok(json!({"recent": recent, "rows": rows}))
            };
            let rejected_by = |open: &Value, chat: &str, recent: &str, rows: &str| {
                let reject = |metric: &str, reason: &str| {
                    open["counters"][metric].as_object().is_some_and(|scopes| {
                        scopes
                            .keys()
                            .any(|scope| scope == &format!("{chat}:{reason}"))
                    })
                };
                reject("cache.recent_rejected", recent) && reject("cache.rows_rejected", rows)
            };
            let rejected = |open: &Value, chat: &str| rejected_by(open, chat, "bytes", "bytes");
            let mut admission = serde_json::Map::new();
            set_limits(Some(small_limits), cx)?;
            let to_fork = self.open_main(cx, FORK).await?;
            let after_main = kept(cx)?;
            let to_other = self.open_main(cx, OTHER).await?;
            let after_fork = kept(cx)?;
            let to_d = self.open_main(cx, PANE_D).await?;
            let after_huge = kept(cx)?;
            let oversize_ok = rejected(&to_fork, MAIN)
                && rejected(&to_other, FORK)
                && rejected(&to_d, OTHER)
                && after_main == json!({"recent": null, "rows": null})
                && after_fork == json!({"recent": null, "rows": null})
                && after_huge == json!({"recent": null, "rows": null});
            checks.check(
            "oversize chats are never kept (bytes ceiling), including one huge entry",
            oversize_ok,
            json!({"afterMain": after_main, "afterFork": after_fork, "afterHugeEntry": after_huge}),
        );
            admission.insert("oversize".into(), json!({"pass": oversize_ok, "toFork": to_fork["counters"], "toOther": to_other["counters"], "toD": to_d["counters"]}));
            // Count ceilings alone (bytes effectively unlimited): entries, then
            // parts (recent transcript) and rows (row cache).
            set_limits(Some((100, 1_000_000, 1usize << 30)), cx)?;
            let by_entries = self.open_main(cx, FORK).await?;
            let after_entries = kept(cx)?;
            set_limits(Some((1_000_000, 100, 1usize << 30)), cx)?;
            let by_parts = self.open_main(cx, PANE_D).await?;
            let after_parts = kept(cx)?;
            let counts_ok = rejected_by(&by_entries, PANE_D, "entries", "entries")
                && rejected_by(&by_parts, FORK, "parts", "rows")
                && after_entries == json!({"recent": null, "rows": null})
                && after_parts == json!({"recent": null, "rows": null});
            checks.check(
                "count ceilings reject on their own (entries, parts and rows)",
                counts_ok,
                json!({"afterEntries": after_entries, "afterParts": after_parts}),
            );
            admission.insert(
            "counts".into(),
            json!({"pass": counts_ok, "byEntries": by_entries["counters"], "byParts": by_parts["counters"]}),
        );
            set_limits(None, cx)?;
            self.open_main(cx, FORK).await?;
            let kept_d = kept(cx)?;
            let back_to_d = self.open_main(cx, PANE_D).await?;
            let kept_fork = kept(cx)?;
            let d_scope = format!("main:{PANE_D}");
            let reused =
                back_to_d["counters"]["transcript.row_cache_hit"][d_scope.as_str()]["items"]
                    .as_u64()
                    .unwrap_or(0);
            let d_painted = painted_entry(&d_scope, "tail-text");
            let within = |kept: &Value| {
                kept["recent"]["bytes"]
                    .as_u64()
                    .is_some_and(|b| b <= 32 * 1024 * 1024)
                    && kept["rows"]["bytes"]
                        .as_u64()
                        .is_some_and(|b| b <= 32 * 1024 * 1024)
            };
            let replace_ok = kept_d["recent"]["chat"] == PANE_D
                && kept_d["rows"]["chat"] == PANE_D
                && kept_fork["recent"]["chat"] == FORK
                && kept_fork["rows"]["chat"] == FORK
                && within(&kept_d)
                && within(&kept_fork)
                && reused as usize >= self.entries * 9 / 10
                && d_painted
                    .as_deref()
                    .is_some_and(|t| t.contains("TEXT-PANEPANE"));
            checks.check(
            "one chat is kept under its ceilings, replaced on the next switch, and reused on reopen",
            replace_ok,
            json!({"keptD": kept_d, "keptFork": kept_fork, "reusedRows": reused, "painted": d_painted}),
        );
            admission.insert(
                "replace".into(),
                json!({"pass": replace_ok, "reopenCounters": back_to_d["counters"]}),
            );
            visibility.insert("admission".into(), Value::Object(admission));
            phases.insert("cacheVisibility".into(), Value::Object(visibility));
            self.select(cx, MAIN);
            wait_wb(window, cx, "Main after the cache phase", |rb| {
                rb["loaded"] == true && rb["main"] == MAIN
            })
            .await?;
            mark(&self.output, "cache-visibility", "end");

            // 9. Post-measurement: drop ownership (failure case 6). A real file
            // dropped through GPUI's platform drop events at the centre of pane
            // B's painted bounds must stage in B's composer only.
            // Post-measurement correctness reads painted control bounds.
            perf_counters::set_probes_enabled(true);
            mark(&self.output, "drop", "start");
            let drop_file =
                std::env::temp_dir().join(format!("luna-perf-drop-{}.png", std::process::id()));
            std::fs::write(&drop_file, DROP_PNG)?;
            let attachments_of = |chat: &str, cx: &mut AsyncApp| -> anyhow::Result<Value> {
                let chat = chat.to_string();
                window.update(cx, move |shell, _, cx| {
                    shell
                        .fixture_workbench_composer(&chat, cx)
                        .map(|composer| composer.read(cx).fixture_attachments())
                        .unwrap_or(Value::Null)
                })
            };
            window.update(cx, |shell, window, cx| {
                shell.fixture_workbench_focus_input(PANE_B, window, cx)
            })?;
            timed(window, cx, true, |_, _| {}).await?;
            let rb = wb(window, cx)?;
            let b_pane = all_panes(&rb)
                .into_iter()
                .find(|pane| pane["active"] == PANE_B)
                .and_then(|pane| pane["id"].as_str().map(str::to_string));
            let b_bounds = b_pane.as_deref().and_then(|id| {
                rb["paneBounds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|entry| entry["id"] == id)
                    .map(|entry| entry["bounds"].clone())
            });
            let main_before = attachments_of(MAIN, cx)?;
            let main_draft_before =
                window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))?;
            let b_before = attachments_of(PANE_B, cx)?;
            let b_draft_before =
                window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(PANE_B, cx))?;
            // Typed-then-drop, deterministically: a real Left keystroke into
            // B's focused input immediately before Entered, in the same
            // callback (keyboard modality before Entered, pointer after).
            let mut typed_then_drop = Value::Null;
            // Evidence for each step of the drop (opt-in counters): the
            // handler that fired, what `add_paths` staged, and whether an
            // earlier drag was still active (GPUI's Entered installs the
            // file payload only when no drag is active).
            let drop_counters = || {
                let snapshot = perf_counters::snapshot();
                json!({
                    "handler": snapshot["drop.external"],
                    "addPaths": snapshot["composer.add_paths"],
                    "blocked": snapshot["composer.add_staged_blocked"],
                })
            };
            let counters_before = drop_counters();
            let mut drop_point = None;
            let mut drop_steps = Vec::new();
            if let Some(bounds) = &b_bounds {
                let get = |key: &str| bounds[key].as_f64().unwrap_or(0.0) as f32;
                let (x, y) = (get("x") + get("w") / 2.0, get("y") + get("h") / 2.0);
                let at = gpui::point(px(x), px(y));
                drop_point = Some(json!({"x": x, "y": y}));
                let paths = gpui::ExternalPaths(vec![drop_file.clone()].into());
                // The platform's order with a presented frame between the
                // events (an OS drag enters, hovers, then drops), so hit
                // testing and drag-over state come from real paints.
                let events = [
                    (
                        "entered",
                        gpui::FileDropEvent::Entered {
                            position: at,
                            paths,
                        },
                    ),
                    ("pending", gpui::FileDropEvent::Pending { position: at }),
                    ("submit", gpui::FileDropEvent::Submit { position: at }),
                ];
                for (step, event) in events {
                    // Keyboard modality (the last input a keystroke) makes
                    // GPUI report no hitbox hovered, which an OS drop must
                    // not inherit.
                    let keyboard_before = AnyWindowHandle::from(window)
                        .update(cx, |_, window, _| window.last_input_was_keyboard())?;
                    let drag_before = AnyWindowHandle::from(window)
                        .update(cx, |_, _, cx| cx.has_active_drag())?;
                    let left = (step == "entered")
                        .then(|| Keystroke::parse("left"))
                        .transpose()?;
                    let typed =
                        AnyWindowHandle::from(window).update(cx, move |_, window, cx| {
                            let typed = left.map(|left| {
                                window.dispatch_keystroke(left, cx);
                                window.last_input_was_keyboard()
                            });
                            window.dispatch_event(gpui::PlatformInput::FileDrop(event), cx);
                            typed.map(|keyboard| (keyboard, window.last_input_was_keyboard()))
                        })?;
                    if let Some((keyboard_after_key, keyboard_after_entered)) = typed {
                        typed_then_drop = json!({
                            "keyboardAfterKeystroke": keyboard_after_key,
                            "keyboardAfterEntered": keyboard_after_entered,
                        });
                    }
                    let drag_after = AnyWindowHandle::from(window)
                        .update(cx, |_, _, cx| cx.has_active_drag())?;
                    let keyboard_after = AnyWindowHandle::from(window)
                        .update(cx, |_, window, _| window.last_input_was_keyboard())?;
                    timed(window, cx, true, |_, _| {}).await?;
                    drop_steps.push(json!({
                        "step": step,
                        "activeDragBefore": drag_before,
                        "activeDragAfter": drag_after,
                        "keyboardModalityBefore": keyboard_before,
                        "keyboardModalityAfter": keyboard_after,
                        "counters": drop_counters(),
                    }));
                }
            }
            settle(cx, 400).await;
            timed(window, cx, true, |_, _| {}).await?;
            let b_after = attachments_of(PANE_B, cx)?;
            let main_after = attachments_of(MAIN, cx)?;
            let main_draft_after =
                window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))?;
            let _ = shot(window, cx, self.output.join("09-drop-pane-b.png"));
            let names = |value: &Value| value["names"].as_array().map(Vec::len).unwrap_or(0);
            let b_draft_after =
                window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(PANE_B, cx))?;
            let typed_ok = typed_then_drop["keyboardAfterKeystroke"] == true
                && typed_then_drop["keyboardAfterEntered"] == false
                && b_draft_after == b_draft_before;
            let drop_ok = b_bounds.is_some()
                && typed_ok
                && names(&b_after) == names(&b_before) + 1
                && main_after == main_before
                && main_draft_after == main_draft_before;
            checks.check(
            "typed then dropped: after a real keystroke in B, an external file dropped on pane B stages in B only; both drafts unchanged",
            drop_ok,
            json!({
                "paneBounds": b_bounds,
                "dropPoint": drop_point,
                "paneBBefore": b_before,
                "paneBAfter": b_after,
                "mainBefore": main_before,
                "mainAfter": main_after,
                "mainDraft": [main_draft_before, main_draft_after],
                "typedThenDrop": typed_then_drop,
                "paneBDraft": [b_draft_before, b_draft_after],
                "countersBefore": counters_before,
                "steps": drop_steps,
                "countersAfter": drop_counters(),
            }),
        );
            // Internal drag: a workspace file row in Main's Files surface, pressed
            // and moved with the button held (production on_drag), released at
            // the centre of pane B. B's draft must gain the file mention.
            let project = cx
                .update(|cx| {
                    self.state
                        .read(cx)
                        .spaces
                        .iter()
                        .find(|space| space.id == PROJECT)
                        .map(|space| space.path.clone())
                })
                .ok_or_else(|| anyhow::anyhow!("the fixture project"))?;
            std::fs::write(
                PathBuf::from(&project).join("drop-note.md"),
                "# Drop note\n",
            )?;
            // Only probes painted after the Files surface opens count.
            perf_counters::clear_bounds("file-row:");
            window.update(cx, |shell, window, cx| {
                shell.fixture_open_surface("files", window, cx)
            })?;
            let mut source = None;
            for _ in 0..80 {
                timed(window, cx, true, |_, _| {}).await?;
                source = perf_counters::bounds("file-row:drop-note.md");
                if source.is_some() {
                    break;
                }
                settle(cx, 100).await;
            }
            // The drop point must be a pointer-visible point of pane B: the
            // centre of B's painted bounds INTERSECTED with the workbench viewport
            // (B may extend behind the Files pane); none visible is a failure.
            let rb = wb(window, cx)?;
            let viewport = rb["rootBounds"].clone();
            let b_painted = all_panes(&rb)
                .into_iter()
                .find(|pane| pane["active"] == PANE_B)
                .and_then(|pane| pane["id"].as_str().map(str::to_string))
                .and_then(|id| {
                    rb["paneBounds"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|entry| entry["id"] == id.as_str())
                        .map(|entry| entry["bounds"].clone())
                });
            let rect = |value: &Value| {
                let get = |key: &str| value[key].as_f64().unwrap_or(0.0) as f32;
                (get("x"), get("y"), get("w"), get("h"))
            };
            let visible_b = b_painted.as_ref().and_then(|pane| {
                let (bx, by, bw, bh) = rect(pane);
                let (vx, vy, vw, vh) = rect(&viewport);
                let (left, top) = (bx.max(vx), by.max(vy));
                let (right, bottom) = ((bx + bw).min(vx + vw), (by + bh).min(vy + vh));
                (right - left > 8.0 && bottom - top > 8.0).then_some((
                    left,
                    top,
                    right - left,
                    bottom - top,
                ))
            });
            let target = visible_b.map(|(x, y, w, h)| (x + w / 2.0, y + h / 2.0));
            let b_draft_before =
                window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(PANE_B, cx))?;
            let main_draft_before_drag =
                window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))?;
            let mouse = |event: gpui::PlatformInput, cx: &mut AsyncApp| -> anyhow::Result<()> {
                AnyWindowHandle::from(window).update(cx, move |_, window, cx| {
                    window.dispatch_event(event, cx);
                })?;
                Ok(())
            };
            if let (Some([sx, sy, sw, sh]), Some((tx, ty))) = (source, target) {
                let from = (sx + sw / 2.0, sy + sh / 2.0);
                let at = |(x, y): (f32, f32)| gpui::point(px(x), px(y));
                mouse(
                    gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                        position: at(from),
                        pressed_button: None,
                        modifiers: Default::default(),
                    }),
                    cx,
                )?;
                timed(window, cx, true, |_, _| {}).await?;
                mouse(
                    gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                        button: gpui::MouseButton::Left,
                        position: at(from),
                        modifiers: Default::default(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                )?;
                for step in 1..=12 {
                    let t = step as f32 / 12.0;
                    mouse(
                        gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                            position: at((from.0 + (tx - from.0) * t, from.1 + (ty - from.1) * t)),
                            pressed_button: Some(gpui::MouseButton::Left),
                            modifiers: Default::default(),
                        }),
                        cx,
                    )?;
                    timed(window, cx, true, |_, _| {}).await?;
                }
                mouse(
                    gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                        button: gpui::MouseButton::Left,
                        position: at((tx, ty)),
                        modifiers: Default::default(),
                        click_count: 1,
                    }),
                    cx,
                )?;
                settle(cx, 400).await;
                timed(window, cx, true, |_, _| {}).await?;
            }
            let b_draft_after =
                window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(PANE_B, cx))?;
            let main_draft_after_drag =
                window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(MAIN, cx))?;
            let _ = shot(window, cx, self.output.join("09-workspace-drag-pane-b.png"));
            let drag_ok = source.is_some()
                && target.is_some()
                && b_draft_after
                    .as_deref()
                    .is_some_and(|d| d.contains("drop-note.md"))
                && !b_draft_before
                    .as_deref()
                    .is_some_and(|d| d.contains("drop-note.md"))
                && main_draft_after_drag == main_draft_before_drag;
            checks.check(
            "a workspace file dragged onto pane B is mentioned in B's draft only; Main's draft unchanged",
            drag_ok,
            json!({
                "source": source,
                "target": target,
                "paneBPainted": b_painted,
                "workbenchViewport": viewport,
                "paneBVisible": visible_b.map(|(x, y, w, h)| json!({"x": x, "y": y, "w": w, "h": h})),
                "failure": if target.is_none() {
                    Some("no pointer-visible point of pane B inside the workbench viewport")
                } else {
                    None
                },
                "paneBDraft": [b_draft_before, b_draft_after],
                "mainDraft": [main_draft_before_drag, main_draft_after_drag],
            }),
        );
            phases.insert(
                "drop".into(),
                json!({"pass": drop_ok, "workspaceDrag": drag_ok}),
            );
            let _ = std::fs::remove_file(&drop_file);
            mark(&self.output, "drop", "end");
        }
        MEASURING.store(false, Ordering::Relaxed);

        // 10. Post-measurement REPRO: native window resize (full -> half ->
        // full, narrow/tall and wide/short) with 1, 2 and 4 groups, Files
        // open, and the left sidebar collapsed. Settled frames must fit the
        // workbench viewport (no outer horizontal scroll), scale pane widths
        // in proportion, keep pills inside their panes, and return to wide
        // with the same chats, drafts, focus and saved layout.
        mark(&self.output, "resize", "start");
        perf_counters::set_probes_enabled(true);
        // Its timings need the test window active: activate this app and
        // window once, then wait (bounded) for the platform to report it
        // active. Never assumed: a failure is recorded, and samples keep
        // their own `windowActive`.
        let activation = {
            let started = Instant::now();
            let before = AnyWindowHandle::from(window)
                .update(cx, |_, window, _| window.is_window_active())?;
            if !before {
                AnyWindowHandle::from(window).update(cx, |_, window, cx| {
                    cx.activate(true);
                    window.activate_window();
                })?;
            }
            let mut active = before;
            while !active && started.elapsed() < Duration::from_secs(3) {
                settle(cx, 50).await;
                active = AnyWindowHandle::from(window)
                    .update(cx, |_, window, _| window.is_window_active())?;
            }
            json!({
                "activeBefore": before,
                "activated": !before,
                "active": active,
                "waitedMs": started.elapsed().as_secs_f64() * 1000.0,
            })
        };
        checks.check(
            "resize: the test window is active before its timings",
            activation["active"] == true,
            activation.clone(),
        );
        let mut resize = serde_json::Map::new();
        fn strip_status(value: &Value) -> Value {
            match value {
                Value::Object(fields) => Value::Object(
                    fields
                        .iter()
                        .filter(|(key, _)| key.as_str() != "status")
                        .map(|(key, value)| (key.clone(), strip_status(value)))
                        .collect(),
                ),
                Value::Array(items) => Value::Array(items.iter().map(strip_status).collect()),
                _ => value.clone(),
            }
        }
        let resize_to = |w: f32, h: f32, cx: &mut AsyncApp| -> anyhow::Result<()> {
            AnyWindowHandle::from(window).update(cx, move |_, window, _| {
                window.resize(size(px(w), px(h)));
            })?;
            Ok(())
        };
        // Geometry of the current frame: viewport, scroll, panes (chat ->
        // bounds), pills for Main and B, layout tree and revision.
        let geometry = |cx: &mut AsyncApp, main: &str| -> anyhow::Result<Value> {
            let rb = wb(window, cx)?;
            let frame = window.update(cx, |shell, _, cx| shell.fixture_frame_readback(cx))?;
            let panes: Vec<Value> = all_panes(&rb)
                .into_iter()
                .map(|pane| {
                    let id = pane["id"].clone();
                    let bounds = rb["paneBounds"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|entry| entry["id"] == id)
                        .map(|entry| entry["bounds"].clone())
                        .unwrap_or(Value::Null);
                    json!({"id": id, "active": pane["active"], "tabs": pane["tabs"], "bounds": bounds})
                })
                .collect();
            let main = main.to_string();
            let pills = window.update(cx, move |shell, _, cx| {
                json!({
                    "main": shell.fixture_composer_surface(&main, cx),
                    "paneB": shell.fixture_composer_surface(PANE_B, cx),
                })
            })?;
            // Layout probes painted since the last clear (composer controls,
            // transcript text column, strip controls, menu rows): settled
            // states read a fresh capture, immediate ones what the resize
            // frame itself painted.
            let probes: serde_json::Map<String, Value> = perf_counters::probes("")
                .into_iter()
                .map(|(name, bounds)| {
                    (
                        name,
                        json!({"x": bounds[0], "y": bounds[1], "w": bounds[2], "h": bounds[3]}),
                    )
                })
                .collect();
            Ok(json!({
                "window": frame["viewportWidth"],
                "viewport": rb["rootBounds"],
                "canvas": rb["canvasBounds"],
                "scroll": rb["canvasScroll"],
                "panes": panes,
                "pills": pills,
                "probes": probes,
                "tabBounds": rb["tabBounds"],
                "plusBounds": rb["plusBounds"],
                "layout": strip_status(&rb["root"]),
                "revision": rb["revision"],
                "rightOpen": frame["rightOpen"],
                "sidebarCollapsed": frame["sidebarCollapsed"],
            }))
        };
        let num = |value: &Value, key: &str| value[key].as_f64().unwrap_or(f64::NAN);
        // Settled-frame checks (one list of problems per state).
        let problems = |g: &Value, main: &str| -> Vec<String> {
            let mut out = Vec::new();
            let viewport = &g["viewport"];
            let (vx, vw) = (num(viewport, "x"), num(viewport, "w"));
            if !vx.is_finite() || !vw.is_finite() || vw <= 0.0 {
                out.push(format!("missing viewport geometry: {viewport}"));
            }
            let scroll = &g["scroll"];
            if !num(scroll, "maxX").is_finite() || !num(scroll, "x").is_finite() {
                out.push(format!("missing canvas scroll geometry: {scroll}"));
            } else if num(scroll, "maxX") > 0.5 || num(scroll, "x").abs() > 0.5 {
                out.push(format!("outer horizontal scroll {}", g["scroll"]));
            }
            for pane in g["panes"].as_array().into_iter().flatten() {
                let b = &pane["bounds"];
                if !num(b, "x").is_finite() || !num(b, "w").is_finite() || num(b, "w") <= 0.0 {
                    out.push(format!("missing pane geometry: {pane}"));
                } else if num(b, "x") < vx - 1.0 || num(b, "x") + num(b, "w") > vx + vw + 1.0 {
                    out.push(format!(
                        "pane {} exceeds the viewport: {b} vs {viewport}",
                        pane["active"]
                    ));
                }
            }
            for (label, chat) in [("main", main), ("paneB", PANE_B)] {
                let Some(pill) = g["pills"][label].as_array() else {
                    continue;
                };
                let pane = g["panes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|pane| pane["active"] == chat);
                if let (Some(pane), [x, _, w, _]) = (pane, pill.as_slice()) {
                    let (x, w) = (x.as_f64().unwrap_or(0.0), w.as_f64().unwrap_or(0.0));
                    let b = &pane["bounds"];
                    if x < num(b, "x") - 1.0 || x + w > num(b, "x") + num(b, "w") + 1.0 {
                        out.push(format!("{label} pill outside its pane: {pill:?} vs {b}"));
                    }
                }
            }
            // Narrow-content rules N1-N6 (slice-6-ui.md), per visible group.
            let probe = |name: &str| -> Option<(f64, f64, f64, f64)> {
                let b = g["probes"].get(name)?;
                Some((num(b, "x"), num(b, "y"), num(b, "w"), num(b, "h")))
            };
            let overlap = |a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| {
                let w = (a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0);
                let h = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
                w > 0.5 && h > 0.5
            };
            let inside_x = |a: (f64, f64, f64, f64), x: f64, w: f64| {
                a.0 >= x - 0.5 && a.0 + a.2 <= x + w + 0.5
            };
            for pane in g["panes"].as_array().into_iter().flatten() {
                let Some(chat) = pane["active"].as_str() else {
                    continue;
                };
                let b = &pane["bounds"];
                let (px_, pw) = (num(b, "x"), num(b, "w"));
                if !pw.is_finite() || pw <= 0.0 {
                    continue;
                }
                let scope = if chat == main {
                    format!("main:{chat}")
                } else {
                    format!("pane:{chat}")
                };
                let composer = format!("composer:{scope}");
                // N4: the pill fits its group and uses most of a narrow one.
                match probe(&format!("{composer}:pill")) {
                    None => out.push(format!("{chat}: composer pill not painted")),
                    Some(pill) => {
                        if !inside_x(pill, px_, pw) {
                            out.push(format!(
                                "{chat}: pill {pill:?} outside its group ({px_}, {pw})"
                            ));
                        }
                        if pw < 240.0 && pill.2 < 0.8 * pw {
                            out.push(format!(
                                "{chat}: pill {} wide in a {pw} group (narrow gutters must shrink)",
                                pill.2
                            ));
                        }
                        // N1/N2: controls inside the pill, none overlapping;
                        // Send always shown; attach/model/access shown or
                        // reachable through an overflow trigger.
                        let more = probe(&format!("{composer}:more"));
                        let mut shown = Vec::new();
                        for part in ["send", "attach", "model", "access"] {
                            match probe(&format!("{composer}:{part}")) {
                                Some(control) if control.2 >= 12.0 => {
                                    if !inside_x(control, pill.0, pill.2) {
                                        out.push(format!(
                                            "{chat}: {part} {control:?} outside the pill {pill:?}"
                                        ));
                                    }
                                    shown.push((part, control));
                                }
                                _ if part == "send" => out.push(format!("{chat}: Send not shown")),
                                _ if more.is_none() => out.push(format!(
                                    "{chat}: {part} neither shown nor in an overflow menu"
                                )),
                                _ => {}
                            }
                        }
                        if let Some(more) = more {
                            if !inside_x(more, pill.0, pill.2) {
                                out.push(format!(
                                    "{chat}: overflow trigger {more:?} outside the pill"
                                ));
                            }
                            shown.push(("more", more));
                        }
                        for (i, (a_name, a)) in shown.iter().enumerate() {
                            for (b_name, b2) in shown.iter().skip(i + 1) {
                                if overlap(*a, *b2) {
                                    out.push(format!(
                                        "{chat}: {a_name} {a:?} overlaps {b_name} {b2:?}"
                                    ));
                                }
                            }
                        }
                        // N3: usable input width.
                        match probe(&format!("{composer}:input")) {
                            None => out.push(format!("{chat}: input not painted")),
                            Some(input) => {
                                let need = (0.5 * pill.2).max(40.0_f64.min(pw - 24.0));
                                if input.2 < need {
                                    out.push(format!(
                                        "{chat}: input {} wide (needs {need}) in pill {}",
                                        input.2, pill.2
                                    ));
                                }
                            }
                        }
                    }
                }
                // N5: the transcript text column uses most of a narrow group.
                if let Some(column) = probe(&format!("transcript:{scope}:column")) {
                    if !inside_x(column, px_, pw) {
                        out.push(format!(
                            "{chat}: transcript column {column:?} outside its group"
                        ));
                    }
                    if pw < 400.0 && column.2 < 0.6 * pw {
                        out.push(format!(
                            "{chat}: transcript column {} wide in a {pw} group",
                            column.2
                        ));
                    }
                }
                // N6: the active tab keeps a usable, identifiable hit target;
                // strip controls fit the group and never cover it.
                let pane_id = pane["id"].as_str().unwrap_or_default();
                let strip = g["tabBounds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|strip| strip["pane"] == pane_id);
                let active_tab = strip.and_then(|strip| {
                    strip["tabs"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|tab| tab["chatId"] == chat)
                        .map(|tab| {
                            let v = &tab["visible"];
                            (num(v, "x"), num(v, "y"), num(v, "w"), num(v, "h"))
                        })
                });
                match active_tab {
                    None => out.push(format!("{chat}: active tab not painted")),
                    Some(tab) => {
                        if tab.2 < 48.0_f64.min(0.4 * pw) {
                            out.push(format!(
                                "{chat}: active tab only {} visible in a {pw} group",
                                tab.2
                            ));
                        }
                        let plus = g["plusBounds"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|entry| entry["pane"] == pane_id)
                            .map(|entry| {
                                let p = &entry["bounds"];
                                (num(p, "x"), num(p, "y"), num(p, "w"), num(p, "h"))
                            });
                        let controls = [
                            ("plus", plus),
                            ("split", probe(&format!("strip:{pane_id}:split"))),
                            ("close", probe(&format!("strip:{pane_id}:close"))),
                            ("more", probe(&format!("strip:{pane_id}:more"))),
                        ];
                        for (name, control) in controls {
                            let Some(control) = control.filter(|c| c.2 > 0.5) else {
                                continue;
                            };
                            if !inside_x(control, px_, pw) {
                                out.push(format!(
                                    "{chat}: strip {name} {control:?} outside its group"
                                ));
                            }
                            if overlap(control, tab) {
                                out.push(format!(
                                    "{chat}: strip {name} {control:?} covers the active tab {tab:?}"
                                ));
                            }
                        }
                    }
                }
            }
            out
        };
        let shares = |g: &Value| -> Vec<f64> {
            let panes = g["panes"].as_array().cloned().unwrap_or_default();
            let total: f64 = num(&g["viewport"], "w");
            panes
                .iter()
                .map(|pane| num(&pane["bounds"], "w") / total)
                .collect()
        };
        let sizes: [(&str, f32, f32); 7] = [
            ("wide", WIDTH, HEIGHT),
            ("half800", 800.0, HEIGHT),
            ("wideAgain", WIDTH, HEIGHT),
            ("half864", 864.0, HEIGHT),
            ("wideAgain2", WIDTH, HEIGHT),
            ("short560", WIDTH, 560.0),
            ("wideFinal", WIDTH, HEIGHT),
        ];
        // A fresh Main (the small chat) with exactly one group.
        self.select(cx, OTHER);
        wait_wb(window, cx, "the resize Main", |rb| {
            rb["loaded"] == true && rb["main"] == OTHER
        })
        .await?;
        let rb = wb(window, cx)?;
        for pane in all_panes(&rb).into_iter().skip(1) {
            let id = pane["id"].as_str().unwrap_or_default().to_string();
            window.update(cx, |shell, _, cx| {
                shell.fixture_workbench_close_pane(&id, cx)
            })?;
            settle(cx, 200).await;
        }
        wait_wb(window, cx, "one group", |rb| all_panes(rb).len() == 1).await?;
        if frame(window, cx)?["rightOpen"] == true {
            window.update(cx, |shell, _, cx| shell.fixture_toggle_sidebar(true, cx))?;
            settle(cx, 500).await;
        }
        let variants: [(&str, usize, bool, bool); 5] = [
            ("groups1", 1, false, false),
            ("groups2", 2, false, false),
            ("groups4", 4, false, false),
            ("groups4Files", 4, true, false),
            ("groups2SidebarCollapsed", 2, false, true),
        ];
        let mut current_groups = 1usize;
        for (variant, groups, files, collapse_sidebar) in variants {
            // Reach the group count (2: B beside the first; 4: C beside the
            // first, D beside the last; back to 2: close the last two).
            while current_groups < groups {
                let rb = wb(window, cx)?;
                let all = all_panes(&rb);
                let (add, at) = match current_groups {
                    1 => (PANE_B, 0),
                    2 => (PANE_C, 0),
                    _ => (PANE_D, all.len() - 1),
                };
                let target = all[at.min(all.len() - 1)]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let before = all.len();
                window.update(cx, |shell, _, cx| {
                    shell.fixture_workbench_open_beside(add, &target, cx)
                })?;
                wait_wb(window, cx, "a resize group", |rb| {
                    all_panes(rb).len() == before + 1
                })
                .await?;
                current_groups += 1;
            }
            while current_groups > groups {
                // Keep the resize Main and pane B: close C or D.
                let rb = wb(window, cx)?;
                let extra = all_panes(&rb)
                    .into_iter()
                    .rev()
                    .find(|pane| pane["active"] != OTHER && pane["active"] != PANE_B)
                    .and_then(|pane| pane["id"].as_str().map(str::to_string))
                    .ok_or_else(|| anyhow::anyhow!("no extra group to close: {rb}"))?;
                let before = all_panes(&rb).len();
                window.update(cx, |shell, _, cx| {
                    shell.fixture_workbench_close_pane(&extra, cx)
                })?;
                wait_wb(window, cx, "fewer resize groups", |rb| {
                    all_panes(rb).len() + 1 == before
                })
                .await?;
                current_groups -= 1;
            }
            let right_open = frame(window, cx)?["rightOpen"] == true;
            if files && !right_open {
                window.update(cx, |shell, window, cx| {
                    shell.fixture_open_surface("files", window, cx)
                })?;
            } else if !files && right_open {
                window.update(cx, |shell, _, cx| shell.fixture_toggle_sidebar(true, cx))?;
            }
            let collapsed = frame(window, cx)?["sidebarCollapsed"] == true;
            if collapse_sidebar != collapsed {
                window.update(cx, |shell, _, cx| shell.fixture_toggle_sidebar(false, cx))?;
            }
            settle(cx, 600).await;
            // Drafts and focus that must survive the cycle.
            window.update(cx, |shell, _, cx| {
                shell.fixture_workbench_type(OTHER, "resize draft main", false, cx)
            })?;
            if groups >= 2 {
                window.update(cx, |shell, _, cx| {
                    shell.fixture_workbench_type(PANE_B, "resize draft b", false, cx)
                })?;
                window.update(cx, |shell, window, cx| {
                    shell.fixture_workbench_focus_input(PANE_B, window, cx)
                })?;
            } else {
                window.update(cx, |shell, window, cx| {
                    shell.fixture_workbench_focus_input(OTHER, window, cx)
                })?;
            }
            let drafts = |cx: &mut AsyncApp| -> anyhow::Result<Value> {
                Ok(json!({
                    "main": window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(OTHER, cx))?,
                    "paneB": window.update(cx, |shell, _, cx| shell.fixture_workbench_draft(PANE_B, cx))?,
                    "keyboard": window.update(cx, |shell, window, cx| shell.fixture_workbench_keyboard_chat(window, cx))?,
                }))
            };
            let mut states = Vec::new();
            let mut wide_shares: Option<Vec<f64>> = None;
            let mut start: Option<(Value, Value)> = None;
            let mut failures = Vec::new();
            for (label, w, h) in sizes {
                perf_counters::clear_bounds("");
                resize_to(w, h, cx)?;
                let immediate_timing = timed(window, cx, true, |_, _| {}).await?;
                let immediate = geometry(cx, OTHER)?;
                settle(cx, 700).await;
                let settled_timing = timed(window, cx, true, |_, _| {}).await?;
                capture_probes(window, cx).await?;
                let settled = geometry(cx, OTHER)?;
                let _ = shot(
                    window,
                    cx,
                    self.output.join(format!("10-resize-{variant}-{label}.png")),
                );
                let mut state_problems = problems(&settled, OTHER);
                let got = shares(&settled);
                if let Some(expected) = &wide_shares {
                    // Proportions scale with the available width.
                    let off = expected.len() != got.len()
                        || expected.iter().zip(&got).any(|(a, b)| (a - b).abs() > 0.02);
                    if off {
                        state_problems.push(format!("pane shares {got:?} vs wide {expected:?}"));
                    }
                } else {
                    wide_shares = Some(got.clone());
                }
                if label == "wide" {
                    start = Some((settled.clone(), drafts(cx)?));
                } else if label.starts_with("wide") {
                    if let Some((first, first_drafts)) = &start {
                        let now = drafts(cx)?;
                        if settled["layout"] != first["layout"] {
                            state_problems.push("saved layout changed".into());
                        }
                        if settled["revision"] != first["revision"] {
                            state_problems.push(format!(
                                "layout revision {} -> {}",
                                first["revision"], settled["revision"]
                            ));
                        }
                        if &now != first_drafts {
                            state_problems.push(format!("drafts/focus {now} vs {first_drafts}"));
                        }
                    }
                }
                for problem in &state_problems {
                    failures.push(format!("{label}: {problem}"));
                }
                states.push(json!({
                    "label": label,
                    "size": [w, h],
                    "immediate": immediate,
                    "settled": settled,
                    "immediateTiming": immediate_timing,
                    "settledTiming": settled_timing,
                    "problems": state_problems,
                }));
            }
            checks.check(
                &format!("resize {variant}: fits the viewport, scales evenly, keeps chats/drafts/focus/layout"),
                failures.is_empty(),
                json!({"failures": failures}),
            );
            resize.insert(
                variant.into(),
                json!({"states": states, "failures": failures}),
            );
            if variant == "groups4" {
                // Acceptance (slice 6 narrow fixes): real pointer input on the
                // NEW compact controls at half width: B's composer More tray
                // (model control -> only B's picker; Escape dismisses without
                // send, stop or draft change) and B's compact strip Group
                // actions (open, read its rows, dismiss; nothing created or
                // closed).
                resize_to(800.0, HEIGHT, cx)?;
                settle(cx, 700).await;
                capture_probes(window, cx).await?;
                let center = |g: &Value, name: &str| -> Option<(f32, f32)> {
                    let b = g["probes"].get(name)?;
                    let (x, y, w, h) = (num(b, "x"), num(b, "y"), num(b, "w"), num(b, "h"));
                    (w > 0.5 && h > 0.5).then(|| ((x + w / 2.0) as f32, (y + h / 2.0) as f32))
                };
                let state = |cx: &mut AsyncApp| -> anyhow::Result<Value> {
                    Ok(window.update(cx, |shell, window, cx| {
                        json!({
                            "drafts": {
                                "main": shell.fixture_workbench_draft(OTHER, cx),
                                "paneB": shell.fixture_workbench_draft(PANE_B, cx),
                            },
                            "sending": {
                                "main": shell.fixture_workbench_sending(OTHER, cx),
                                "paneB": shell.fixture_workbench_sending(PANE_B, cx),
                            },
                            "models": {
                                "main": shell.fixture_workbench_model(OTHER, cx),
                                "paneB": shell.fixture_workbench_model(PANE_B, cx),
                            },
                            "trays": {
                                "main": shell.fixture_workbench_controls_tray(OTHER, cx),
                                "paneB": shell.fixture_workbench_controls_tray(PANE_B, cx),
                            },
                            "menu": shell.fixture_workbench_menu(),
                            "escapeLog": shell.fixture_workbench_escape_log().len(),
                            "keyboard": shell.fixture_workbench_keyboard_chat(window, cx),
                        })
                    })?)
                };
                let escape = |cx: &mut AsyncApp| -> anyhow::Result<()> {
                    let key = Keystroke::parse("escape")?;
                    AnyWindowHandle::from(window).update(cx, move |_, window, cx| {
                        window.dispatch_keystroke(key, cx);
                    })?;
                    Ok(())
                };
                // Nothing sent, stopped, re-modeled or edited since `base`.
                let unchanged = |base: &Value, now: &Value, step: &str, out: &mut Vec<String>| {
                    if now["drafts"] != base["drafts"] {
                        out.push(format!(
                            "{step}: drafts {} vs {}",
                            now["drafts"], base["drafts"]
                        ));
                    }
                    if now["sending"]["main"] == true || now["sending"]["paneB"] == true {
                        out.push(format!("{step}: a send started {}", now["sending"]));
                    }
                    if now["escapeLog"] != base["escapeLog"] {
                        out.push(format!("{step}: Escape reached the stop path"));
                    }
                    for chat in ["main", "paneB"] {
                        if now["models"][chat][0] != base["models"][chat][0] {
                            out.push(format!("{step}: {chat} model changed"));
                        }
                    }
                };
                let g0 = geometry(cx, OTHER)?;
                let s0 = state(cx)?;
                let b_more = format!("composer:pane:{PANE_B}:more");
                let b_model = format!("composer:pane:{PANE_B}:model");
                let mut tray_failures = Vec::new();
                let mut tray_steps = vec![json!({"step": "before", "state": s0})];
                match center(&g0, &b_more) {
                    None => tray_failures.push(format!(
                        "B's compact composer More not painted at 800 wide: {}",
                        g0["probes"]
                    )),
                    Some((x, y)) => {
                        click_at(window, cx, x, y).await?;
                        let g1 = geometry(cx, OTHER)?;
                        let s1 = state(cx)?;
                        let _ = shot(window, cx, self.output.join("10-accept-tray-open.png"));
                        if s1["trays"]["paneB"] != true {
                            tray_failures.push("clicking B's More did not open B's tray".into());
                        }
                        if s1["trays"]["main"] == true {
                            tray_failures.push("clicking B's More opened Main's tray".into());
                        }
                        unchanged(&s0, &s1, "tray open", &mut tray_failures);
                        tray_steps
                            .push(json!({"step": "trayOpen", "state": s1, "probes": g1["probes"]}));
                        match center(&g1, &b_model) {
                            None => tray_failures
                                .push("B's model control not painted in the open tray".into()),
                            Some((x, y)) => {
                                click_at(window, cx, x, y).await?;
                                let s2 = state(cx)?;
                                let _ = shot(
                                    window,
                                    cx,
                                    self.output.join("10-accept-tray-model-picker.png"),
                                );
                                if s2["models"]["paneB"][1] != true {
                                    tray_failures
                                        .push("B's model picker did not open from the tray".into());
                                }
                                if s2["models"]["main"][1] == true {
                                    tray_failures.push("Main's model picker opened".into());
                                }
                                unchanged(&s0, &s2, "picker open", &mut tray_failures);
                                tray_steps.push(json!({"step": "pickerOpen", "state": s2}));
                                // Escape: the picker first, then the tray.
                                escape(cx)?;
                                settle(cx, 300).await;
                                capture_probes(window, cx).await?;
                                let s3 = state(cx)?;
                                if s3["models"]["paneB"][1] == true {
                                    tray_failures.push("Escape left B's picker open".into());
                                }
                                unchanged(&s0, &s3, "first Escape", &mut tray_failures);
                                tray_steps.push(json!({"step": "escapePicker", "state": s3}));
                                escape(cx)?;
                                settle(cx, 300).await;
                                capture_probes(window, cx).await?;
                                let g4 = geometry(cx, OTHER)?;
                                let s4 = state(cx)?;
                                let _ = shot(
                                    window,
                                    cx,
                                    self.output.join("10-accept-tray-dismissed.png"),
                                );
                                if s4["trays"]["paneB"] == true || s4["trays"]["main"] == true {
                                    tray_failures.push("the second Escape left a tray open".into());
                                }
                                if s4["models"]["paneB"][1] == true
                                    || s4["models"]["main"][1] == true
                                {
                                    tray_failures
                                        .push("a model picker is open after Escape".into());
                                }
                                if center(&g4, &b_model).is_some() {
                                    tray_failures.push(
                                        "B's tray controls still painted after Escape".into(),
                                    );
                                }
                                if center(&g4, &b_more).is_none() {
                                    tray_failures.push("B's More disappeared after Escape".into());
                                }
                                unchanged(&s0, &s4, "second Escape", &mut tray_failures);
                                tray_steps.push(json!({"step": "escapeTray", "state": s4, "probes": g4["probes"]}));
                            }
                        }
                    }
                }
                checks.check(
                    "narrow acceptance: B's compact tray opens only B's model picker; Escape dismisses without send/stop/draft change",
                    tray_failures.is_empty(),
                    json!({"failures": tray_failures.clone(), "steps": tray_steps.clone()}),
                );
                // B's compact strip Group actions: open, read, dismiss.
                let g5 = geometry(cx, OTHER)?;
                let s5 = state(cx)?;
                let b_pane = g5["panes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|pane| pane["active"] == PANE_B)
                    .and_then(|pane| pane["id"].as_str().map(str::to_string))
                    .unwrap_or_default();
                let strip_more = format!("strip:{b_pane}:more");
                let mut menu_failures = Vec::new();
                let mut menu_steps = vec![json!({"step": "before", "state": s5})];
                match center(&g5, &strip_more) {
                    None => menu_failures.push(format!(
                        "B's compact strip Group actions not painted at 800 wide: {}",
                        g5["probes"]
                    )),
                    Some((x, y)) => {
                        click_at(window, cx, x, y).await?;
                        let g6 = geometry(cx, OTHER)?;
                        let s6 = state(cx)?;
                        let _ = shot(
                            window,
                            cx,
                            self.output.join("10-accept-group-menu-open.png"),
                        );
                        let menu = &s6["menu"];
                        if menu["open"] != true
                            || menu["group"] != true
                            || menu["paneId"] != b_pane.as_str()
                        {
                            menu_failures
                                .push(format!("Group actions menu not open for B: {menu}"));
                        }
                        let rows: Vec<String> = g6["probes"]
                            .as_object()
                            .into_iter()
                            .flatten()
                            .filter(|(name, _)| name.starts_with("wb-menu:"))
                            .map(|(name, _)| name.trim_start_matches("wb-menu:").to_string())
                            .collect();
                        for expected in [
                            "wb-menu-new-child",
                            "wb-menu-new-split",
                            "wb-menu-close-group",
                            "wb-menu-close",
                            "wb-menu-split-right",
                        ] {
                            if !rows.iter().any(|row| row == expected) {
                                menu_failures
                                    .push(format!("Group actions lacks {expected}: {rows:?}"));
                            }
                        }
                        let window_w = g6["window"].as_f64().unwrap_or(f64::NAN);
                        for (name, b) in g6["probes"].as_object().into_iter().flatten() {
                            if name.starts_with("wb-menu:")
                                && (num(b, "x") < -0.5
                                    || num(b, "x") + num(b, "w") > window_w + 0.5)
                            {
                                menu_failures
                                    .push(format!("menu row {name} {b} outside the window"));
                            }
                        }
                        if g6["revision"] != g5["revision"]
                            || all_panes(&wb(window, cx)?).len() != 4
                        {
                            menu_failures.push("opening Group actions changed the layout".into());
                        }
                        unchanged(&s5, &s6, "menu open", &mut menu_failures);
                        menu_steps.push(json!({"step": "menuOpen", "state": s6, "rows": rows}));
                        escape(cx)?;
                        settle(cx, 400).await;
                        capture_probes(window, cx).await?;
                        let g7 = geometry(cx, OTHER)?;
                        let s7 = state(cx)?;
                        let _ = shot(
                            window,
                            cx,
                            self.output.join("10-accept-group-menu-dismissed.png"),
                        );
                        if s7["menu"]["open"] == true {
                            menu_failures.push("Escape left Group actions open".into());
                        }
                        if g7["probes"].as_object().is_some_and(|probes| {
                            probes.keys().any(|name| name.starts_with("wb-menu:"))
                        }) {
                            menu_failures.push("menu rows still painted after Escape".into());
                        }
                        if g7["layout"] != g5["layout"] || g7["revision"] != g5["revision"] {
                            menu_failures.push(format!(
                                "layout/revision changed: {} -> {}",
                                g5["revision"], g7["revision"]
                            ));
                        }
                        if g7["panes"].as_array().map(Vec::len)
                            != g5["panes"].as_array().map(Vec::len)
                        {
                            menu_failures.push("group count changed".into());
                        }
                        unchanged(&s5, &s7, "menu dismissed", &mut menu_failures);
                        menu_steps.push(json!({"step": "menuDismissed", "state": s7}));
                    }
                }
                checks.check(
                    "narrow acceptance: B's compact strip Group actions lists group and tab actions; Escape dismisses with no layout change",
                    menu_failures.is_empty(),
                    json!({"failures": menu_failures.clone(), "steps": menu_steps.clone()}),
                );
                resize.insert(
                    "acceptance".into(),
                    json!({
                        "size": [800.0, HEIGHT],
                        "tray": tray_steps,
                        "trayFailures": tray_failures,
                        "groupMenu": menu_steps,
                        "groupMenuFailures": menu_failures,
                    }),
                );
                resize_to(WIDTH, HEIGHT, cx)?;
                settle(cx, 600).await;
            }
        }
        // Back to the measured geometry and Main.
        resize_to(WIDTH, HEIGHT, cx)?;
        if frame(window, cx)?["sidebarCollapsed"] == true {
            window.update(cx, |shell, _, cx| shell.fixture_toggle_sidebar(false, cx))?;
        }
        settle(cx, 600).await;
        resize.insert("activation".into(), activation);
        phases.insert("resize".into(), Value::Object(resize));
        self.select(cx, MAIN);
        wait_wb(window, cx, "Main after the resize phase", |rb| {
            rb["loaded"] == true && rb["main"] == MAIN
        })
        .await?;
        mark(&self.output, "resize", "end");
        let failed = checks.failed();
        let results = json!({
            "fixture": "workbench-performance-fixture",
            "mode": if self.candidate { "candidate" } else { "baseline" },
            "phaseSelection": if resize_only { "resize" } else { "all" },
            // Default false: measured phases painted no layout probes
            // (`LUNA_PERF_MEASURE_LAYOUT_PROBES=1` is a diagnostic override).
            "layoutProbesWhileMeasuring": layout_probes_while_measuring,
            "entries": self.entries,
            "largeToolOutputs": 200,
            "fullOutputBytes": self.full_len,
            "window": {"width": WIDTH, "height": HEIGHT},
            "pid": std::process::id(),
            "executable": std::env::current_exe().ok(),
            "seedMs": self.seed_ms,
            "phases": phases,
            "checks": checks.list,
            "failedChecks": failed,
        });
        write_json(self.output.join("results.json"), &results);
        if self.candidate && !failed.is_empty() {
            anyhow::bail!("candidate checks failed: {failed:?}");
        }
        let _ = std::fs::write(
            self.output.join("workbench-performance.pass"),
            format!(
                "{} mode, {} entries, {} failed checks\n",
                if self.candidate {
                    "candidate"
                } else {
                    "baseline"
                },
                self.entries,
                failed.len()
            ),
        );
        Ok(())
    }
}
