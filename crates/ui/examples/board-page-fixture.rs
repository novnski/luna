//! Headless GPUI E2E for the Board page (project board slice A8).
//!
//! Drives the production `Shell` in a hidden window with real pointer input
//! (move/down/up and wheel through GPUI's hit testing, so drags are GPUI
//! drags) and real keystrokes, against a REAL in-process engine on a
//! temporary data directory with a scripted Mock harness. Tickets are created
//! through the Board RPC (`BoardCreate`, the method `BoardApi::create`
//! calls); every engine-side expectation is read back through `BoardList` /
//! `BoardRead`. A counting transport around the UI's engine connection
//! records every `Board*` call the UI makes, so "exactly one move" and "zero
//! execution calls" are counted, not inferred. No provider, network, GitHub
//! or credential is touched.
//!
//! Failure cases this run must catch (written before the implementation):
//! 1. Placement: Board is missing from the sidebar's navigation, or is not
//!    directly below Automations (New thread → Automations → Board), or
//!    activating it does not open `Page(Board)` beside the thread list.
//! 2. Scope: opening from a sidebar project filter does not prefilter to that
//!    project; All projects omits a project's tickets, shows a ticket without
//!    naming its project, or mixes another project's tickets into a single
//!    project; an explicit All projects choice is overridden by switching
//!    pages when the sidebar filter did not change.
//! 3. Columns: not exactly Backlog, Ready, Done left to right; a count that
//!    disagrees with the engine's list; the help ("Held or stopped", "Runs
//!    automatically") missing; the column order disagreeing with the engine's
//!    rank order.
//! 4. Drag Backlog → Ready issues zero or several `BoardMove` calls, any
//!    `BoardStart`/`BoardStartBatch`/`BoardControl`/`BoardRank`, asks for a
//!    confirmation, or the card does not stay in Ready with the engine's phase
//!    (still "Not started", or a badge that disagrees with `BoardRead`).
//! 5. Drag Ready → Backlog: not exactly one more `BoardMove`; the card claims
//!    "Stopped" (or "Not started") while the engine still reports Stop
//!    requested / Stopping / Unknown, or never shows the engine's final phase.
//! 6. Reorder within a column issues anything but exactly one `BoardRank` (a
//!    move, start or stop), the painted order differs from the engine's, or
//!    the engine records a launch or an intent for the reordered ticket.
//! 7. Dropping on Done issues any Board mutation, moves the card, or claims a
//!    review; the notice does not explain that Done is engine-owned.
//! 8. Keyboard: arrows do not move focus between cards; Alt+Right / Alt+Left
//!    do not issue exactly one `BoardMove` each; focus leaves the card after a
//!    move; Enter does not open that ticket's detail drawer in the overlay
//!    host and Escape does not return focus to it; Shift+F10 does not open a menu offering Move to
//!    Ready, Move to Backlog, Open chat and Archive; Archive/Unarchive from
//!    the menu and Cmd+Backspace do not issue exactly one `BoardArchive`.
//! 9. Accessibility: a card's label lacks its key, title, column or phase; a
//!    phase change is not announced, or announcing moves focus.
//! 10. Live update: a ticket created concurrently through RPC reorders the
//!     column and the focused card loses keyboard focus or selection.
//! 11. List: a narrow window does not switch to List automatically (or does
//!     so by overwriting the saved view), a ticket is missing from the list,
//!     a row's keyboard move differs from the card's, or widening does not
//!     bring the columns back.
//! 12. Empty state: a project without tickets does not offer Write manually,
//!     Discuss with AI, From GitHub issue and Add existing chat, or Write
//!     manually needs GitHub or opens anywhere but the page's one overlay
//!     host (A9's `BoardSurfaces`).
//! 13. Preferences: after re-creating the Shell, the scope, view, search and
//!     source filter, selected card or scroll offset are not restored, or
//!     `ui-settings.json` lacks them.
//! 14. Interface sizes 12, 16 and 20: columns stop sitting side by side at
//!     the default width, a card overflows its column, or toolbar controls
//!     overlap.
//! 15. Convergence: a second, narrow window on the same app state keeps
//!     redrawing (the automatic List decision flip-flopping, or two windows
//!     overwriting shared state each frame), or never settles on the List.
//! 16. Pagination: in a project with more tickets than one engine page
//!     (528 Backlog, then Ready and Done tickets that sort after all of
//!     them), a Ready or Done ticket is missing; a column count reports only
//!     the loaded page or disagrees with the engine; Backlog offers no Load
//!     more; Load more is not exactly one bounded `BoardList` (limit ≤ 500)
//!     for that column with the engine's cursor; the next page is missing,
//!     duplicated or out of the engine's order; or loading it clears or
//!     moves the selection or keyboard focus.
//! 17. Live update after more pages: a ticket created at the top through RPC
//!     (shifting every ticket across the page boundary) is missing, or the
//!     re-read loses, duplicates or reorders a ticket, drops the loaded pages
//!     back to the first, or the selected page-2 card loses selection or
//!     focus.
//! 18. Scope/filter change during pagination: a search typed while a Load
//!     more is in flight shows a stale ticket or keeps extra pages; clearing
//!     the search does not return to the first page; switching to All
//!     projects and back shows another scope's ticket, keeps the old pages,
//!     or disagrees with the engine.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example board-page-fixture
//! Run:   target/debug/examples/board-page-fixture [ABSOLUTE_EVIDENCE_DIR]
//!   (default: <repo>/target/qa/board/page)
//! Writes manifest.json (every step's expectation and observation, the
//! SHA-256 of each PNG and readback snapshot, the commit), the PNG captures
//! and board-page.pass; a failure writes failure.txt and failure.png, keeps
//! the partial manifest and exits nonzero.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    AppContext, AsyncApp, Bounds, Keystroke, MouseButton, PlatformInput, WindowBounds,
    WindowHandle, WindowOptions, point, px, size,
};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::mock::MockHarness;
use luna_proto::{
    AgentEvent, BoardColumn, BoardCreateRequest, BoardListRequest, BoardPlacement,
    BoardReadRequest, BoardScope, DoneStatus, HarnessId, WorkItemEventKind,
};
use luna_rpc::methods;
use luna_ui::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const P_LUNA: &str = "proj-luna";
const P_SITE: &str = "proj-site";
const P_NOTES: &str = "proj-notes";
const WIDTH: f32 = 1440.0;
const HEIGHT: f32 = 900.0;
/// A second window narrow enough for the automatic List view.
const NARROW_WINDOW: f32 = 760.0;
/// Paces the Mock run so Working and Stopping are observable.
const MOCK_DELAY_MS: &str = "700";

type Window = WindowHandle<shell::Shell>;
/// Every `Board*` call the UI made: (method, params).
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// The UI's engine transport, counting Board calls on the way through.
struct CountingRpc {
    inner: Arc<dyn luna_rpc::RpcService>,
    calls: Calls,
    hold: Arc<Hold>,
}

/// Item 18's gate on the UI transport: while closed, a `BoardList` with a
/// cursor (a Load more) waits before it reaches the engine, so a search
/// provably reaches the engine while that read is in flight. The fixture
/// opens it only after it has observed the search's result.
struct Hold {
    /// `true`: open (the default; every read passes).
    gate: tokio::sync::watch::Sender<bool>,
    /// Held reads not yet answered (or abandoned).
    held: AtomicU64,
    /// Held reads the engine answered (an abandoned one is dropped first).
    answered: AtomicU64,
    /// Search reads (`BoardList` with text) that arrived while a held read
    /// was unanswered.
    overlapped: AtomicU64,
}

impl Hold {
    fn new() -> Self {
        Self {
            gate: tokio::sync::watch::channel(true).0,
            held: AtomicU64::new(0),
            answered: AtomicU64::new(0),
            overlapped: AtomicU64::new(0),
        }
    }

    fn set_open(&self, open: bool) {
        self.gate.send_replace(open);
    }
}

/// Counts a held read until it is answered or dropped.
struct Held<'a>(&'a AtomicU64);

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, AtomicOrdering::SeqCst);
    }
}

#[async_trait::async_trait]
impl luna_rpc::RpcService for CountingRpc {
    async fn handle(
        &self,
        method: &str,
        params: Value,
    ) -> Result<luna_rpc::RpcReply, luna_rpc::RpcError> {
        if method.starts_with("Board") {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_owned(), params.clone()));
        }
        if method == methods::BOARD_LIST {
            if params["cursor"].is_string() && !*self.hold.gate.borrow() {
                self.hold.held.fetch_add(1, AtomicOrdering::SeqCst);
                let _held = Held(&self.hold.held);
                let mut gate = self.hold.gate.subscribe();
                let _ = gate.wait_for(|open| *open).await;
                let reply = self.inner.handle(method, params).await;
                self.hold.answered.fetch_add(1, AtomicOrdering::SeqCst);
                return reply;
            }
            if params["text"].is_string() && self.hold.held.load(AtomicOrdering::SeqCst) > 0 {
                self.hold.overlapped.fetch_add(1, AtomicOrdering::SeqCst);
            }
        }
        self.inner.handle(method, params).await
    }
}

/// The fixture's own (uncounted) view of the engine.
#[derive(Clone)]
struct Engine {
    rt: tokio::runtime::Handle,
    client: Arc<luna_rpc::RpcClient>,
}

impl Engine {
    async fn call(&self, method: &'static str, params: Value) -> anyhow::Result<Value> {
        let client = self.client.clone();
        self.rt
            .spawn(async move { client.call(method, params).await })
            .await?
            .map_err(|error| anyhow::anyhow!("{method}: {error}"))
    }

    async fn create(
        &self,
        project: &str,
        key: &str,
        title: &str,
        placement: BoardPlacement,
    ) -> anyhow::Result<String> {
        let request = BoardCreateRequest {
            request_id: format!("fixture-{key}"),
            project_id: Some(project.into()),
            title: title.into(),
            description: format!("Fixture brief: {title}."),
            implementation: None,
            review: None,
            completion: None,
            labels: Vec::new(),
            priority: None,
            context_refs: Vec::new(),
            origin: None,
            placement,
        };
        let receipt = self
            .call(methods::BOARD_CREATE, serde_json::to_value(request)?)
            .await?;
        receipt["workItemId"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("BoardCreate receipt without workItemId: {receipt}"))
    }

    async fn list(&self, scope: BoardScope) -> anyhow::Result<Value> {
        let request = BoardListRequest {
            scope,
            limit: Some(500),
            ..Default::default()
        };
        self.call(methods::BOARD_LIST, serde_json::to_value(request)?)
            .await
    }

    async fn read(&self, id: &str) -> anyhow::Result<Value> {
        let request = BoardReadRequest {
            work_item_id: id.into(),
            project_id: None,
            history_limit: Some(20),
        };
        self.call(methods::BOARD_READ, serde_json::to_value(request)?)
            .await
    }

    /// Engine column ids in rank order.
    async fn column_ids(&self, project: &str, column: &str) -> anyhow::Result<Vec<String>> {
        let page = self
            .list(BoardScope::Project {
                project_id: project.into(),
            })
            .await?;
        Ok(page["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|view| view["item"]["column"] == column)
            .filter_map(|view| view["item"]["id"].as_str().map(str::to_owned))
            .collect())
    }

    /// Every ticket id of one column in the engine's order, following the
    /// engine's cursor page by page (the ground truth for pagination).
    async fn all_ids(
        &self,
        scope: BoardScope,
        column: BoardColumn,
        text: Option<&str>,
    ) -> anyhow::Result<Vec<String>> {
        let mut ids = Vec::new();
        let mut cursor = None;
        for _ in 0..50 {
            let request = BoardListRequest {
                scope: scope.clone(),
                columns: vec![column],
                text: text.map(str::to_owned),
                cursor: cursor.take(),
                limit: Some(luna_proto::BOARD_LIST_MAX_LIMIT),
                ..Default::default()
            };
            let page = self
                .call(methods::BOARD_LIST, serde_json::to_value(request)?)
                .await?;
            ids.extend(
                page["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|view| view["item"]["id"].as_str().map(str::to_owned)),
            );
            match page["nextCursor"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => return Ok(ids),
            }
        }
        anyhow::bail!("the engine's {column:?} pages never ended")
    }

    async fn phase(&self, id: &str) -> anyhow::Result<String> {
        let detail = self.read(id).await?;
        Ok(detail["view"]["phase"].as_str().unwrap_or("?").to_owned())
    }
}

/// The repeatable artifact: steps with expectation/observation, hashes.
struct Manifest {
    out: PathBuf,
    steps: Vec<Value>,
    snapshots: Vec<Value>,
}

impl Manifest {
    fn step(&mut self, name: &str, expected: &str, observed: Value) {
        self.steps.push(json!({
            "step": name,
            "expected": expected,
            "observed": observed,
            "pass": true,
        }));
        let _ = self.write("running");
    }

    fn hash_file(&mut self, name: &str, path: &Path) -> anyhow::Result<String> {
        let bytes = std::fs::read(path)?;
        let sha = hex(&Sha256::digest(&bytes));
        self.snapshots.push(json!({
            "name": name,
            "file": path.file_name().map(|f| f.to_string_lossy().to_string()),
            "sha256": sha,
            "bytes": bytes.len(),
        }));
        Ok(sha)
    }

    fn json_snapshot(&mut self, name: &str, value: &Value) -> anyhow::Result<String> {
        let path = self.out.join(format!("{name}.json"));
        std::fs::write(&path, serde_json::to_vec_pretty(value)?)?;
        self.hash_file(name, &path)
    }

    fn write(&self, result: &str) -> anyhow::Result<()> {
        let commit = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned());
        let manifest = json!({
            "fixture": "board-page-fixture",
            "slice": "A8 Board page",
            "result": result,
            "commit": commit,
            "engine": "in-process EngineCore, temporary data dir, scripted Mock harness",
            "input": "real GPUI pointer (move/down/up/wheel) and keystrokes on the production Shell",
            "steps": self.steps,
            "snapshots": self.snapshots,
        });
        std::fs::write(
            self.out.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ---- window and input --------------------------------------------------------

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn draw(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
    })?;
    Ok(())
}

async fn frames(window: Window, cx: &mut AsyncApp, ms: u64) -> anyhow::Result<()> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        draw(window, cx)?;
        settle(cx, 16).await;
    }
    draw(window, cx)
}

fn capture(
    window: Window,
    cx: &mut AsyncApp,
    manifest: &mut Manifest,
    name: &str,
) -> anyhow::Result<String> {
    let path = manifest.out.join(format!("{name}.png"));
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(&path)?;
        Ok(())
    })??;
    manifest.hash_file(name, &path)
}

fn input(window: Window, cx: &mut AsyncApp, event: PlatformInput) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(event, cx);
    })?;
    Ok(())
}

fn hover(
    window: Window,
    cx: &mut AsyncApp,
    at: (f32, f32),
    pressed: Option<MouseButton>,
) -> anyhow::Result<()> {
    input(
        window,
        cx,
        PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position: point(px(at.0), px(at.1)),
            pressed_button: pressed,
            modifiers: Default::default(),
        }),
    )
}

fn button(
    window: Window,
    cx: &mut AsyncApp,
    at: (f32, f32),
    which: MouseButton,
    down: bool,
) -> anyhow::Result<()> {
    let position = point(px(at.0), px(at.1));
    input(
        window,
        cx,
        if down {
            PlatformInput::MouseDown(gpui::MouseDownEvent {
                button: which,
                position,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            })
        } else {
            PlatformInput::MouseUp(gpui::MouseUpEvent {
                button: which,
                position,
                modifiers: Default::default(),
                click_count: 1,
            })
        },
    )
}

async fn click(window: Window, cx: &mut AsyncApp, at: (f32, f32)) -> anyhow::Result<()> {
    hover(window, cx, at, None)?;
    frames(window, cx, 50).await?;
    button(window, cx, at, MouseButton::Left, true)?;
    button(window, cx, at, MouseButton::Left, false)?;
    frames(window, cx, 250).await?;
    park(window, cx)?;
    frames(window, cx, 120).await
}

/// A real GPUI drag: press, travel in steps (past the drag threshold), hold,
/// release over the target.
async fn drag(
    window: Window,
    cx: &mut AsyncApp,
    from: (f32, f32),
    to: (f32, f32),
) -> anyhow::Result<()> {
    hover(window, cx, from, None)?;
    draw(window, cx)?;
    button(window, cx, from, MouseButton::Left, true)?;
    for step in 1..=20 {
        let t = step as f32 / 20.0;
        hover(
            window,
            cx,
            (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t),
            Some(MouseButton::Left),
        )?;
        draw(window, cx)?;
        settle(cx, 8).await;
    }
    frames(window, cx, 120).await?;
    button(window, cx, to, MouseButton::Left, false)?;
    frames(window, cx, 300).await?;
    park(window, cx)?;
    frames(window, cx, 100).await
}

fn park(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    hover(window, cx, (WIDTH - 6.0, 6.0), None)
}

async fn wheel(window: Window, cx: &mut AsyncApp, at: (f32, f32), dy: f32) -> anyhow::Result<()> {
    hover(window, cx, at, None)?;
    for _ in 0..6 {
        input(
            window,
            cx,
            PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                position: point(px(at.0), px(at.1)),
                delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(dy / 6.0))),
                modifiers: Default::default(),
                touch_phase: gpui::TouchPhase::Moved,
            }),
        )?;
        draw(window, cx)?;
        settle(cx, 16).await;
    }
    frames(window, cx, 200).await
}

async fn press(window: Window, cx: &mut AsyncApp, combo: &str) -> anyhow::Result<()> {
    let keystroke = Keystroke::parse(combo)?;
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_keystroke(keystroke, cx);
    })?;
    frames(window, cx, 160).await
}

async fn type_text(window: Window, cx: &mut AsyncApp, text: &str) -> anyhow::Result<()> {
    for ch in text.chars() {
        let combo = if ch == ' ' {
            "space".to_owned()
        } else {
            ch.to_string()
        };
        press(window, cx, &combo).await?;
    }
    Ok(())
}

// ---- readbacks ----------------------------------------------------------------

fn board(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    draw(window, cx)?;
    let value = window.update(cx, |shell, window, cx| {
        shell.fixture_board_readback(window, cx)
    })?;
    anyhow::ensure!(!value.is_null(), "the Board page is not mounted");
    Ok(value)
}

fn sidebar(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    draw(window, cx)?;
    let value = window.update(cx, |shell, _, cx| {
        shell.fixture_sidebar_sections_readback(cx)
    })?;
    anyhow::ensure!(!value.is_null(), "the sidebar probe is not recording");
    Ok(value)
}

/// Poll the Board readback until `done` holds with no mutation in flight.
async fn wait(
    window: Window,
    cx: &mut AsyncApp,
    what: &str,
    done: impl Fn(&Value) -> bool,
) -> anyhow::Result<Value> {
    let mut last = Value::Null;
    for _ in 0..150 {
        if let Ok(rb) = board(window, cx) {
            if done(&rb) && rb["pending"] == 0 {
                return Ok(rb);
            }
            last = rb;
        }
        settle(cx, 100).await;
    }
    anyhow::bail!("timed out waiting for {what}; last readback: {last}")
}

fn num(value: &Value) -> f32 {
    value.as_f64().unwrap_or(f64::NAN) as f32
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn card<'a>(rb: &'a Value, id: &str) -> anyhow::Result<&'a Value> {
    rb["cards"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|card| card["id"] == id)
        .ok_or_else(|| anyhow::anyhow!("card {id} is not on the board"))
}

fn rect_of(value: &Value) -> anyhow::Result<(f32, f32, f32, f32)> {
    anyhow::ensure!(!value.is_null(), "no painted bounds");
    Ok((
        num(&value["x"]),
        num(&value["y"]),
        num(&value["w"]),
        num(&value["h"]),
    ))
}

fn center(rect: (f32, f32, f32, f32)) -> (f32, f32) {
    (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0)
}

fn card_rect(rb: &Value, id: &str) -> anyhow::Result<(f32, f32, f32, f32)> {
    rect_of(&card(rb, id)?["bounds"]).map_err(|e| anyhow::anyhow!("card {id}: {e}"))
}

fn column<'a>(rb: &'a Value, name: &str) -> anyhow::Result<&'a Value> {
    rb["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|column| column["column"] == name)
        .ok_or_else(|| anyhow::anyhow!("no {name} column"))
}

fn column_ids(rb: &Value, name: &str) -> Vec<String> {
    column(rb, name)
        .map(|column| strings(&column["ids"]))
        .unwrap_or_default()
}

fn control(rb: &Value, label: &str) -> anyhow::Result<(f32, f32, f32, f32)> {
    rect_of(&rb["controls"][label]).map_err(|e| anyhow::anyhow!("control {label}: {e}"))
}

fn sidebar_rect(rb: &Value, label: &str) -> anyhow::Result<(f32, f32, f32, f32)> {
    rect_of(&rb["bounds"][label]).map_err(|e| anyhow::anyhow!("sidebar {label}: {e}"))
}

/// The UI's Board calls since `mark`: (method, workItemId, params).
fn calls_since(calls: &Calls, mark: usize) -> Vec<(String, String, Value)> {
    calls.lock().unwrap()[mark..]
        .iter()
        .map(|(method, params)| {
            (
                method.clone(),
                params["workItemId"].as_str().unwrap_or_default().to_owned(),
                params.clone(),
            )
        })
        .collect()
}

fn count(calls: &[(String, String, Value)], method: &str) -> usize {
    calls.iter().filter(|(m, ..)| m == method).count()
}

/// No execution-affecting call besides what the step allows.
fn ensure_no_execution(calls: &[(String, String, Value)], context: &str) -> anyhow::Result<()> {
    for method in [
        methods::BOARD_START,
        methods::BOARD_START_BATCH,
        methods::BOARD_CONTROL,
        methods::BOARD_REVIEW_START,
    ] {
        anyhow::ensure!(
            count(calls, method) == 0,
            "{context}: the UI called {method}: {calls:?}"
        );
    }
    Ok(())
}

fn mark(calls: &Calls) -> usize {
    calls.lock().unwrap().len()
}

fn overlaps(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
    a.0 < b.0 + b.2 - 0.5 && b.0 < a.0 + a.2 - 0.5 && a.1 < b.1 + b.3 - 0.5 && b.1 < a.1 + a.3 - 0.5
}

// ---- passes -------------------------------------------------------------------

/// Item 1: Board sits directly below Automations and opens its page.
async fn navigation_pass(
    window: Window,
    cx: &mut AsyncApp,
    manifest: &mut Manifest,
) -> anyhow::Result<()> {
    let rb = sidebar(window, cx)?;
    let new_thread = sidebar_rect(&rb, "nav-new-thread")?;
    let automations = sidebar_rect(&rb, "nav-automations")?;
    let board_row = sidebar_rect(&rb, "nav-board")?;
    anyhow::ensure!(
        new_thread.1 < automations.1 && automations.1 < board_row.1,
        "item 1: navigation order must be New thread, Automations, Board: {new_thread:?} {automations:?} {board_row:?}"
    );
    let gap = board_row.1 - (automations.1 + automations.3);
    anyhow::ensure!(
        (-0.5..=6.0).contains(&gap) && (board_row.0 - automations.0).abs() < 0.5,
        "item 1: Board must sit directly below Automations (gap {gap}, x {} vs {})",
        board_row.0,
        automations.0
    );
    click(window, cx, center(board_row)).await?;
    let rb = sidebar(window, cx)?;
    anyhow::ensure!(
        rb["route"] == "Page(Board)",
        "item 1: activating Board must open its page: {}",
        rb["route"]
    );
    manifest.step(
        "navigation",
        "Board directly below Automations; activating it opens Page(Board)",
        json!({"automations": automations, "board": board_row, "gap": gap, "route": rb["route"]}),
    );
    Ok(())
}

/// Item 14 at one Interface size, plus the columns of item 3.
async fn scale_pass(
    window: Window,
    cx: &mut AsyncApp,
    engine: &Engine,
    manifest: &mut Manifest,
    font: typography::UiFontSize,
) -> anyhow::Result<()> {
    let size_px = font.pixels() as u32;
    window.update(cx, |_, window, cx| {
        typography::set_font_size(font, window, cx);
    })?;
    frames(window, cx, 500).await?;
    let rb = wait(window, cx, "the Luna board", |rb| {
        rb["status"] == "Live" && rb["effectiveView"] == "Board"
    })
    .await?;
    let names: Vec<String> = rb["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["label"].as_str().map(str::to_owned))
        .collect();
    anyhow::ensure!(
        names == ["Backlog", "Ready", "Done"],
        "item 3 ({size_px}px): columns {names:?}"
    );
    let rects: Vec<(f32, f32, f32, f32)> = ["backlog", "ready", "done"]
        .into_iter()
        .map(|name| rect_of(&column(&rb, name)?["bounds"]))
        .collect::<anyhow::Result<_>>()?;
    anyhow::ensure!(
        rects[0].0 < rects[1].0
            && rects[1].0 < rects[2].0
            && (rects[0].1 - rects[1].1).abs() < 1.0
            && (rects[1].1 - rects[2].1).abs() < 1.0,
        "item 14 ({size_px}px): columns must sit side by side: {rects:?}"
    );
    let help: Vec<String> = ["backlog", "ready"]
        .into_iter()
        .map(|name| column(&rb, name).map(|c| c["help"].as_str().unwrap_or_default().to_owned()))
        .collect::<anyhow::Result<_>>()?;
    anyhow::ensure!(
        help == ["Held or stopped", "Runs automatically"],
        "item 3: column help {help:?}"
    );
    for name in ["backlog", "ready", "done"] {
        let ui = column_ids(&rb, name);
        let engine_ids = engine.column_ids(P_LUNA, name).await?;
        anyhow::ensure!(
            ui == engine_ids && column(&rb, name)?["count"] == ui.len(),
            "item 3 ({size_px}px): {name} shows {ui:?}, the engine has {engine_ids:?}"
        );
        let column_rect = rect_of(&column(&rb, name)?["bounds"])?;
        for id in &ui {
            let card = card_rect(&rb, id)?;
            anyhow::ensure!(
                card.0 >= column_rect.0 - 0.5
                    && card.0 + card.2 <= column_rect.0 + column_rect.2 + 0.5,
                "item 14 ({size_px}px): card {id} {card:?} overflows its column {column_rect:?}"
            );
        }
    }
    let labels = [
        "board-project",
        "board-view",
        "board-search",
        "board-attention",
        "board-filters",
        "board-github",
        "board-new-issue",
    ];
    let controls: Vec<(f32, f32, f32, f32)> = labels
        .iter()
        .map(|label| control(&rb, label))
        .collect::<anyhow::Result<_>>()?;
    for (i, a) in controls.iter().enumerate() {
        for (j, b) in controls.iter().enumerate().skip(i + 1) {
            anyhow::ensure!(
                !overlaps(*a, *b),
                "item 14 ({size_px}px): {} {a:?} overlaps {} {b:?}",
                labels[i],
                labels[j]
            );
        }
    }
    let png = capture(window, cx, manifest, &format!("s{size_px}-board"))?;
    let snapshot = manifest.json_snapshot(&format!("s{size_px}-readback"), &rb)?;
    manifest.step(
        &format!("interface-{size_px}"),
        "three side-by-side columns with help and engine counts; cards inside columns; toolbar controls never overlap",
        json!({"columns": rects, "png": png, "readback": snapshot}),
    );
    Ok(())
}

/// Items 2–12 at the default Interface size.
async fn behaviour_pass(
    window: Window,
    cx: &mut AsyncApp,
    engine: &Engine,
    calls: &Calls,
    manifest: &mut Manifest,
    seeds: &[(String, &'static str)],
) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| {
        typography::set_font_size(typography::UiFontSize::ALL[4], window, cx);
    })?;
    frames(window, cx, 400).await?;
    let id = |key: &str| {
        seeds
            .iter()
            .find(|(_, k)| *k == key)
            .map(|(id, _)| id.clone())
            .expect("seeded")
    };

    // Item 2: the sidebar filter prefiltered the Board to Luna.
    let rb = wait(window, cx, "the Luna scope", |rb| rb["status"] == "Live").await?;
    anyhow::ensure!(
        rb["scope"] == json!({"kind": "project", "projectId": P_LUNA}),
        "item 2: opening from the Luna filter must prefilter: {}",
        rb["scope"]
    );
    let luna_cards = rb["cards"].as_array().map(Vec::len).unwrap_or(0);
    anyhow::ensure!(
        rb["cards"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|card| card["project"] == P_LUNA),
        "item 2: another project's ticket in the Luna scope"
    );

    // Item 2: All projects through the selector, by keyboard in its menu.
    let project = control(&rb, "board-project")?;
    click(window, cx, center(project)).await?;
    press(window, cx, "down").await?;
    press(window, cx, "enter").await?;
    let all = wait(window, cx, "All projects", |rb| {
        rb["scope"]["kind"] == "allProjects"
    })
    .await?;
    let engine_all = engine.list(BoardScope::AllProjects).await?;
    let mut engine_ids: Vec<String> = engine_all["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|view| view["item"]["id"].as_str().map(str::to_owned))
        .collect();
    let mut ui_ids: Vec<String> = all["cards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|card| card["id"].as_str().map(str::to_owned))
        .collect();
    engine_ids.sort();
    ui_ids.sort();
    anyhow::ensure!(
        ui_ids == engine_ids && ui_ids.len() > luna_cards,
        "item 2: All projects must aggregate every project's tickets: {ui_ids:?} vs {engine_ids:?}"
    );
    for card in all["cards"].as_array().into_iter().flatten() {
        let name = if card["project"] == P_SITE {
            "Website"
        } else {
            "Luna"
        };
        anyhow::ensure!(
            card["label"]
                .as_str()
                .is_some_and(|label| label.contains(&format!("project {name}"))),
            "item 2: an All projects card must name its project: {card}"
        );
    }
    capture(window, cx, manifest, "b-all-projects")?;
    // Switching pages without a sidebar change keeps the explicit choice.
    let rb = sidebar(window, cx)?;
    click(window, cx, center(sidebar_rect(&rb, "nav-automations")?)).await?;
    let rb = sidebar(window, cx)?;
    click(window, cx, center(sidebar_rect(&rb, "nav-board")?)).await?;
    let kept = wait(window, cx, "the kept scope", |rb| rb["status"] == "Live").await?;
    anyhow::ensure!(
        kept["scope"]["kind"] == "allProjects",
        "item 2: a page switch must not change the Board's explicit scope: {}",
        kept["scope"]
    );
    // Back to Luna (menu rows: All projects, Luna, Notes, Website).
    click(window, cx, center(control(&kept, "board-project")?)).await?;
    press(window, cx, "down").await?;
    press(window, cx, "down").await?;
    press(window, cx, "enter").await?;
    let rb = wait(window, cx, "the Luna scope again", |rb| {
        rb["scope"]["projectId"] == P_LUNA
    })
    .await?;
    manifest.step(
        "scope",
        "sidebar filter prefilters; All projects aggregates and names each project; explicit scope survives a page switch",
        json!({"luna": luna_cards, "all": ui_ids.len(), "kept": kept["scope"]}),
    );

    // Item 4: drag Backlog → Ready.
    let l1 = id("L1");
    let before = mark(calls);
    let ready = rect_of(&column(&rb, "ready")?["bounds"])?;
    drag(
        window,
        cx,
        center(card_rect(&rb, &l1)?),
        (ready.0 + ready.2 / 2.0, ready.1 + 70.0),
    )
    .await?;
    wait(window, cx, "L1 in Ready", |rb| {
        card(rb, &l1).is_ok_and(|card| card["column"] == "ready")
    })
    .await?;
    let made = calls_since(calls, before);
    let moves: Vec<&(String, String, Value)> = made
        .iter()
        .filter(|(m, ..)| m == methods::BOARD_MOVE)
        .collect();
    anyhow::ensure!(
        moves.len() == 1 && moves[0].1 == l1 && moves[0].2["to"] == "ready",
        "item 4: one BoardMove to Ready expected: {made:?}"
    );
    anyhow::ensure!(
        count(&made, methods::BOARD_RANK) == 0,
        "item 4: a column move must not rank separately: {made:?}"
    );
    ensure_no_execution(&made, "item 4")?;
    // The badge follows the engine's phase, which must leave Not started.
    let mut agreed = None;
    for _ in 0..120 {
        let phase = engine.phase(&l1).await?;
        let rb = board(window, cx)?;
        let ui = card(&rb, &l1)?.clone();
        if phase != "notStarted" && ui["phase"].as_str().map(lower_first) == Some(phase.clone()) {
            agreed = Some((phase, ui));
            break;
        }
        settle(cx, 100).await;
    }
    let (phase, ui) = agreed.ok_or_else(|| {
        anyhow::anyhow!("item 4: the Ready card never showed the engine's started phase")
    })?;
    anyhow::ensure!(
        ui["column"] == "ready" && ui["badge"] != "Not started",
        "item 4: the card must stay in Ready with a phase badge: {ui}"
    );
    let png = capture(window, cx, manifest, "b-drag-to-ready")?;
    manifest.step(
        "drag-backlog-to-ready",
        "exactly one BoardMove(to ready); no start/control/rank; card stays in Ready with the engine's phase",
        json!({"calls": made.iter().map(|(m, id, _)| json!([m, id])).collect::<Vec<_>>(), "enginePhase": phase, "badge": ui["badge"], "png": png}),
    );

    // Item 5: drag back Ready → Backlog; the badge never claims stopped early.
    let before = mark(calls);
    let rb = board(window, cx)?;
    let backlog = rect_of(&column(&rb, "backlog")?["bounds"])?;
    drag(
        window,
        cx,
        center(card_rect(&rb, &l1)?),
        (backlog.0 + backlog.2 / 2.0, backlog.1 + 60.0),
    )
    .await?;
    wait(window, cx, "L1 back in Backlog", |rb| {
        card(rb, &l1).is_ok_and(|card| card["column"] == "backlog")
    })
    .await?;
    let made = calls_since(calls, before);
    anyhow::ensure!(
        count(&made, methods::BOARD_MOVE) == 1
            && made
                .iter()
                .any(|(m, id, p)| m == methods::BOARD_MOVE && *id == l1 && p["to"] == "backlog"),
        "item 5: one BoardMove to Backlog expected: {made:?}"
    );
    ensure_no_execution(&made, "item 5")?;
    let mut seen = Vec::new();
    let mut settled = None;
    for _ in 0..200 {
        let rb = board(window, cx)?;
        let ui = card(&rb, &l1)?.clone();
        let phase = engine.phase(&l1).await?;
        let badge = ui["badge"].as_str().unwrap_or_default().to_owned();
        if seen.last() != Some(&(badge.clone(), phase.clone())) {
            seen.push((badge.clone(), phase.clone()));
        }
        // Stopped on the card only once the engine says so (it may lag the
        // engine, never lead it).
        if badge == "Stopped" {
            anyhow::ensure!(
                phase == "stopped",
                "item 5: the card says Stopped while the engine says {phase}"
            );
        }
        if matches!(phase.as_str(), "stopped" | "notStarted")
            && ui["phase"].as_str().map(lower_first) == Some(phase.clone())
        {
            settled = Some(ui);
            break;
        }
        settle(cx, 100).await;
    }
    let settled = settled.ok_or_else(|| {
        anyhow::anyhow!(
            "item 5: the Backlog card never reached the engine's stopped state: {seen:?}"
        )
    })?;
    let png = capture(window, cx, manifest, "b-drag-to-backlog")?;
    manifest.step(
        "drag-ready-to-backlog",
        "exactly one BoardMove(to backlog); the badge tracks Stop requested/Stopping/Stopped from engine state and never leads it",
        json!({"sequence": seen, "final": settled["badge"], "png": png}),
    );

    // Item 6: reorder within Backlog → one BoardRank, nothing else.
    let rb = board(window, cx)?;
    let order = column_ids(&rb, "backlog");
    anyhow::ensure!(
        order.len() >= 3,
        "item 6: Backlog needs three tickets: {order:?}"
    );
    let moved = order.last().unwrap().clone();
    let top = card_rect(&rb, &order[0])?;
    let before = mark(calls);
    drag(
        window,
        cx,
        center(card_rect(&rb, &moved)?),
        (top.0 + top.2 / 2.0, top.1 + 6.0),
    )
    .await?;
    let rb = wait(window, cx, "the reordered Backlog", |rb| {
        column_ids(rb, "backlog").first() == Some(&moved)
    })
    .await?;
    let made = calls_since(calls, before);
    anyhow::ensure!(
        count(&made, methods::BOARD_RANK) == 1
            && count(&made, methods::BOARD_MOVE) == 0
            && made
                .iter()
                .any(|(m, id, _)| m == methods::BOARD_RANK && *id == moved),
        "item 6: a reorder must be exactly one BoardRank: {made:?}"
    );
    ensure_no_execution(&made, "item 6")?;
    let engine_order = engine.column_ids(P_LUNA, "backlog").await?;
    anyhow::ensure!(
        column_ids(&rb, "backlog") == engine_order,
        "item 6: painted order {:?} differs from the engine's {engine_order:?}",
        column_ids(&rb, "backlog")
    );
    let detail = engine.read(&moved).await?;
    anyhow::ensure!(
        detail["launches"].as_array().is_none_or(Vec::is_empty) && detail["intent"].is_null(),
        "item 6: a reorder must leave no launch or intent: {}",
        detail
    );
    manifest.step(
        "reorder-within-column",
        "exactly one BoardRank; zero move/start/control; engine order equals painted order; no launch or intent",
        json!({"moved": moved, "order": engine_order}),
    );

    // Item 7: dropping on Done calls nothing and moves nothing.
    let victim = column_ids(&rb, "backlog")[1].clone();
    let done = rect_of(&column(&rb, "done")?["bounds"])?;
    let before = mark(calls);
    drag(
        window,
        cx,
        center(card_rect(&rb, &victim)?),
        (done.0 + done.2 / 2.0, done.1 + 60.0),
    )
    .await?;
    let rb = wait(window, cx, "the refused Done drop", |rb| {
        !rb["notice"].is_null()
    })
    .await?;
    let made = calls_since(calls, before);
    let mutations: Vec<&(String, String, Value)> = made
        .iter()
        .filter(|(m, ..)| {
            !matches!(
                m.as_str(),
                methods::BOARD_LIST
                    | methods::BOARD_READ
                    | methods::BOARD_OUTCOMES
                    | methods::BOARD_WATCH
                    | methods::BOARD_AUTHORING_LIST
            )
        })
        .collect();
    anyhow::ensure!(
        mutations.is_empty(),
        "item 7: dropping on Done must call nothing: {mutations:?}"
    );
    let notice = rb["notice"].as_str().unwrap_or_default();
    anyhow::ensure!(
        card(&rb, &victim)?["column"] == "backlog"
            && notice.contains("Done")
            && notice.contains("review"),
        "item 7: the card must stay put with an explanation: {notice}"
    );
    anyhow::ensure!(
        engine.read(&victim).await?["view"]["item"]["column"] == "backlog",
        "item 7: the engine must still hold the ticket in Backlog"
    );
    manifest.step(
        "drop-on-done",
        "no Board mutation; the card stays in Backlog; the notice says Done is engine-owned",
        json!({"notice": notice}),
    );

    // Items 8 and 9: keyboard only from a focused card.
    let target = column_ids(&rb, "backlog")[0].clone();
    let next = column_ids(&rb, "backlog")[1].clone();
    // One pointer click to put focus on the board; everything after is keys.
    click(window, cx, center(card_rect(&rb, &target)?)).await?;
    let rb = wait(window, cx, "the detail drawer", |rb| {
        rb["overlay"] == "detail"
    })
    .await?;
    anyhow::ensure!(
        rb["overlayItem"] == target.as_str(),
        "item 8: clicking a card opens its detail: {}",
        rb["overlayItem"]
    );
    press(window, cx, "escape").await?;
    wait(window, cx, "focus back on the card", |rb| {
        rb["overlay"].is_null() && rb["focused"] == target.as_str()
    })
    .await?;
    press(window, cx, "down").await?;
    let rb = wait(window, cx, "Down to the next card", |rb| {
        rb["focused"] == next.as_str()
    })
    .await?;
    press(window, cx, "up").await?;
    wait(window, cx, "Up back", |rb| rb["focused"] == target.as_str()).await?;
    let label = card(&rb, &target)?["label"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let key = card(&rb, &target)?["key"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    anyhow::ensure!(
        label.contains(&key)
            && label.contains("Backlog")
            && label.contains(card(&rb, &target)?["badge"].as_str().unwrap_or("?")),
        "item 9: the card label must name key, title, column and phase: {label}"
    );
    let before = mark(calls);
    press(window, cx, "alt-right").await?;
    let rb = wait(window, cx, "Alt+Right moved to Ready", |rb| {
        card(rb, &target).is_ok_and(|card| card["column"] == "ready")
    })
    .await?;
    let made = calls_since(calls, before);
    anyhow::ensure!(
        count(&made, methods::BOARD_MOVE) == 1,
        "item 8: Alt+Right must be one BoardMove: {made:?}"
    );
    ensure_no_execution(&made, "item 8")?;
    anyhow::ensure!(
        rb["focused"] == target.as_str(),
        "item 8: focus must stay on the moved card: {}",
        rb["focused"]
    );
    // Item 9: the change is announced without moving focus.
    let announced = wait(window, cx, "the phase announcement", |rb| {
        rb["announcement"]
            .as_str()
            .is_some_and(|text| text.contains(&key))
    })
    .await?;
    anyhow::ensure!(
        announced["focused"] == target.as_str(),
        "item 9: announcing must not move focus: {}",
        announced["focused"]
    );
    let before = mark(calls);
    press(window, cx, "alt-left").await?;
    wait(window, cx, "Alt+Left moved to Backlog", |rb| {
        card(rb, &target).is_ok_and(|card| card["column"] == "backlog")
    })
    .await?;
    let made = calls_since(calls, before);
    anyhow::ensure!(
        count(&made, methods::BOARD_MOVE) == 1,
        "item 8: Alt+Left must be one BoardMove: {made:?}"
    );
    press(window, cx, "enter").await?;
    let rb = wait(window, cx, "Enter opens the drawer", |rb| {
        rb["overlay"] == "detail"
    })
    .await?;
    anyhow::ensure!(
        rb["overlayItem"] == target.as_str(),
        "item 8: Enter opens the focused ticket"
    );
    press(window, cx, "escape").await?;
    wait(window, cx, "Escape returns focus", |rb| {
        rb["overlay"].is_null() && rb["focused"] == target.as_str()
    })
    .await?;
    press(window, cx, "shift-f10").await?;
    let rb = wait(window, cx, "the card menu", |rb| !rb["menu"].is_null()).await?;
    let keys: Vec<String> = rb["menuEntries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["key"].as_str().map(str::to_owned))
        .collect();
    for wanted in ["card-ready", "card-backlog", "card-chat", "card-archive"] {
        anyhow::ensure!(
            keys.iter().any(|key| key == wanted),
            "item 8: the card menu lacks {wanted}: {keys:?}"
        );
    }
    capture(window, cx, manifest, "b-card-menu")?;
    // Archive from the menu: Up wraps to the last row (Archive).
    let before = mark(calls);
    press(window, cx, "up").await?;
    press(window, cx, "enter").await?;
    let rb = wait(window, cx, "the archived card hidden", |rb| {
        card(rb, &target).is_err()
    })
    .await?;
    let made = calls_since(calls, before);
    anyhow::ensure!(
        count(&made, methods::BOARD_ARCHIVE) == 1
            && made.iter().any(|(m, id, p)| m == methods::BOARD_ARCHIVE
                && *id == target
                && p["archived"] == true),
        "item 8: Archive must be one BoardArchive: {made:?}"
    );
    ensure_no_execution(&made, "item 8 archive")?;
    // Show archived (Filters menu, Up from the bottom skips a disabled
    // Clear), then Cmd+Backspace on the focused card unarchives it.
    click(window, cx, center(control(&rb, "board-filters")?)).await?;
    press(window, cx, "up").await?;
    press(window, cx, "enter").await?;
    let rb = wait(window, cx, "the archived card shown", |rb| {
        card(rb, &target).is_ok_and(|card| card["archived"] == true)
            && rb["focused"] == target.as_str()
    })
    .await?;
    let before = mark(calls);
    press(window, cx, "cmd-backspace").await?;
    wait(window, cx, "the card restored", |rb| {
        card(rb, &target).is_ok_and(|card| card["archived"] == false)
    })
    .await?;
    let made = calls_since(calls, before);
    anyhow::ensure!(
        count(&made, methods::BOARD_ARCHIVE) == 1
            && made
                .iter()
                .any(|(m, _, p)| m == methods::BOARD_ARCHIVE && p["archived"] == false),
        "item 8: Cmd+Backspace must unarchive once: {made:?}"
    );
    click(window, cx, center(control(&rb, "board-filters")?)).await?;
    press(window, cx, "up").await?;
    press(window, cx, "up").await?;
    press(window, cx, "enter").await?;
    wait(window, cx, "archived hidden again", |rb| {
        rb["filters"]["archived"] == "exclude"
    })
    .await?;
    manifest.step(
        "keyboard",
        "arrows move focus; Alt+Right/Left are one BoardMove each with focus kept; Enter/Escape open and leave the drawer; Shift+F10 menu offers Ready/Backlog/Open chat/Archive; archive and unarchive are one BoardArchive each",
        json!({"target": target, "menu": keys, "announcement": announced["announcement"], "label": label}),
    );

    // Item 10: a live insert above the focused card keeps its focus.
    press(window, cx, "home").await?;
    let rb = board(window, cx)?;
    let focused = rb["focused"].as_str().unwrap_or_default().to_owned();
    anyhow::ensure!(
        !focused.is_empty(),
        "item 10: a card must hold focus: {}",
        rb["focused"]
    );
    let index_before = column_ids(&rb, "backlog")
        .iter()
        .position(|id| *id == focused);
    let inserted = engine
        .create(
            P_LUNA,
            "L-live",
            "Triage crash reports",
            BoardPlacement::Top,
        )
        .await?;
    let rb = wait(window, cx, "the live insert", |rb| {
        column_ids(rb, "backlog").first() == Some(&inserted)
    })
    .await?;
    let index_after = column_ids(&rb, "backlog")
        .iter()
        .position(|id| *id == focused);
    anyhow::ensure!(
        rb["focused"] == focused.as_str() && rb["selected"] == focused.as_str(),
        "item 10: focus/selection must survive the live reorder: {} / {}",
        rb["focused"],
        rb["selected"]
    );
    anyhow::ensure!(
        index_before
            .zip(index_after)
            .is_some_and(|(b, a)| a == b + 1),
        "item 10: the focused card must have moved down one place: {index_before:?} → {index_after:?}"
    );
    manifest.step(
        "live-update-focus",
        "a concurrent RPC insert reorders the column; the focused card keeps focus and selection",
        json!({"focused": focused, "before": index_before, "after": index_after}),
    );

    // Item 11: List at a narrow width, same actions; widening restores.
    window.update(cx, |_, window, _| {
        window.resize(size(px(760.0), px(HEIGHT)))
    })?;
    frames(window, cx, 500).await?;
    let rb = wait(window, cx, "the automatic List", |rb| {
        rb["effectiveView"] == "List" && rb["narrow"] == true
    })
    .await?;
    anyhow::ensure!(
        rb["view"] == "Board",
        "item 11: the narrow List must not overwrite the saved view: {}",
        rb["view"]
    );
    for card in rb["cards"].as_array().into_iter().flatten() {
        anyhow::ensure!(
            !card["bounds"].is_null(),
            "item 11: {} is missing from the List",
            card["key"]
        );
    }
    let png = capture(window, cx, manifest, "b-list-narrow")?;
    let before = mark(calls);
    press(window, cx, "alt-right").await?;
    wait(window, cx, "the List row moved to Ready", |rb| {
        card(rb, &focused).is_ok_and(|card| card["column"] == "ready")
    })
    .await?;
    press(window, cx, "alt-left").await?;
    wait(window, cx, "the List row back in Backlog", |rb| {
        card(rb, &focused).is_ok_and(|card| card["column"] == "backlog")
    })
    .await?;
    let made = calls_since(calls, before);
    anyhow::ensure!(
        count(&made, methods::BOARD_MOVE) == 2,
        "item 11: List keyboard moves must be the same BoardMove calls: {made:?}"
    );
    ensure_no_execution(&made, "item 11")?;
    window.update(cx, |_, window, _| {
        window.resize(size(px(WIDTH), px(HEIGHT)))
    })?;
    frames(window, cx, 500).await?;
    wait(window, cx, "the columns again", |rb| {
        rb["effectiveView"] == "Board"
    })
    .await?;
    manifest.step(
        "list-narrow",
        "List chosen automatically below the narrow width without changing the saved view; rows keep the keyboard moves; widening restores the columns",
        json!({"png": png}),
    );

    // Item 12: the empty state (Notes has no tickets).
    let rb = board(window, cx)?;
    click(window, cx, center(control(&rb, "board-project")?)).await?;
    for _ in 0..3 {
        press(window, cx, "down").await?;
    }
    press(window, cx, "enter").await?;
    let rb = wait(window, cx, "the empty Notes board", |rb| {
        rb["emptyState"] == true
    })
    .await?;
    for label in [
        "board-empty-manual",
        "board-empty-discuss",
        "board-empty-github",
        "board-empty-chat",
    ] {
        control(&rb, label)?;
    }
    let png = capture(window, cx, manifest, "b-empty")?;
    // Write manually opens A9's form in the page's one overlay host (the
    // Board shows one project, so no chooser step) and needs no GitHub.
    click(window, cx, center(control(&rb, "board-empty-manual")?)).await?;
    let manual = wait(window, cx, "Write manually", |rb| rb["overlay"] == "manual").await?;
    let form_png = capture(window, cx, manifest, "b-empty-write-manually")?;
    press(window, cx, "escape").await?;
    let rb = wait(window, cx, "the drawer closed", |rb| {
        rb["overlay"].is_null()
    })
    .await?;
    manifest.step(
        "empty-state",
        "Write manually, Discuss with AI, From GitHub issue and Add existing chat offered; Write manually opens A9's form in the overlay host without GitHub; Escape closes it",
        json!({"png": png, "form": form_png, "surface": manual["overlay"]}),
    );
    click(window, cx, center(control(&rb, "board-project")?)).await?;
    press(window, cx, "down").await?;
    press(window, cx, "down").await?;
    press(window, cx, "enter").await?;
    wait(window, cx, "Luna again", |rb| {
        rb["scope"]["projectId"] == P_LUNA
    })
    .await?;
    Ok(())
}

/// Item 13: presentation preferences survive re-creating the Shell.
async fn prefs_pass(
    window: Window,
    cx: &mut AsyncApp,
    manifest: &mut Manifest,
    data: &Path,
    reopen: impl FnOnce(&mut AsyncApp, f32) -> anyhow::Result<Window>,
) -> anyhow::Result<Window> {
    // A short window so the List scrolls.
    window.update(cx, |_, window, _| window.resize(size(px(WIDTH), px(470.0))))?;
    frames(window, cx, 400).await?;
    let rb = board(window, cx)?;
    let switch = control(&rb, "board-view")?;
    click(
        window,
        cx,
        (switch.0 + switch.2 * 0.75, switch.1 + switch.3 / 2.0),
    )
    .await?;
    wait(window, cx, "List chosen", |rb| rb["view"] == "List").await?;
    click(window, cx, center(control(&rb, "board-search")?)).await?;
    type_text(window, cx, "o").await?;
    let rb = wait(window, cx, "the search applied", |rb| {
        rb["filters"]["text"] == "o"
    })
    .await?;
    // Source: Written manually (Filters rows: attention, any provider, three
    // providers, any source, then Written manually).
    click(window, cx, center(control(&rb, "board-filters")?)).await?;
    for _ in 0..7 {
        press(window, cx, "down").await?;
    }
    press(window, cx, "enter").await?;
    let rb = wait(window, cx, "the source filter", |rb| {
        rb["filters"]["source"] == "local"
    })
    .await?;
    // Select the last row by keyboard, scroll the page with the wheel.
    let ids: Vec<String> = ["backlog", "ready", "done"]
        .into_iter()
        .flat_map(|name| column_ids(&rb, name))
        .collect();
    anyhow::ensure!(
        ids.len() >= 3,
        "item 13: the filtered List is too short: {ids:?}"
    );
    click(window, cx, center(card_rect(&rb, &ids[0])?)).await?;
    press(window, cx, "escape").await?;
    press(window, cx, "end").await?;
    let selected = ids.last().unwrap().clone();
    let rb = wait(window, cx, "the last row selected", |rb| {
        rb["selected"] == selected.as_str()
    })
    .await?;
    let viewport = rect_of(&rb["scroll"]["viewport"])?;
    if num(&rb["scroll"]["offset"]) < 20.0 {
        wheel(window, cx, center(viewport), -160.0).await?;
    }
    let rb = wait(window, cx, "the scroll persisted", |rb| {
        num(&rb["prefs"]["scrollY"]) > 20.0
            && (num(&rb["prefs"]["scrollY"]) - num(&rb["scroll"]["offset"])).abs() < 2.0
    })
    .await?;
    let saved = rb["prefs"].clone();
    // The settings file catches up after its debounce.
    let mut on_disk = Value::Null;
    for _ in 0..40 {
        let settings = settings::UiSettings::load(data);
        on_disk = serde_json::to_value(&settings.board)?;
        if on_disk == saved {
            break;
        }
        settle(cx, 100).await;
    }
    anyhow::ensure!(
        on_disk == saved,
        "item 13: ui-settings.json must hold the Board prefs: {on_disk} vs {saved}"
    );
    capture(window, cx, manifest, "p-before")?;

    // Re-create the Shell (same app state, same settings store).
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, _| window.remove_window())?;
    settle(cx, 200).await;
    // The same short window from the first frame, so the restored offset is
    // reachable.
    let window = reopen(cx, 470.0)?;
    frames(window, cx, 900).await?;
    let rb = wait(window, cx, "the restored Board", |rb| {
        rb["status"] == "Live" && rb["selected"] == selected.as_str()
    })
    .await?;
    let restored = rb["prefs"].clone();
    anyhow::ensure!(
        restored["projectId"] == saved["projectId"]
            && restored["view"] == saved["view"]
            && restored["search"] == saved["search"]
            && restored["source"] == saved["source"]
            && restored["selectedItem"] == saved["selectedItem"],
        "item 13: prefs not restored: {restored} vs {saved}"
    );
    anyhow::ensure!(
        rb["scope"]["projectId"] == P_LUNA
            && rb["effectiveView"] == "List"
            && rb["filters"]["text"] == "o"
            && rb["filters"]["source"] == "local",
        "item 13: the restored Board does not apply its prefs: {}",
        rb
    );
    let offset = num(&rb["scroll"]["offset"]);
    anyhow::ensure!(
        (offset - num(&saved["scrollY"])).abs() < 2.0,
        "item 13: scroll {offset} not restored to {}",
        saved["scrollY"]
    );
    anyhow::ensure!(
        rb["focused"] == selected.as_str(),
        "item 13: the restored selection must hold keyboard focus: {}",
        rb["focused"]
    );
    let png = capture(window, cx, manifest, "p-restored")?;
    let snapshot = manifest.json_snapshot("p-restored-readback", &rb)?;
    manifest.step(
        "prefs-restore",
        "scope, view, search, source filter, selected card and scroll restored after re-creating the Shell and saved in ui-settings.json",
        json!({"saved": saved, "restored": restored, "offset": offset, "png": png, "readback": snapshot}),
    );
    Ok(window)
}

/// The Board readback without drawing a frame (frame counting).
fn board_undrawn(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    window.update(cx, |shell, window, cx| {
        shell.fixture_board_readback(window, cx)
    })
}

/// Item 15: a second, narrow window on the same app state settles. Both
/// windows' Board pages stop rendering once nothing changes: no continuous
/// redraw from the narrow decision (or anything else) in either window.
async fn convergence_pass(
    window: Window,
    cx: &mut AsyncApp,
    manifest: &mut Manifest,
    open: impl FnOnce(&mut AsyncApp) -> anyhow::Result<Window>,
) -> anyhow::Result<()> {
    let narrow = open(cx)?;
    settle(cx, 600).await;
    // A few frames: the restored Board page mounts and decides List.
    frames(narrow, cx, 400).await?;
    let rb = wait(narrow, cx, "the narrow window's List", |rb| {
        rb["status"] == "Live" && rb["effectiveView"] == "List"
    })
    .await?;
    let png = capture(narrow, cx, manifest, "c-narrow-window")?;
    // Now only watch: no input, no forced draws.
    settle(cx, 300).await;
    let before = (
        num(&board_undrawn(narrow, cx)?["renders"]),
        num(&board_undrawn(window, cx)?["renders"]),
    );
    settle(cx, 1000).await;
    let after = (
        num(&board_undrawn(narrow, cx)?["renders"]),
        num(&board_undrawn(window, cx)?["renders"]),
    );
    anyhow::ensure!(
        // A settled page renders only for real changes (a late engine
        // observation, a timer); continuous redraw is dozens per second.
        after.0 - before.0 <= 5.0 && after.1 - before.1 <= 5.0,
        "item 15: the Board keeps redrawing with nothing changing: narrow {before:?} → {after:?} (renders)"
    );
    let still = board_undrawn(narrow, cx)?;
    anyhow::ensure!(
        still["effectiveView"] == "List" && still["view"] == rb["view"],
        "item 15: the narrow window's view changed by itself: {}",
        still["effectiveView"]
    );
    manifest.step(
        "narrow-second-window-converges",
        "a second narrow window on the same state shows the List and, like the first window, stops rendering within a second",
        json!({"renders": {"before": before, "after": after}, "png": png}),
    );
    gpui::AnyWindowHandle::from(narrow).update(cx, |_, window, _| window.remove_window())?;
    settle(cx, 200).await;
    Ok(())
}

/// One column's pagination readback: (loaded, total, hasMore, pages, count text).
fn paging(rb: &Value, name: &str) -> (usize, Value, Value, Value, Value) {
    let entry = column(rb, name).cloned().unwrap_or(Value::Null);
    (
        column_ids(rb, name).len(),
        entry["total"].clone(),
        entry["hasMore"].clone(),
        entry["pages"].clone(),
        entry["countText"].clone(),
    )
}

fn unique(ids: &[String]) -> bool {
    ids.iter().collect::<std::collections::HashSet<_>>().len() == ids.len()
}

/// The `BoardList` calls among `calls`.
fn list_calls(calls: &[(String, String, Value)]) -> Vec<Value> {
    calls
        .iter()
        .filter(|(m, ..)| m == methods::BOARD_LIST)
        .map(|(.., params)| params.clone())
        .collect()
}

/// Every UI read is bounded by the engine's page.
fn ensure_bounded(lists: &[Value], context: &str) -> anyhow::Result<()> {
    for params in lists {
        let limit = params["limit"].as_u64();
        anyhow::ensure!(
            limit.is_some_and(
                |limit| limit >= 1 && limit <= u64::from(luna_proto::BOARD_LIST_MAX_LIMIT)
            ),
            "{context}: an unbounded BoardList: {params}"
        );
    }
    Ok(())
}

/// Wheel the Board page back to its top: the header and toolbar scroll
/// with the page, so toolbar controls are only clickable there.
async fn scroll_top(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    let rb = board(window, cx)?;
    let offset = num(&rb["scroll"]["offset"]);
    if offset > 0.5 {
        let viewport = rect_of(&rb["scroll"]["viewport"])?;
        wheel(window, cx, center(viewport), offset + 400.0).await?;
    }
    wait(window, cx, "the page scrolled to its top", |rb| {
        num(&rb["scroll"]["offset"]) < 0.5
    })
    .await
}

/// Bring the Backlog column's Load more into view: select the column's
/// first visible card (closing the drawer the click opens), End to its last
/// loaded card (the keyboard reveal scrolls), then wheel until the control
/// sits inside the window.
async fn reveal_load_more(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    let rb = board(window, cx)?;
    let first = column_ids(&rb, "backlog")
        .into_iter()
        .find(|id| card_rect(&rb, id).is_ok_and(|rect| rect.1 > 0.0 && rect.1 + rect.3 < HEIGHT))
        .ok_or_else(|| anyhow::anyhow!("no visible Backlog card: {rb}"))?;
    // A click selects the card and opens its drawer; Escape closes the
    // drawer and returns focus to the card.
    click(window, cx, center(card_rect(&rb, &first)?)).await?;
    press(window, cx, "escape").await?;
    wait(window, cx, "the drawer closed on the clicked card", |rb| {
        rb["overlay"].is_null() && rb["focused"] == first.as_str()
    })
    .await?;
    press(window, cx, "end").await?;
    let last = column_ids(&rb, "backlog")
        .last()
        .cloned()
        .unwrap_or_default();
    let mut rb = wait(window, cx, "the last loaded Backlog card", |rb| {
        rb["selected"] == last.as_str() && rb["focused"] == last.as_str()
    })
    .await?;
    for _ in 0..12 {
        if let Ok(rect) = control(&rb, "board-load-more-backlog")
            && rect.1 > 0.0
            && rect.1 + rect.3 < HEIGHT - 4.0
        {
            return Ok(rb);
        }
        let lane = rect_of(&column(&rb, "backlog")?["bounds"])?;
        wheel(window, cx, (lane.0 + lane.2 / 2.0, HEIGHT / 2.0), -240.0).await?;
        rb = board(window, cx)?;
    }
    anyhow::bail!(
        "Backlog's Load more never came into view: {}",
        rb["controls"]
    )
}

/// Items 16–18: a project past one engine page. Runs last: it adds 531
/// Website tickets.
async fn pagination_pass(
    window: Window,
    cx: &mut AsyncApp,
    engine: &Engine,
    store: &Arc<luna_sync::DocsStore>,
    calls: &Calls,
    hold: &Hold,
    manifest: &mut Manifest,
) -> anyhow::Result<()> {
    let start = mark(calls);
    // Full size, Board view, no search or source filter (item 13 left the
    // List, "o" and Written manually).
    window.update(cx, |_, window, _| {
        window.resize(size(px(WIDTH), px(HEIGHT)))
    })?;
    frames(window, cx, 400).await?;
    let rb = scroll_top(window, cx).await?;
    let switch = control(&rb, "board-view")?;
    click(
        window,
        cx,
        (switch.0 + switch.2 * 0.25, switch.1 + switch.3 / 2.0),
    )
    .await?;
    let rb = wait(window, cx, "the Board view", |rb| rb["view"] == "Board").await?;
    click(window, cx, center(control(&rb, "board-search")?)).await?;
    press(window, cx, "cmd-a").await?;
    press(window, cx, "backspace").await?;
    let rb = wait(window, cx, "the search cleared", |rb| {
        rb["filters"]["text"] == ""
    })
    .await?;
    // Filters rows: attention, any provider, three providers, then Any source.
    click(window, cx, center(control(&rb, "board-filters")?)).await?;
    for _ in 0..6 {
        press(window, cx, "down").await?;
    }
    press(window, cx, "enter").await?;
    wait(window, cx, "any source", |rb| {
        rb["filters"]["source"].is_null() && rb["menu"].is_null()
    })
    .await?;

    // Seed 530 Website tickets at the bottom through the Board RPC; five
    // titles carry "zebra" (two of them past the first page). The last four
    // then go to Done and Ready as stored records with no execution intent
    // (the lifecycle never starts a saved Ready), so they sort after every
    // Backlog ticket in the engine's (project, column, rank) order.
    let seeded = Instant::now();
    let zebra = [7usize, 250, 480, 505, 520];
    let mut paged = Vec::with_capacity(530);
    for n in 1..=530usize {
        let title = if zebra.contains(&n) {
            format!("Zebra crossing sign {n:03}")
        } else {
            format!("Paged backlog ticket {n:03}")
        };
        paged.push(
            engine
                .create(P_SITE, &format!("S{n:03}"), &title, BoardPlacement::Bottom)
                .await?,
        );
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64;
    for (n, to) in [
        (527usize, BoardColumn::Done),
        (528, BoardColumn::Done),
        (529, BoardColumn::Ready),
        (530, BoardColumn::Ready),
    ] {
        let id = &paged[n - 1];
        let item = store
            .board_item(id)
            .map_err(|error| anyhow::anyhow!("reading {id}: {error:?}"))?
            .ok_or_else(|| anyhow::anyhow!("seeded ticket {id} is missing"))?;
        let mut moved = item.clone();
        moved.column = to;
        moved.revision += 1;
        moved.updated_at_ms = now_ms;
        let event = match to {
            BoardColumn::Done => WorkItemEventKind::Completed {
                execution_generation: 0,
            },
            _ => WorkItemEventKind::Moved {
                from: BoardColumn::Backlog,
                to,
                intent_revision: 0,
            },
        };
        store
            .board_commit(
                luna_sync::BoardCommit {
                    request_id: format!("fixture-paged-{n}"),
                    payload_hash: "fixture".into(),
                    committed_at_ms: now_ms,
                    item: luna_sync::BoardItemWrite::Replace {
                        expected_revision: item.revision,
                        item: Box::new(moved),
                        placement: Some(luna_sync::BoardRankPlacement::Bottom),
                    },
                    rows: Vec::new(),
                    event: Some((event, true)),
                    receipt: None,
                },
                None,
            )
            .map_err(|error| anyhow::anyhow!("seeding S{n:03} into {to:?}: {error:?}"))?;
    }
    let site = BoardScope::Project {
        project_id: P_SITE.into(),
    };
    let backlog = engine
        .all_ids(site.clone(), BoardColumn::Backlog, None)
        .await?;
    let ready = engine
        .all_ids(site.clone(), BoardColumn::Ready, None)
        .await?;
    let done = engine
        .all_ids(site.clone(), BoardColumn::Done, None)
        .await?;
    anyhow::ensure!(
        backlog.len() > 500
            && ready.ends_with(&[paged[528].clone(), paged[529].clone()])
            && done.ends_with(&[paged[526].clone(), paged[527].clone()]),
        "item 16: seeding did not produce the paged Website board: {} Backlog, Ready {ready:?}, Done {done:?}",
        backlog.len()
    );
    let seed_ms = seeded.elapsed().as_millis();

    // Item 16: Website opens on its first page per column.
    let rb = scroll_top(window, cx).await?;
    click(window, cx, center(control(&rb, "board-project")?)).await?;
    for _ in 0..4 {
        press(window, cx, "down").await?;
    }
    press(window, cx, "enter").await?;
    let rb = wait(window, cx, "the paged Website board", |rb| {
        rb["scope"]["projectId"] == P_SITE
            && rb["status"] == "Live"
            && column_ids(rb, "backlog").len() >= 500
    })
    .await?;
    let png = capture(window, cx, manifest, "g-first-page")?;
    let snapshot = manifest.json_snapshot("g-first-page-readback", &rb)?;
    anyhow::ensure!(
        column_ids(&rb, "ready") == ready && column_ids(&rb, "done") == done,
        "item 16: later Ready/Done tickets are unreachable: Ready shows {:?} of {ready:?}, Done shows {:?} of {done:?} (Backlog shows {} of {}; header {:?})",
        column_ids(&rb, "ready"),
        column_ids(&rb, "done"),
        column_ids(&rb, "backlog").len(),
        backlog.len(),
        rb["subtitle"]
    );
    let first_page = luna_proto::BOARD_LIST_MAX_LIMIT as usize;
    anyhow::ensure!(
        column_ids(&rb, "backlog") == backlog[..first_page],
        "item 16: Backlog's first page differs from the engine's"
    );
    let (loaded, total, more, pages, text) = paging(&rb, "backlog");
    anyhow::ensure!(
        loaded == first_page
            && total == backlog.len()
            && more == true
            && pages == 1
            && text == format!("{first_page} of {}", backlog.len()),
        "item 16: Backlog must say {first_page} of {} with more to load: loaded {loaded}, total {total}, hasMore {more}, pages {pages}, count {text}",
        backlog.len()
    );
    for (name, ids) in [("ready", &ready), ("done", &done)] {
        let (loaded, total, more, _, text) = paging(&rb, name);
        anyhow::ensure!(
            loaded == ids.len()
                && total == ids.len()
                && more == false
                && text == ids.len().to_string(),
            "item 16: {name} must be complete with the engine's count {}: loaded {loaded}, total {total}, hasMore {more}, count {text}",
            ids.len()
        );
    }
    control(&rb, "board-load-more-backlog")?;
    for name in ["ready", "done"] {
        anyhow::ensure!(
            rb["controls"][format!("board-load-more-{name}")].is_null(),
            "item 16: a complete {name} column offers Load more"
        );
    }
    manifest.step(
        "pagination-first-page",
        "a 528-ticket Backlog shows its first 500 (500 of 528, Load more) while the Ready and Done tickets that sort after all of them are shown with the engine's counts",
        json!({"backlog": backlog.len(), "ready": ready, "done": done, "seedMs": seed_ms, "png": png, "readback": snapshot}),
    );

    // Load more: one bounded column read with the engine's cursor; the
    // selection and focus stay on the last first-page card.
    let rb = reveal_load_more(window, cx).await?;
    let anchor = backlog[first_page - 1].clone();
    let engine_cursor = engine
        .call(
            methods::BOARD_LIST,
            serde_json::to_value(BoardListRequest {
                scope: site.clone(),
                columns: vec![BoardColumn::Backlog],
                limit: Some(luna_proto::BOARD_LIST_MAX_LIMIT),
                ..Default::default()
            })?,
        )
        .await?["nextCursor"]
        .clone();
    let before = mark(calls);
    click(window, cx, center(control(&rb, "board-load-more-backlog")?)).await?;
    let rb = wait(window, cx, "the second Backlog page", |rb| {
        column_ids(rb, "backlog").len() == backlog.len()
    })
    .await?;
    let made = calls_since(calls, before);
    let lists = list_calls(&made);
    let png = capture(window, cx, manifest, "g-load-more")?;
    let snapshot = manifest.json_snapshot("g-load-more-readback", &rb)?;
    anyhow::ensure!(
        lists.len() == 1
            && lists[0]["columns"] == json!(["backlog"])
            && lists[0]["cursor"] == engine_cursor
            && lists[0]["scope"]["projectId"] == P_SITE,
        "item 16: Load more must be one Backlog BoardList with the engine's cursor {engine_cursor}: {lists:?}"
    );
    ensure_bounded(&lists, "item 16")?;
    ensure_no_execution(&made, "item 16")?;
    let ids = column_ids(&rb, "backlog");
    anyhow::ensure!(
        ids == backlog && unique(&ids),
        "item 16: Backlog after Load more differs from the engine's order or repeats a ticket"
    );
    let (_, total, more, pages, text) = paging(&rb, "backlog");
    anyhow::ensure!(
        total == backlog.len() && more == false && pages == 2 && text == backlog.len().to_string(),
        "item 16: a fully loaded Backlog must say {}: total {total}, hasMore {more}, pages {pages}, count {text}",
        backlog.len()
    );
    anyhow::ensure!(
        rb["selected"] == anchor.as_str() && rb["focused"] == anchor.as_str(),
        "item 16: Load more moved the selection/focus from {anchor}: selected {}, focused {}",
        rb["selected"],
        rb["focused"]
    );
    anyhow::ensure!(
        rb["controls"]["board-load-more-backlog"].is_null(),
        "item 16: a complete Backlog still offers Load more"
    );
    manifest.step(
        "pagination-load-more",
        "Load more is one bounded Backlog BoardList with the engine's cursor; Backlog then matches the engine (528, no duplicates) and the selected card keeps selection and focus",
        json!({"call": lists[0], "png": png, "readback": snapshot}),
    );

    // Item 17: select a page-2 card, then a ticket created at the top
    // shifts every ticket by one across the page boundary.
    press(window, cx, "end").await?;
    let tail = backlog.last().unwrap().clone();
    wait(window, cx, "a page-2 card selected", |rb| {
        rb["selected"] == tail.as_str() && rb["focused"] == tail.as_str()
    })
    .await?;
    let before = mark(calls);
    let live = engine
        .create(
            P_SITE,
            "SL1",
            "Live ticket created at the top",
            BoardPlacement::Top,
        )
        .await?;
    let backlog = engine
        .all_ids(site.clone(), BoardColumn::Backlog, None)
        .await?;
    anyhow::ensure!(
        backlog.first() == Some(&live),
        "item 17: the live ticket is not first in the engine's Backlog"
    );
    let rb = wait(window, cx, "the live ticket with every page", |rb| {
        column_ids(rb, "backlog") == backlog
    })
    .await?;
    let made = calls_since(calls, before);
    let lists = list_calls(&made);
    let png = capture(window, cx, manifest, "g-live-update")?;
    let snapshot = manifest.json_snapshot("g-live-update-readback", &rb)?;
    ensure_bounded(&lists, "item 17")?;
    ensure_no_execution(&made, "item 17")?;
    anyhow::ensure!(
        lists
            .iter()
            .any(|params| params["columns"] == json!(["backlog"]) && params["cursor"].is_string()),
        "item 17: the live re-read did not re-read Backlog's second page: {lists:?}"
    );
    let (_, total, more, pages, _) = paging(&rb, "backlog");
    anyhow::ensure!(
        unique(&column_ids(&rb, "backlog"))
            && total == backlog.len()
            && more == false
            && pages == 2,
        "item 17: the re-read must keep both pages without duplicates: total {total}, hasMore {more}, pages {pages}"
    );
    anyhow::ensure!(
        column_ids(&rb, "ready") == ready && column_ids(&rb, "done") == done,
        "item 17: the live re-read changed Ready/Done"
    );
    anyhow::ensure!(
        rb["selected"] == tail.as_str() && rb["focused"] == tail.as_str(),
        "item 17: the live update moved the selection/focus from page-2 card {tail}: selected {}, focused {}",
        rb["selected"],
        rb["focused"]
    );
    manifest.step(
        "pagination-live-update",
        "a ticket created at the top after Load more shows first; the re-read keeps both pages (529, no duplicates, engine order) and the selected page-2 card keeps selection and focus",
        json!({"live": live, "reads": lists.len(), "png": png, "readback": snapshot}),
    );

    // Item 18: All projects resets to each column's first page.
    let rb = scroll_top(window, cx).await?;
    click(window, cx, center(control(&rb, "board-project")?)).await?;
    press(window, cx, "down").await?;
    press(window, cx, "enter").await?;
    let all = BoardScope::AllProjects;
    let all_backlog = engine
        .all_ids(all.clone(), BoardColumn::Backlog, None)
        .await?;
    let all_ready = engine
        .all_ids(all.clone(), BoardColumn::Ready, None)
        .await?;
    let all_done = engine.all_ids(all.clone(), BoardColumn::Done, None).await?;
    let rb = wait(window, cx, "All projects' first pages", |rb| {
        rb["scope"]["kind"] == "allProjects"
            && rb["status"] == "Live"
            && column_ids(rb, "backlog") == all_backlog[..first_page]
    })
    .await?;
    let png = capture(window, cx, manifest, "g-all-projects")?;
    let (_, total, more, pages, _) = paging(&rb, "backlog");
    anyhow::ensure!(
        total == all_backlog.len() && more == true && pages == 1,
        "item 18: All projects must start on Backlog's first page: total {total}, hasMore {more}, pages {pages}"
    );
    anyhow::ensure!(
        column_ids(&rb, "ready") == all_ready && column_ids(&rb, "done") == all_done,
        "item 18: All projects' Ready/Done differ from the engine's"
    );
    // A search typed while Load more is in flight: the transport's gate
    // holds the cursor read until the search's result is on screen, so the
    // search's reads provably reach the engine first and the stale page
    // answers after them. No stale ticket or page may survive.
    let rb = reveal_load_more(window, cx).await?;
    let more_at = center(control(&rb, "board-load-more-backlog")?);
    let before = mark(calls);
    hold.set_open(false);
    let clicked = Instant::now();
    hover(window, cx, more_at, None)?;
    draw(window, cx)?;
    button(window, cx, more_at, MouseButton::Left, true)?;
    button(window, cx, more_at, MouseButton::Left, false)?;
    frames(window, cx, 120).await?;
    let loading = board(window, cx)?;
    let in_flight = list_calls(&calls_since(calls, before))
        .iter()
        .any(|params| params["cursor"].is_string());
    let held_at_click = hold.held.load(AtomicOrdering::SeqCst);
    anyhow::ensure!(
        in_flight && held_at_click == 1 && column(&loading, "backlog")?["loading"] == true,
        "item 18: Load more did not issue a held cursor read (held {held_at_click}) or show it loading: {:?}",
        column(&loading, "backlog")?
    );
    let loading_png = capture(window, cx, manifest, "g-load-more-in-flight")?;
    press(window, cx, "cmd-f").await?;
    type_text(window, cx, "zebra").await?;
    let zebras = engine
        .all_ids(all.clone(), BoardColumn::Backlog, Some("zebra"))
        .await?;
    anyhow::ensure!(
        zebras.len() == zebra.len(),
        "item 18: the engine's zebra search found {zebras:?}"
    );
    wait(window, cx, "only the search's tickets", |rb| {
        rb["filters"]["text"] == "zebra" && column_ids(rb, "backlog") == zebras
    })
    .await?;
    let searched_ms = clicked.elapsed().as_millis() as u64;
    let made = calls_since(calls, before);
    let ordered = made
        .iter()
        .position(|(m, _, p)| m == methods::BOARD_LIST && p["cursor"].is_string())
        < made
            .iter()
            .position(|(m, _, p)| m == methods::BOARD_LIST && p["text"] == "zebra");
    let overlapped = hold.overlapped.load(AtomicOrdering::SeqCst);
    // 1: the Load more is still in flight; 0: the UI abandoned it (the
    // transport dropped the held call).
    let held_at_result = hold.held.load(AtomicOrdering::SeqCst);
    anyhow::ensure!(
        ordered && overlapped >= 1,
        "item 18: the search's reads did not reach the engine while Load more was in flight (search reads during the held read: {overlapped}; shown after {searched_ms} ms)"
    );
    // Release the held Load more only now: its stale page (if the UI still
    // waits for it) answers after the search's result.
    hold.set_open(true);
    let mut resolved = false;
    for _ in 0..100 {
        if hold.held.load(AtomicOrdering::SeqCst) == 0 {
            resolved = true;
            break;
        }
        settle(cx, 50).await;
    }
    anyhow::ensure!(resolved, "item 18: the released Load more never resolved");
    frames(window, cx, 1000).await?;
    let held_answered = hold.answered.load(AtomicOrdering::SeqCst);
    let rb = board(window, cx)?;
    let png_search = capture(window, cx, manifest, "g-search-during-load")?;
    let snapshot = manifest.json_snapshot("g-search-during-load-readback", &rb)?;
    let stale: Vec<String> = rb["cards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|card| {
            !card["title"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .contains("zebra")
        })
        .map(|card| card["id"].as_str().unwrap_or_default().to_owned())
        .collect();
    let (_, _, more, pages, _) = paging(&rb, "backlog");
    anyhow::ensure!(
        stale.is_empty() && column_ids(&rb, "backlog") == zebras && more == false && pages == 1,
        "item 18: a search during Load more kept stale tickets {stale:?} or pages (hasMore {more}, pages {pages})"
    );
    // Clearing the search returns to the first page.
    press(window, cx, "cmd-a").await?;
    press(window, cx, "backspace").await?;
    let all_backlog = engine
        .all_ids(all.clone(), BoardColumn::Backlog, None)
        .await?;
    let rb = wait(window, cx, "the first page again", |rb| {
        rb["filters"]["text"] == "" && column_ids(rb, "backlog") == all_backlog[..first_page]
    })
    .await?;
    let (_, _, more, pages, _) = paging(&rb, "backlog");
    anyhow::ensure!(
        more == true && pages == 1,
        "item 18: clearing the search must return to Backlog's first page: hasMore {more}, pages {pages}"
    );
    // Back to Website: its first page, nothing from another project.
    let rb = scroll_top(window, cx).await?;
    click(window, cx, center(control(&rb, "board-project")?)).await?;
    for _ in 0..4 {
        press(window, cx, "down").await?;
    }
    press(window, cx, "enter").await?;
    let rb = wait(window, cx, "Website's first page again", |rb| {
        rb["scope"]["projectId"] == P_SITE
            && rb["status"] == "Live"
            && column_ids(rb, "backlog") == backlog[..first_page]
    })
    .await?;
    let png_back = capture(window, cx, manifest, "g-website-again")?;
    let snapshot_back = manifest.json_snapshot("g-website-again-readback", &rb)?;
    let foreign: Vec<&Value> = rb["cards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|card| card["project"] != P_SITE)
        .collect();
    let (_, total, more, pages, text) = paging(&rb, "backlog");
    anyhow::ensure!(
        foreign.is_empty()
            && total == backlog.len()
            && more == true
            && pages == 1
            && text == format!("{first_page} of {}", backlog.len())
            && column_ids(&rb, "ready") == ready
            && column_ids(&rb, "done") == done,
        "item 18: Website after All projects: foreign {foreign:?}, total {total}, hasMore {more}, pages {pages}, count {text}"
    );
    let all_lists = list_calls(&calls_since(calls, start));
    ensure_bounded(&all_lists, "items 16–18")?;
    manifest.step(
        "pagination-scope-and-filter",
        "All projects starts on each column's first page and matches the engine; a search typed while Load more is in flight shows only its five tickets (no stale page); clearing it and switching back to Website return to the first page with nothing from another scope",
        json!({
            "heldAtClick": held_at_click,
            "heldWhenSearchShown": held_at_result,
            "searchShownAfterMs": searched_ms,
            "searchReadsDuringHeldLoadMore": overlapped,
            "heldLoadMoreAnsweredByEngine": held_answered,
            "loadMoreInFlightPng": loading_png,
            "allProjectsPng": png,
            "searchPng": png_search,
            "searchReadback": snapshot,
            "websitePng": png_back,
            "websiteReadback": snapshot_back,
            "boardListCalls": all_lists.len(),
        }),
    );
    Ok(())
}

/// End a failed run with a nonzero status. The GPUI run loop can end the
/// process on quit with status 0, so a failure (whose failure.txt,
/// failure.png and manifest are already written) never reaches it. The
/// fixture's temporary data root goes with it, as on a normal exit.
fn exit_failed(temp_root: &Path) -> ! {
    if let Err(error) = std::fs::remove_dir_all(temp_root) {
        eprintln!(
            "board-page fixture: temporary root {} left behind: {error}",
            temp_root.display()
        );
    }
    std::process::exit(1)
}

/// `Working` → `working` (the wire's camelCase phase names).
fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn mock_script() -> Vec<AgentEvent> {
    let mut script: Vec<AgentEvent> = (0..14)
        .map(|ix| AgentEvent::TextDelta {
            text: format!("Working through the brief, step {ix}. "),
        })
        .collect();
    script.push(AgentEvent::Done {
        status: DoneStatus::Completed,
        result: None,
        error: None,
        session_id: None,
    });
    script
}

fn main() -> anyhow::Result<()> {
    // SAFETY: set before the Tokio runtime, the GPUI platform or any other
    // thread exists, and never changed afterwards; the in-process Mock
    // harness only reads them.
    unsafe {
        std::env::set_var("LUNA_MOCK_DELAY_MS", MOCK_DELAY_MS);
        std::env::set_var("LUNA_HARNESS", "mock");
    }
    let output = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/board/page")
        });
    std::fs::create_dir_all(&output)?;
    let output = output.canonicalize()?;
    for stale in [
        "manifest.json",
        "board-page.pass",
        "failure.txt",
        "failure.png",
    ] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let root = tempfile::tempdir()?;
    // A failed run exits from inside the GPUI loop (below), where the
    // tempdir's drop never runs: it removes this root itself.
    let temp_root = root.path().to_path_buf();
    let ui_data = root.path().join("ui");
    let engine_data = root.path().join("engine");
    std::fs::create_dir_all(&ui_data)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: mock_script(),
    }));
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&engine_data, "board-page-fixture", "board-page-fixture"),
            Arc::new(registry),
            HarnessId::Mock,
            None,
            AgentAccountsConfig {
                data_dir: engine_data.clone(),
                claude_config_dir: root.path().join("providers/claude"),
                claude_config_file: root.path().join("providers/claude.json"),
                codex_home: root.path().join("providers/codex"),
            },
        )
    })?;
    let mut auth = AuthConfig::new("", root.path().join("auth"));
    auth.dev_user_id = "board-page-fixture".into();
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let device = core.device_id.clone();
    let store = core.store.clone();
    for (id, name) in [(P_LUNA, "Luna"), (P_SITE, "Website"), (P_NOTES, "Notes")] {
        let path = root.path().join(name.to_lowercase());
        std::fs::create_dir_all(&path)?;
        core.workspace.create_space(
            id,
            &device,
            path.to_str().unwrap(),
            Some(name.into()),
            false,
        )?;
    }
    let engine = Engine {
        rt: runtime.handle().clone(),
        client: Arc::new(luna_rpc::memory_client(core.rpc_service())),
    };
    // Seed through the Board RPC, bottom placement so the order is L1..L4.
    let seeds: Vec<(String, &'static str)> = runtime.block_on(async {
        let mut seeds = Vec::new();
        for (project, key, title) in [
            (P_LUNA, "L1", "Fix login redirect loop"),
            (P_LUNA, "L2", "Add audit log export"),
            (P_LUNA, "L3", "Polish settings copy"),
            (P_LUNA, "L4", "Speed up cold start"),
            (P_SITE, "W1", "Refresh pricing page"),
            (P_SITE, "W2", "Fix broken footer links"),
        ] {
            let id = engine
                .create(project, key, title, BoardPlacement::Bottom)
                .await?;
            seeds.push((id, key));
        }
        anyhow::Ok(seeds)
    })?;
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let hold = Arc::new(Hold::new());
    let service: Arc<dyn luna_rpc::RpcService> = Arc::new(CountingRpc {
        inner: core.rpc_service(),
        calls: calls.clone(),
        hold: hold.clone(),
    });
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core_with_service(
        &core, service,
    ))?;
    let spaces = core.workspace.read_spaces()?;
    let chats = core.workspace.read_chats()?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 0,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let failure = Arc::new(Mutex::new(None::<String>));
    let failure_for_ui = failure.clone();
    gpui_platform::headless()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let mut settings = settings::UiSettings::default();
            // Opening the Board from the Luna project prefilters it.
            settings.space_filter = Some(P_LUNA.into());
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
            let state = cx.new(|_| {
                let mut state = state::AppState::new();
                state.local_device_id = Some(device.clone());
                state.devices = vec![
                    serde_json::from_value(json!({
                        "id": device, "name": "Studio Mac", "platform": "macos", "lastSeenAt": null
                    }))
                    .unwrap(),
                ];
                state.spaces = spaces;
                state.chats = chats;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.auto_selected = true;
                state.connection = state::ConnectionStatus::Ready;
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
                state.fixture_attachment_engine(handle);
                state
            });
            let open = {
                let state = state.clone();
                let boot = boot.clone();
                move |cx: &mut gpui::App, width: f32, height: f32| -> anyhow::Result<Window> {
                    let state = state.clone();
                    let boot = boot.clone();
                    let window = cx.open_window(
                        WindowOptions {
                            window_background: theme::Theme::of(cx).window_background_appearance(),
                            show: false,
                            focus: false,
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(20.0), px(20.0)),
                                size(px(width), px(height)),
                            ))),
                            ..Default::default()
                        },
                        |window, cx| {
                            window.set_rem_size(px(typography::font_size(cx).pixels()));
                            cx.new(|cx| shell::Shell::new(state, boot, cx))
                        },
                    )?;
                    let _ = window.update(cx, |shell, window, cx| {
                        window.activate_window();
                        shell.fixture_enable_sidebar_probe(cx);
                    });
                    Ok(window)
                }
            };
            let window = match open(cx, WIDTH, HEIGHT) {
                Ok(window) => window,
                Err(error) => {
                    eprintln!("board-page fixture failed: {error:#}");
                    let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                    exit_failed(&temp_root);
                }
            };
            let out = output.clone();
            let failure = failure_for_ui.clone();
            cx.spawn(async move |cx| {
                let mut manifest = Manifest {
                    out: out.clone(),
                    steps: Vec::new(),
                    snapshots: Vec::new(),
                };
                let mut current = window;
                let result: anyhow::Result<()> = async {
                    cx.update(|cx| motion::set_reduced_motion(cx, true));
                    // The boot overlay fades after the first state frame.
                    frames(current, cx, 1500).await?;
                    navigation_pass(current, cx, &mut manifest).await?;
                    for font in [
                        typography::UiFontSize::ALL[0],
                        typography::UiFontSize::ALL[4],
                        typography::UiFontSize::ALL[6],
                    ] {
                        scale_pass(current, cx, &engine, &mut manifest, font).await?;
                    }
                    behaviour_pass(current, cx, &engine, &calls, &mut manifest, &seeds).await?;
                    let data = ui_data.clone();
                    current = prefs_pass(current, cx, &mut manifest, &data, |cx, height| {
                        cx.update(|cx| open(cx, WIDTH, height))
                    })
                    .await?;
                    convergence_pass(current, cx, &mut manifest, |cx| {
                        cx.update(|cx| open(cx, NARROW_WINDOW, HEIGHT))
                    })
                    .await?;
                    pagination_pass(
                        current,
                        cx,
                        &engine,
                        &store,
                        &calls,
                        &hold,
                        &mut manifest,
                    )
                    .await?;
                    Ok(())
                }
                .await;
                match result {
                    Ok(()) => {
                        let written = manifest.write("pass").and_then(|_| {
                            std::fs::write(
                                out.join("board-page.pass"),
                                "PASS: headless production Shell, real in-process engine and Mock harness, real GPUI drags and keystrokes at Interface sizes 12, 16 and 20: Board sits directly below Automations; the sidebar filter prefilters and All projects aggregates with project names; Backlog/Ready/Done with help and engine counts; a drag to Ready is one BoardMove and shows the engine's phase; a drag back is one BoardMove and never claims Stopped before the engine; a reorder is one BoardRank with no launch; a drop on Done calls nothing; keyboard moves, drawer, card menu, archive/unarchive and announcements keep focus; a live insert keeps the focused card; narrow windows get the List with the same actions; the empty state offers four paths without GitHub; presentation prefs restore after re-creating the Shell; past one engine page every column stays reachable: Load more is one bounded cursor read per column, live re-reads keep every loaded page without duplicates and keep the selection, and scope or search changes return to the first page\n",
                            )
                            .map_err(anyhow::Error::from)
                        });
                        if let Err(error) = written {
                            eprintln!("board-page fixture could not write evidence: {error:#}");
                            let _ = std::fs::write(
                                out.join("failure.txt"),
                                format!("could not write evidence: {error:#}\n"),
                            );
                            *failure.lock().unwrap() = Some(format!("{error:#}"));
                            exit_failed(&temp_root);
                        }
                    }
                    Err(error) => {
                        eprintln!("board-page fixture failed: {error:#}");
                        let _ = std::fs::write(out.join("failure.txt"), format!("{error:#}\n"));
                        let _ = gpui::AnyWindowHandle::from(current).update(cx, |_, window, cx| {
                            window.draw(cx).clear();
                            if let Ok(image) = window.render_to_image() {
                                let _ = image.save(out.join("failure.png"));
                            }
                        });
                        let _ = manifest.write("fail");
                        *failure.lock().unwrap() = Some(format!("{error:#}"));
                        exit_failed(&temp_root);
                    }
                }
                let _ = gpui::AnyWindowHandle::from(current)
                    .update(cx, |_, window, _| window.remove_window());
                // A passing run ends here too: the GPUI run loop can end the
                // process on quit, before the tempdir's drop runs. Persist the
                // last settings revision first and drop its debounced write,
                // so nothing recreates the root after it is removed.
                settle(cx, 200).await;
                cx.update(|cx| settings::flush(cx));
                if let Err(error) = std::fs::remove_dir_all(&temp_root) {
                    eprintln!(
                        "board-page fixture: temporary root {} left behind: {error}",
                        temp_root.display()
                    );
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    runtime.block_on(core.shutdown());
    if let Some(error) = failure.lock().unwrap().take() {
        anyhow::bail!(error);
    }
    Ok(())
}
