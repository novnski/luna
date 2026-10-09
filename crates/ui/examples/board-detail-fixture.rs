//! Headless GPUI E2E for the Board detail slice (A9): the New ticket chooser,
//! the Write manually form with its implementation/review settings, and the
//! ticket detail drawer (brief, settings, actions, dispatch history, review,
//! evidence).
//!
//! Drives the production `Shell` in a hidden window with real pointer and
//! keyboard input (GPUI hit testing on painted control bounds, typed keys,
//! Enter, Escape, wheel scrolling) against an in-process `EngineCore` with a
//! temporary data directory and a scripted Mock harness. Every catalog
//! choice comes from the engine's `BoardExecutionOptions`; every saved value
//! is read back through `BoardRead`. No provider, network or GitHub access.
//!
//! Failure cases this run must catch (written before the implementation):
//! 1. The chooser merges the two experiences: Write manually and Discuss with
//!    AI are not two distinct choices, Discuss with AI renders a title or
//!    description input (beside, below or hidden in the pane), or it does not
//!    mount the temporary authoring pane.
//! 2. The manual form's settings are not the chat composer's own pickers:
//!    the agent row lacks the model pill, the access lock or the checkout, the
//!    reviewer row shows an access lock, a picker lists a model that is not
//!    in the host's `BoardExecutionOptions`, the reasoning tray offers a level
//!    the selected model does not support, or a pick by search + Enter, the
//!    tray (Right, Enter) or the access menu does not reach the saved draft.
//!    A pick must not touch the chat composer's sticky defaults
//!    (`composer-defaults.json`).
//! 3. A model change keeps an incompatible reasoning level in the draft or on
//!    screen, or resets it without telling the user.
//! 4. Inherited values are unexplained: in Project defaults the model pill
//!    shows something other than the engine's effective model, the agent
//!    row does not say it inherits the project's settings, or the review
//!    profile with no reviewer configured does not say that the
//!    implementation model reviews in a separate reviewer chat.
//! 5. Validation is implicit: an empty title is sent to the engine (the ticket
//!    count changes) or its error is not shown in the form.
//! 6. Save to Backlog creates a chat, a launch or a second ticket (double
//!    click), saves other settings than the ones selected, or its receipt
//!    omits the key, the "no chat" statement or the saved settings.
//! 7. The detail drawer shows other requested settings than the saved ones,
//!    or claims resolved/observed values that no launch or observation
//!    recorded (unknown must read as not resolved / not observed).
//! 8. A brief edit from a stale revision silently overwrites a concurrent
//!    change or loses the user's text, or the conflict offers no explicit
//!    choice; keeping mine must save over the current revision explicitly.
//! 9. Rename does not go through the engine (title and revision unchanged)
//!    or Enter does not submit it.
//! 10. Send update to chat is offered while nothing runs, or is missing while
//!     the run is active with an unsent brief revision; sending does not add
//!     a brief-update dispatch through the engine.
//! 11. Start is not the engine move (no first-start launch, wrong column), or
//!     the dispatch history counts disagree with the engine's launch
//!     snapshots (first start, continuations, brief updates).
//! 12. Review and evidence are not rendered from the engine's records: repair
//!     rounds not shown as "N of 2", a finding without its severity word or
//!     blocking label (colour-only meaning), no reviewer model or phase, no
//!     Review details link for the archived reviewer, a cleanup state other
//!     than the engine's own cleanup readback (the reviewer is a real chat
//!     archived through the engine); outcomes (diff stats, pull request,
//!     checks, artifacts) without their state or actual actor.
//! 13. Review details does not open the archived reviewer chat (route and
//!     selection), or opening it unarchives it.
//! 14. Escape does not close the drawer; in a narrow window the drawer is
//!     wider than the page or a control paints outside it; the header with
//!     the key, title and Close scrolls away with the body, or the pinned
//!     Close does not close the drawer.
//! 15. Two controls share an element id, which the accessibility tree
//!     rejects (a debug build panics): a model listed twice, repeated
//!     reasoning levels, the two profiles' pickers side by side in the
//!     drawer, or a harness switch (Claude Code, Codex, OpenCode and back)
//!     through each profile's model picker. The run builds the tree every
//!     frame and fails on any panic.
//! 16. The Write manually description is not the chat composer: it has no
//!     attach control, `@` offers no project file, or Enter submits the
//!     form (or sends) instead of breaking the line.
//! 17. An image dropped on the description is not saved with the ticket
//!     (BoardRead lacks its upload path), or an `@` file picked there is not
//!     in the saved brief as the composer's file link.
//! 19. Generate title is offered before the brief says enough (about 100
//!     characters), or once offered it does not fill the title from the
//!     device's title model, or it saves anything.
//! 18. The ticket's first start does not deliver the brief's image: the
//!     launch snapshot lacks it, or the working chat's first message has no
//!     `Attached images` block naming it.
//! 20. After Save to Backlog the keyboard is lost: the field that held it
//!     is gone and nothing in the receipt takes it, so Enter cannot open the
//!     ticket (and Escape cannot reach the drawer).
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example board-detail-fixture
//! Run:   target/debug/examples/board-detail-fixture [ABSOLUTE_EVIDENCE_DIR]
//! Default evidence directory: target/qa/board/detail/. Writes PNG captures,
//! readback.json and manifest.json (steps with expected/observed values and
//! SHA-256 of every capture) plus board-detail.pass; a failure writes
//! failure.txt/failure.png and exits nonzero.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail, ensure};
use gpui::{
    AppContext, AsyncApp, Bounds, Entity, Keystroke, MouseButton, PlatformInput, WindowBounds,
    WindowHandle, WindowOptions, point, px, size,
};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::mock::MockHarness;
use luna_proto::{
    AgentEvent, BoardActor, BoardChatRole, BoardColumn, BoardContextMode, BoardDependency,
    BoardDestination, BoardExecutionIntent, BoardExecutionOptions, BoardFollowUp,
    BoardInstructionKind, BoardItemDetail, BoardListPage, BoardMutationReceipt, BoardPhase,
    BoardSettingOrigin, BoardSettingSource, DependencySource, DoneStatus, FirstPromptDelivery,
    HarnessId, IssueLinkRole, IssueProvider, IssueReference, IssueRepository, LaunchSnapshot,
    Model, ModelOption, ModelOptionChoice, OriginalContainer, OutcomeKind, OutcomeReference,
    PermissionMode, ReasoningLevel, ResolvedExecution, ReviewBasis, ReviewCleanup,
    ReviewCleanupState, ReviewFinding, ReviewProfileResolution, ReviewResult, ReviewRun,
    ReviewRunOutcome, ReviewRunState, ReviewSeverity, ReviewVerdict, ReviewerPlacement,
    TaskLinkRole, WorkItem, WorkItemEventKind, WorkItemTaskLink,
};
use luna_rpc::methods;
use luna_ui::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const PROJECT: &str = "board-detail-project";
const PROJECT_NAME: &str = "Luna Board QA";
/// The working chat and the archived reviewer of the evidence ticket.
const EV_MAIN: &str = "b9d1a000-0000-4000-8000-000000000001";
const EV_REVIEW: &str = "b9d1a000-0000-4000-8000-000000000002";
const WIDTH: f32 = 1360.0;
const HEIGHT: f32 = 900.0;
const NARROW: f32 = 760.0;
/// Spaces the scripted provider's events so a started run stays active
/// long enough to edit the brief and send an update while it runs.
const MOCK_DELAY_MS: &str = "1200";
/// The drawer's cleanup lines for the engine's Archived and Closed states.
const CLEANUP_ARCHIVED: &str = "Cleanup: Archived; the review history is kept.";
const CLEANUP_CLOSED: &str = "Cleanup: Archived and closed; the review history is kept.";
/// What the scripted title model answers for a ticket title.
const GENERATED_TITLE: &str = "Retry failed uploads three times";

const REVIEW_FALLBACK: &str =
    "No reviewer is configured: the implementation model reviews in a separate reviewer chat.";

type Window = WindowHandle<shell::Shell>;

// ---------------------------------------------------------------------------
// A deterministic provider peer
// ---------------------------------------------------------------------------

/// A scripted provider peer registered under a real harness id (Claude
/// Code, Codex, OpenCode) with its own catalog. Runs report a native
/// session and stream slowly.
///
/// The catalogs are deliberately hostile to element ids derived from catalog
/// values: a model listed twice, a model option whose choice is `default`
/// beside the editor's Default chip, repeated reasoning levels, model ids
/// spelled like the editor's own control names (`inherit`, `default`,
/// `show-all`, `more`), and more models than the editor lists before "Show
/// all". Codex's first two models drive the manual-form scenario.
struct ScriptedPeer {
    harness: HarnessId,
    name: &'static str,
    models: Vec<Model>,
    levels: &'static [ReasoningLevel],
}

fn model(id: &str, label: &str, levels: &[ReasoningLevel], options: Vec<ModelOption>) -> Model {
    Model {
        id: id.into(),
        label: label.into(),
        description: None,
        reasoning_levels: levels.to_vec(),
        options,
        first_seen_at_ms: None,
    }
}

fn option(id: &str, label: &str, default: &str, choices: &[(&str, &str)]) -> ModelOption {
    ModelOption {
        id: id.into(),
        label: label.into(),
        choices: choices
            .iter()
            .map(|(id, label)| ModelOptionChoice {
                id: (*id).into(),
                label: (*label).into(),
            })
            .collect(),
        default_choice: default.into(),
    }
}

const LEVELS: &[ReasoningLevel] = &[
    ReasoningLevel::Low,
    ReasoningLevel::Medium,
    ReasoningLevel::High,
    ReasoningLevel::XHigh,
];

fn codex_peer() -> ScriptedPeer {
    use ReasoningLevel::{High, Low, Medium};
    let tier = || {
        option(
            "serviceTier",
            "Service tier",
            "default",
            &[("default", "Standard"), ("fast", "Fast"), ("flex", "Flex")],
        )
    };
    let mut models = vec![
        model("fixture-sol", "Fixture Sol", &[Low, Medium, High], vec![]),
        model("fixture-luna", "Fixture Luna", &[Medium], vec![]),
        model(
            "fixture-astra",
            "Fixture Astra",
            &[Low, Medium, High, High],
            vec![tier()],
        ),
        model("fixture-sol", "Fixture Sol (listed again)", &[Low], vec![]),
    ];
    for (id, label) in [
        ("gpt-6.1-sol", "GPT-6.1 Sol"),
        ("gpt-6-astra", "GPT-6 Astra"),
        ("gpt-6-sol", "GPT-6 Sol"),
        ("gpt-6-luna", "GPT-6 Luna"),
        ("gpt-5.6-sol", "GPT-5.6 Sol"),
        ("gpt-5.6-terra", "GPT-5.6 Terra"),
        ("gpt-5.6-luna", "GPT-5.6 Luna"),
        ("inherit", "Inherit"),
        ("default", "Default"),
        ("show-all", "Show all"),
        ("more", "More"),
    ] {
        models.push(model(id, label, &[Medium, High], vec![tier()]));
    }
    ScriptedPeer {
        harness: HarnessId::Codex,
        name: "Codex (fixture peer)",
        models,
        levels: &[Low, Medium, High],
    }
}

fn claude_peer() -> ScriptedPeer {
    use ReasoningLevel::{High, Low, Medium, XHigh};
    let thinking = || {
        option(
            "thinking",
            "Thinking",
            "off",
            &[("off", "Off"), ("on", "On")],
        )
    };
    let context = || {
        option(
            "context",
            "Context window",
            "200k",
            &[("200k", "200K"), ("1m", "1M"), ("default", "Default")],
        )
    };
    ScriptedPeer {
        harness: HarnessId::ClaudeCode,
        name: "Claude Code (fixture peer)",
        models: vec![
            model(
                "fixture-opus",
                "Fixture Opus",
                &[Low, Medium, High, XHigh],
                vec![thinking(), context()],
            ),
            model(
                "fixture-sonnet",
                "Fixture Sonnet",
                &[Low, Medium, High],
                vec![thinking()],
            ),
            model("default", "Default", &[Medium], vec![]),
            model(
                "fixture-opus",
                "Fixture Opus (listed again)",
                &[Low],
                vec![],
            ),
        ],
        levels: LEVELS,
    }
}

fn opencode_peer() -> ScriptedPeer {
    use ReasoningLevel::{High, Medium};
    let variant = || {
        option(
            "variant",
            "Variant",
            "default",
            &[
                ("default", "Default"),
                ("max", "max"),
                ("minimal", "minimal"),
            ],
        )
    };
    ScriptedPeer {
        harness: HarnessId::Opencode,
        name: "OpenCode (fixture peer)",
        models: vec![
            model(
                "fixture-open/sonnet",
                "Fixture Open Sonnet",
                &[Medium, High, High],
                vec![variant(), variant()],
            ),
            model("fixture-open/gpt", "Fixture Open GPT", &[], vec![]),
            model(
                "fixture-open/sonnet",
                "Fixture Open Sonnet (listed again)",
                &[],
                vec![],
            ),
        ],
        levels: &[Medium, High],
    }
}

/// The model each harness switch selects so its option rows render.
fn model_with_options(harness: &str) -> &'static str {
    match harness {
        "codex" => "fixture-astra",
        "claude-code" => "fixture-opus",
        _ => "fixture-open/sonnet",
    }
}

#[async_trait::async_trait]
impl luna_harness::Harness for ScriptedPeer {
    fn id(&self) -> HarnessId {
        self.harness
    }
    fn display_name(&self) -> &str {
        self.name
    }
    fn supports_steering(&self) -> bool {
        true
    }
    fn steering_mode(&self) -> luna_proto::SteeringMode {
        luna_proto::SteeringMode::StepBoundary
    }
    fn reasoning_levels(&self) -> &[ReasoningLevel] {
        self.levels
    }
    async fn models(&self) -> Result<Vec<Model>, luna_harness::HarnessError> {
        Ok(self.models.clone())
    }
    /// The device's title model: a ticket title prompt gets a title.
    async fn run_title(
        &self,
        request: luna_proto::RunRequest,
        controls: luna_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<AgentEvent, luna_harness::HarnessError>>,
        luna_harness::HarnessError,
    > {
        let text = if request
            .prompt
            .starts_with("Write one short, specific ticket title")
        {
            GENERATED_TITLE
        } else {
            "Fixture title"
        };
        let script = vec![
            AgentEvent::TextDelta { text: text.into() },
            AgentEvent::Done {
                status: DoneStatus::Completed,
                result: None,
                error: None,
                session_id: None,
            },
        ];
        MockHarness { script }.run(request, controls).await
    }

    async fn run(
        &self,
        request: luna_proto::RunRequest,
        controls: luna_harness::RunControls,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<AgentEvent, luna_harness::HarnessError>>,
        luna_harness::HarnessError,
    > {
        let session = format!("fixture-session-{}", uuid::Uuid::new_v4().simple());
        let mut script = vec![AgentEvent::SessionStarted {
            harness: self.harness,
            model: request
                .model
                .clone()
                .unwrap_or_else(|| self.models[0].id.clone()),
            tools: Vec::new(),
            cwd: request.cwd.clone(),
            session_id: session.clone(),
            assistant_message_id: format!("fixture-assistant-{}", uuid::Uuid::new_v4().simple()),
        }];
        for step in 1..=14 {
            script.push(AgentEvent::TextDelta {
                text: format!("Working on the ticket, step {step}. "),
            });
        }
        script.push(AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some(session),
        });
        MockHarness { script }.run(request, controls).await
    }
}

// ---------------------------------------------------------------------------
// Evidence: steps, captures, manifest
// ---------------------------------------------------------------------------

struct Run {
    dir: PathBuf,
    steps: Vec<Value>,
    captures: Vec<Value>,
    readbacks: serde_json::Map<String, Value>,
}

impl Run {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            steps: Vec::new(),
            captures: Vec::new(),
            readbacks: serde_json::Map::new(),
        }
    }

    /// Record one expectation; a failed one ends the run.
    fn check(
        &mut self,
        id: &str,
        expected: Value,
        observed: Value,
        pass: bool,
    ) -> anyhow::Result<()> {
        println!("{} {id}", if pass { "ok  " } else { "FAIL" });
        self.steps.push(json!({
            "id": id,
            "expected": expected,
            "observed": observed,
            "pass": pass,
        }));
        ensure!(pass, "{id}: expected {expected}, observed {observed}");
        Ok(())
    }

    fn keep(&mut self, name: &str, readback: &Value) {
        self.readbacks.insert(name.into(), readback.clone());
    }

    fn capture(&mut self, window: Window, cx: &mut AsyncApp, name: &str) -> anyhow::Result<()> {
        let path = self.dir.join(format!("{name}.png"));
        capture(window, cx, &path)?;
        let bytes = std::fs::read(&path)?;
        self.captures.push(json!({
            "name": name,
            "file": path.file_name().map(|name| name.to_string_lossy().to_string()),
            "sha256": hex(&Sha256::digest(&bytes)),
            "bytes": bytes.len(),
        }));
        Ok(())
    }

    fn write(&self, outcome: &str, error: Option<String>) -> anyhow::Result<()> {
        let manifest = json!({
            "fixture": "board-detail-fixture",
            "slice": "A9 board detail",
            "outcome": outcome,
            "error": error,
            "commit": commit(),
            "finishedAt": chrono::Utc::now().to_rfc3339(),
            "steps": self.steps,
            "captures": self.captures,
        });
        std::fs::write(
            self.dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        std::fs::write(
            self.dir.join("readback.json"),
            serde_json::to_vec_pretty(&Value::Object(self.readbacks.clone()))?,
        )?;
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn commit() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into())
}

// ---------------------------------------------------------------------------
// Input and readback
// ---------------------------------------------------------------------------

fn capture(window: Window, cx: &mut AsyncApp, path: &Path) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })??;
    Ok(())
}

fn draw(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
    })?;
    Ok(())
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn input(window: Window, cx: &mut AsyncApp, event: PlatformInput) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(event, cx);
    })?;
    Ok(())
}

fn hover(window: Window, cx: &mut AsyncApp, at: (f32, f32)) -> anyhow::Result<()> {
    input(
        window,
        cx,
        PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position: point(px(at.0), px(at.1)),
            pressed_button: None,
            modifiers: Default::default(),
        }),
    )
}

fn press(window: Window, cx: &mut AsyncApp, at: (f32, f32), down: bool) -> anyhow::Result<()> {
    let position = point(px(at.0), px(at.1));
    input(
        window,
        cx,
        if down {
            PlatformInput::MouseDown(gpui::MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            })
        } else {
            PlatformInput::MouseUp(gpui::MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count: 1,
            })
        },
    )
}

async fn click_at(window: Window, cx: &mut AsyncApp, at: (f32, f32)) -> anyhow::Result<()> {
    hover(window, cx, at)?;
    draw(window, cx)?;
    settle(cx, 60).await;
    press(window, cx, at, true)?;
    press(window, cx, at, false)?;
    settle(cx, 200).await;
    draw(window, cx)?;
    Ok(())
}

fn wheel(window: Window, cx: &mut AsyncApp, at: (f32, f32), dy: f32) -> anyhow::Result<()> {
    input(
        window,
        cx,
        PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
            position: point(px(at.0), px(at.1)),
            delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(dy))),
            modifiers: Default::default(),
            touch_phase: gpui::TouchPhase::Moved,
        }),
    )
}

/// A physical key press: the platform's key down then key up, which is
/// what activates a focused control (GPUI clicks on the key up).
async fn key_press(window: Window, cx: &mut AsyncApp, key: &str) -> anyhow::Result<()> {
    let keystroke = Keystroke::parse(key)?;
    input(
        window,
        cx,
        PlatformInput::KeyDown(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        }),
    )?;
    input(
        window,
        cx,
        PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }),
    )?;
    settle(cx, 150).await;
    draw(window, cx)?;
    Ok(())
}

async fn keys(window: Window, cx: &mut AsyncApp, combos: &[&str]) -> anyhow::Result<()> {
    for combo in combos {
        let keystroke = Keystroke::parse(combo)?;
        gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
            window.dispatch_keystroke(keystroke, cx);
        })?;
        draw(window, cx)?;
        settle(cx, 25).await;
    }
    settle(cx, 150).await;
    draw(window, cx)?;
    Ok(())
}

/// Type lowercase text into the focused field, one key at a time.
async fn type_text(window: Window, cx: &mut AsyncApp, text: &str) -> anyhow::Result<()> {
    let combos: Vec<String> = text
        .chars()
        .map(|ch| {
            if ch == ' ' {
                "space".into()
            } else {
                ch.to_string()
            }
        })
        .collect();
    let combos: Vec<&str> = combos.iter().map(String::as_str).collect();
    keys(window, cx, &combos).await
}

/// Make AppKit ask for the window's accessibility children, which turns on
/// GPUI's AccessKit tree for every later frame.
#[cfg(target_os = "macos")]
fn activate_accessibility(window: &gpui::Window) -> anyhow::Result<()> {
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow!("window handle unavailable: {error}"))?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        bail!("fixture window is not an AppKit window");
    };
    let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    // SAFETY: the view belongs to this live window on the main thread; each
    // returned pointer is checked before it is messaged.
    let ns_window: *mut AnyObject = unsafe { objc2::msg_send![view, window] };
    ensure!(!ns_window.is_null(), "fixture view has no NSWindow");
    let content: *mut AnyObject = unsafe { objc2::msg_send![ns_window, contentView] };
    ensure!(!content.is_null(), "fixture NSWindow has no content view");
    let _children: *mut AnyObject = unsafe { objc2::msg_send![content, accessibilityChildren] };
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn activate_accessibility(_: &gpui::Window) -> anyhow::Result<()> {
    bail!("the duplicate-id check needs the macOS accessibility tree")
}

/// Turn the accessibility tree on and confirm GPUI builds it.
fn arm_a11y(window: Window, cx: &mut AsyncApp) -> anyhow::Result<bool> {
    gpui::AnyWindowHandle::from(window)
        .update(cx, |_, window, _| activate_accessibility(window))??;
    let tree = gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window.draw(cx).clear();
        window.debug_a11y_tree_json()
    })?;
    Ok(tree.is_some())
}

fn surfaces(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    draw(window, cx)?;
    window.update(cx, |shell, window, cx| {
        let page = shell
            .fixture_board_page()
            .ok_or_else(|| anyhow!("the Board page is not mounted"))?;
        let surfaces = page.read(cx).surfaces().clone();
        Ok(surfaces.read(cx).fixture_readback(window, cx))
    })?
}

fn rect(readback: &Value, name: &str) -> Option<[f32; 4]> {
    let value = readback["probe"]["bounds"].get(name)?.as_array()?;
    Some([
        value.first()?.as_f64()? as f32,
        value.get(1)?.as_f64()? as f32,
        value.get(2)?.as_f64()? as f32,
        value.get(3)?.as_f64()? as f32,
    ])
}

fn text<'a>(readback: &'a Value, key: &str) -> &'a str {
    readback["probe"]["texts"][key].as_str().unwrap_or("")
}

fn chips_with(readback: &Value, prefix: &str) -> Vec<String> {
    readback["probe"]["chips"]
        .as_object()
        .map(|chips| {
            chips
                .keys()
                .filter(|id| id.starts_with(prefix))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// The label a probed pop-up button (a settings mode menu) showed.
fn chip_label(readback: &Value, id: &str) -> String {
    let chip = &readback["probe"]["chips"][id];
    chip["label"]
        .as_str()
        .or_else(|| chip[0].as_str())
        .unwrap_or("")
        .to_owned()
}

/// The chat composer's sticky defaults file, as saved on disk (a ticket's
/// picks must never change it).
fn composer_defaults_snapshot(env: &Env) -> Value {
    let path = env.boot.data_dir.join("composer-defaults.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
        Err(_) => Value::String("absent".into()),
    }
}

fn center(bounds: [f32; 4]) -> (f32, f32) {
    (bounds[0] + bounds[2] / 2.0, bounds[1] + bounds[3] / 2.0)
}

/// Controls that live in the detail drawer's pinned header.
const HEADER_CONTROLS: &[&str] = &[
    "detail-close",
    "detail-rename",
    "detail-title-input",
    "detail-title-save",
    "detail-title-cancel",
];

fn inside(inner: [f32; 4], outer: [f32; 4]) -> bool {
    inner[0] >= outer[0] - 0.5
        && inner[1] >= outer[1] - 0.5
        && inner[0] + inner[2] <= outer[0] + outer[2] + 0.5
        && inner[1] + inner[3] <= outer[1] + outer[3] + 0.5
}

/// Wheel-scroll the drawer until `name` is fully inside its visible area: a
/// control in the detail's pinned header is always visible; one in its body
/// must be inside the body's (non-scrolling) frame.
async fn reveal(window: Window, cx: &mut AsyncApp, name: &str) -> anyhow::Result<[f32; 4]> {
    let mut sign = 1.0_f32;
    let mut last: Option<f32> = None;
    for _ in 0..60 {
        let rb = surfaces(window, cx)?;
        let target = rect(&rb, name).ok_or_else(|| anyhow!("{name} was not painted"))?;
        let drawer = rect(&rb, "board-drawer").ok_or_else(|| anyhow!("no drawer painted"))?;
        // Only the pinned header's own controls are always visible. A body
        // control scrolled up under the header also paints inside the
        // header's rectangle, but is clipped: it must be scrolled back.
        if HEADER_CONTROLS.contains(&name)
            && rect(&rb, "detail-header").is_some_and(|header| inside(target, header))
        {
            return Ok(target);
        }
        let (top, bottom) = match rect(&rb, "detail-scroll") {
            Some(body) => (body[1] + 8.0, body[1] + body[3] - 8.0),
            None => (drawer[1] + 24.0, drawer[1] + drawer[3] - 24.0),
        };
        if target[1] >= top && target[1] + target[3] <= bottom {
            return Ok(target);
        }
        // Wanted content movement: positive moves content down.
        let want = if target[1] < top {
            (top - target[1]).min(260.0)
        } else {
            -(target[1] + target[3] - bottom).min(260.0)
        };
        if let Some(previous) = last
            && (target[1] - previous).abs() > 0.5
            && ((target[1] - previous) > 0.0) != (want > 0.0)
        {
            // The last wheel moved the content the other way: flip.
            sign = -sign;
        }
        last = Some(target[1]);
        let at = (drawer[0] + drawer[2] / 2.0, drawer[1] + drawer[3] / 2.0);
        hover(window, cx, at)?;
        wheel(window, cx, at, want * sign)?;
        settle(cx, 60).await;
    }
    bail!("could not scroll {name} into the drawer")
}

/// Press inside a text box's bottom padding, below any line of text: the
/// box (not the input element) receives the press.
async fn press_box_bottom(window: Window, cx: &mut AsyncApp, name: &str) -> anyhow::Result<()> {
    let bounds = reveal(window, cx, name).await?;
    // The brief is the chat composer: press its text area's last line, far
    // right (past the text, clear of the attach button and the pickers).
    let rb = surfaces(window, cx)?;
    let input = &state_of(&rb)["descriptionComposer"]["input"];
    if name == "manual-description" && !input.is_null() {
        let n = |key: &str| input[key].as_f64().unwrap_or(0.0) as f32;
        return click_at(window, cx, (n("x") + n("w") - 6.0, n("y") + n("h") - 4.0)).await;
    }
    click_at(window, cx, (bounds[0] + 40.0, bounds[1] + bounds[3] - 4.0)).await
}

async fn click(window: Window, cx: &mut AsyncApp, name: &str) -> anyhow::Result<()> {
    let bounds = reveal(window, cx, name).await?;
    click_at(window, cx, center(bounds)).await
}

/// Poll the overlay until `done` holds.
async fn wait_for(
    window: Window,
    cx: &mut AsyncApp,
    what: &str,
    done: impl Fn(&Value) -> bool,
) -> anyhow::Result<Value> {
    let mut last = Value::Null;
    for _ in 0..120 {
        last = surfaces(window, cx)?;
        if done(&last) {
            return Ok(last);
        }
        settle(cx, 100).await;
    }
    bail!("timed out waiting for {what}: {}", one_line(&last))
}

fn one_line(value: &Value) -> String {
    let text = value.to_string();
    text.chars().take(3000).collect()
}

// ---------------------------------------------------------------------------
// Engine access (the same RPC the UI uses)
// ---------------------------------------------------------------------------

struct Env {
    reader: Arc<luna_rpc::RpcClient>,
    workspace: luna_engine::WorkspaceHost,
    store: Arc<luna_sync::DocsStore>,
    state: Entity<state::AppState>,
    device: String,
    project_path: String,
    boot: EngineBootConfig,
}

/// A hidden window with a production Shell on the shared state.
fn open_shell(
    cx: &mut gpui::App,
    state: Entity<state::AppState>,
    boot: EngineBootConfig,
    width: f32,
) -> anyhow::Result<Window> {
    cx.open_window(
        WindowOptions {
            window_background: theme::Theme::of(cx).window_background_appearance(),
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(20.0), px(20.0)),
                size(px(width), px(HEIGHT)),
            ))),
            show: false,
            focus: false,
            ..Default::default()
        },
        |window, cx| {
            window.set_rem_size(px(typography::font_size(cx).pixels()));
            cx.new(|cx| shell::Shell::new(state, boot, cx))
        },
    )
    .map_err(|error| anyhow!("opening a window: {error:#}"))
}

async fn rpc<T: serde::de::DeserializeOwned>(
    env: &Env,
    method: &str,
    params: Value,
) -> anyhow::Result<T> {
    let value = env
        .reader
        .call(method, params)
        .await
        .map_err(|error| anyhow!("{method}: {error}"))?;
    serde_json::from_value(value).with_context(|| format!("{method}: unexpected reply"))
}

async fn read_ticket(env: &Env, id: &str) -> anyhow::Result<BoardItemDetail> {
    rpc(
        env,
        methods::BOARD_READ,
        json!({ "workItemId": id, "historyLimit": 50 }),
    )
    .await
}

async fn project_tickets(env: &Env) -> anyhow::Result<BoardListPage> {
    rpc(
        env,
        methods::BOARD_LIST,
        json!({ "scope": { "kind": "project", "projectId": PROJECT }, "archived": "include" }),
    )
    .await
}

/// Archive a chat through the ordinary task-command path, as the app's
/// archive action does: read a fresh task snapshot immediately before the
/// command, fence it with that revision (and binding generation), and retry
/// once with a re-read snapshot and a new command id when another update
/// moved the revision in between.
async fn archive_chat(env: &Env, task_id: &str) -> anyhow::Result<luna_proto::CommandReceipt> {
    let mut receipt = None;
    for attempt in 0..2 {
        let read: luna_proto::TaskCommandRead = rpc(
            env,
            methods::GET_TASK_COMMAND_SNAPSHOT,
            json!({ "taskId": task_id }),
        )
        .await?;
        let mut params = json!({
            "commandId": format!("fixture-archive-{task_id}-{attempt}"),
            "actorId": env.device,
            "clientId": env.device,
            "clientSessionId": "board-detail-fixture",
            "taskId": task_id,
            "intent": { "kind": "setArchived", "archived": true },
            "expectedTaskRevision": read.snapshot.task_revision,
        });
        if let Some(generation) = read.snapshot.binding_generation {
            params["expectedBindingGeneration"] = generation.into();
        }
        let answer: luna_proto::CommandReceipt =
            rpc(env, methods::EXECUTE_TASK_COMMAND, params).await?;
        let stale = answer.status == luna_proto::CommandReceiptStatus::Conflict
            && answer.reason.as_ref().map(|reason| reason.code)
                == Some(luna_proto::CommandReasonCode::StaleTaskRevision);
        receipt = Some(answer);
        if !stale {
            break;
        }
    }
    receipt.ok_or_else(|| anyhow!("no archive receipt"))
}

fn chat_count(env: &Env) -> anyhow::Result<usize> {
    Ok(env.workspace.read_chats()?.len())
}

fn permission_key(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "ask",
        PermissionMode::Auto => "auto",
        PermissionMode::Full => "full",
    }
}

fn permission_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "Limited access",
        PermissionMode::Auto => "Approve for me",
        PermissionMode::Full => "Full access",
    }
}

fn reasoning_label(level: ReasoningLevel) -> &'static str {
    match level {
        ReasoningLevel::Minimal => "Minimal",
        ReasoningLevel::Low => "Low",
        ReasoningLevel::Medium => "Medium",
        ReasoningLevel::High => "High",
        ReasoningLevel::XHigh => "X-High",
        ReasoningLevel::Max => "Max",
        ReasoningLevel::Ultra => "Ultra",
        ReasoningLevel::Ultracode => "Ultracode",
        ReasoningLevel::Ultrathink => "Ultrathink",
    }
}

fn wire<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The catalog choice the manual-form scenario drives: a harness with a
/// model `a` supporting a level `level` that model `b` does not support.
struct Choice {
    harness: HarnessId,
    harness_label: String,
    a: Model,
    b: Model,
    level: ReasoningLevel,
    permission: PermissionMode,
    accounts: usize,
}

fn pick_choice(options: &BoardExecutionOptions) -> Option<Choice> {
    let mut harnesses: Vec<_> = options
        .harnesses
        .iter()
        .filter(|entry| entry.available)
        .collect();
    // Prefer the deterministic peer; any qualifying harness works.
    harnesses.sort_by_key(|entry| entry.harness != HarnessId::Codex);
    for entry in harnesses {
        for a in &entry.models {
            for b in &entry.models {
                if a.id == b.id {
                    continue;
                }
                let Some(level) = a
                    .reasoning_levels
                    .iter()
                    .find(|level| !b.reasoning_levels.contains(level))
                else {
                    continue;
                };
                let permission = entry
                    .permission_modes
                    .iter()
                    .copied()
                    .find(|mode| *mode == PermissionMode::Auto)
                    .or_else(|| entry.permission_modes.first().copied())?;
                return Some(Choice {
                    harness: entry.harness,
                    harness_label: match entry.harness {
                        HarnessId::Codex => "Codex".into(),
                        HarnessId::ClaudeCode => "Claude Code".into(),
                        HarnessId::Opencode => "OpenCode".into(),
                        HarnessId::Mock => "Mock".into(),
                        other => format!("{other:?}"),
                    },
                    a: a.clone(),
                    b: b.clone(),
                    level: *level,
                    permission,
                    accounts: entry.accounts.len(),
                });
            }
        }
    }
    None
}

fn review_falls_back(options: &BoardExecutionOptions) -> bool {
    match &options.effective_review {
        None => true,
        Some(review) => review
            .sources
            .iter()
            .any(|source| source.source == BoardSettingOrigin::ImplementationFallback),
    }
}

// ---------------------------------------------------------------------------
// Engine records for the evidence ticket
// ---------------------------------------------------------------------------

fn sha(text: &str) -> String {
    format!("sha256:{}", hex(&Sha256::digest(text.as_bytes())))
}

/// Seed what the lifecycle and review slices write for a finished ticket:
/// Done, a working chat, an archived reviewer, three dispatches, outcomes,
/// two review rounds, review/repair counters, a source issue and a blocker.
/// Written through the store's one atomic commit primitive.
fn seed_evidence(env: &Env, item: &WorkItem, blocker: &str, choice: &Choice) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp_millis();
    let minute = 60_000;
    let engine = |component: &str| BoardActor::Engine {
        component: component.into(),
    };
    let user = BoardActor::User {
        device_id: env.device.clone(),
    };
    let resolved = |model: &Model, level: Option<ReasoningLevel>| ResolvedExecution {
        harness: choice.harness,
        provider_account_id: None,
        model: Some(model.id.clone()),
        reasoning: level,
        model_options: Default::default(),
        permission_mode: choice.permission,
        host_device_id: env.device.clone(),
        project_id: item.project_id.clone(),
        cwd: env.project_path.clone(),
        checkout_id: None,
        branch: None,
        sources: vec![BoardSettingSource {
            field: "model".into(),
            source: BoardSettingOrigin::Ticket,
        }],
    };
    let mut done = item.clone();
    done.column = BoardColumn::Done;
    done.revision += 1;
    done.updated_at_ms = now;
    done.updated_by = engine("review");
    let launch = |index: usize, kind: BoardInstructionKind, generation: u64, brief: u32| {
        let text = format!(
            "LUNA board {} for {} (generation {generation}, brief revision {brief}): {}",
            match kind {
                BoardInstructionKind::FirstStart => "first start",
                _ => "continuation",
            },
            item.key,
            item.description
        );
        LaunchSnapshot {
            id: format!("ls-fixture-{index}"),
            work_item_id: item.id.clone(),
            launch_request_id: format!("lr-fixture-{index}"),
            execution_generation: generation,
            intent_revision: generation,
            brief_revision: brief,
            kind,
            sent_title: item.title.clone(),
            sent_description: item.description.clone(),
            instruction_hash: sha(&text),
            instruction_text: text,
            attachments: Vec::new(),
            input_items: Vec::new(),
            context_refs: vec![],
            issue_context: None,
            review_base: None,
            requested: item.implementation.clone(),
            resolved: resolved(&choice.b, None),
            destination: BoardDestination::NewMain,
            context_mode: BoardContextMode::Fresh,
            task_id: EV_MAIN.into(),
            main_parent_id: None,
            command_id: format!("cmd-fixture-{index}"),
            message_id: format!("msg-fixture-{index}"),
            command_receipt: None,
            follow_up: BoardFollowUp::None,
            result_recipient_task_id: None,
            created_by: user.clone(),
            created_at_ms: now - (60 - index as i64 * 10) * minute,
        }
    };
    let outcome = |id: &str, kind: OutcomeKind, label: &str| OutcomeReference {
        id: id.into(),
        work_item_id: item.id.clone(),
        kind,
        execution_generation: 3,
        task_id: Some(EV_MAIN.into()),
        label: label.into(),
        basis: Some("diff:9f2c4e1ab37d".into()),
        url: None,
        path: None,
        state: None,
        detail: Value::Null,
        historical: false,
        observed_by: engine("review"),
        observed_at_ms: now - 5 * minute,
    };
    let review_profile = ReviewProfileResolution {
        requested: Default::default(),
        resolved: resolved(&choice.a, Some(choice.level)),
        observed_model: Some(choice.a.id.clone()),
        observed_harness: Some(choice.harness),
        implementation_fallback: true,
    };
    let basis = ReviewBasis {
        packet_version: 1,
        brief_revision: 2,
        checkout_id: None,
        cwd: env.project_path.clone(),
        base_revision: Some("a1b2c3d".into()),
        head_revision: Some("d4e5f6a".into()),
        diff_hash: "diff:9f2c4e1ab37d".into(),
        uncommitted_manifest_hash: None,
        packet_hash: "pk-fixture".into(),
        check_outcome_ids: vec!["oc-check".into()],
        omissions: vec![],
        captured_at_ms: now - 20 * minute,
    };
    let run = |round: u8,
               verdict: ReviewVerdict,
               findings: Vec<ReviewFinding>,
               cleanup: ReviewCleanupState,
               outcome: ReviewRunOutcome| ReviewRun {
        id: format!("rr-fixture-{round}"),
        work_item_id: item.id.clone(),
        version: 0,
        execution_generation: 3,
        round,
        primary_task_id: EV_MAIN.into(),
        reviewer_task_id: Some(EV_REVIEW.into()),
        display_main_parent_id: EV_MAIN.into(),
        placement: ReviewerPlacement::UnderTicket,
        launch_request_id: format!("rl-fixture-{round}"),
        profile: review_profile.clone(),
        basis: Some(basis.clone()),
        state: ReviewRunState::Completed,
        result: Some(ReviewResult {
            verdict,
            basis_packet_hash: "pk-fixture".into(),
            findings,
            summary: match verdict {
                ReviewVerdict::Passed => "The retry stops after three attempts.".into(),
                _ => "One blocking issue.".into(),
            },
            observed_model: Some(choice.a.id.clone()),
            submitted_at_ms: now - (20 - round as i64 * 10) * minute,
        }),
        delivered_id: Some(format!("dl-fixture-{round}")),
        stop_request_id: None,
        cleanup: ReviewCleanup {
            state: cleanup,
            cleanup_id: format!("cl-fixture-{round}"),
            attempts: 0,
            archived_at_ms: None,
            detail: None,
        },
        created_at_ms: now - (25 - round as i64 * 10) * minute,
        updated_at_ms: now - (20 - round as i64 * 10) * minute,
        // A5's optional review-run fields (merge compile fix).
        detail: None,
        attention: None,
        requested_by: None,
        result_request_id: None,
        late_result: false,
        outcome: Some(outcome),
        implementer_summary: None,
        evidence: Vec::new(),
        reported_check_ids: Vec::new(),
        supersedes: None,
        relaunches: 0,
    };
    let rows = vec![
        luna_sync::BoardRowWrite::TaskLink(Box::new(WorkItemTaskLink {
            id: "wl-fixture-working".into(),
            work_item_id: item.id.clone(),
            task_id: EV_MAIN.into(),
            role: TaskLinkRole::Working,
            active: true,
            chat_role: Some(BoardChatRole::Main),
            original_container: Some(OriginalContainer::MainChat),
            linked_by: engine("lifecycle"),
            linked_at_ms: now - 60 * minute,
            unlinked_at_ms: None,
            source_message_id: None,
            context_refs: vec![],
            launch_snapshot_id: Some("ls-fixture-1".into()),
            result_subscriptions: vec![],
        })),
        luna_sync::BoardRowWrite::TaskLink(Box::new(WorkItemTaskLink {
            id: "wl-fixture-reviewer".into(),
            work_item_id: item.id.clone(),
            task_id: EV_REVIEW.into(),
            role: TaskLinkRole::Reviewer,
            active: true,
            chat_role: Some(BoardChatRole::Side),
            original_container: None,
            linked_by: engine("review"),
            linked_at_ms: now - 25 * minute,
            unlinked_at_ms: None,
            source_message_id: None,
            context_refs: vec![],
            launch_snapshot_id: None,
            result_subscriptions: vec![],
        })),
        luna_sync::BoardRowWrite::Launch(Box::new(launch(
            1,
            BoardInstructionKind::FirstStart,
            1,
            1,
        ))),
        luna_sync::BoardRowWrite::Launch(Box::new(launch(
            2,
            BoardInstructionKind::Continuation,
            2,
            1,
        ))),
        luna_sync::BoardRowWrite::Launch(Box::new(launch(
            3,
            BoardInstructionKind::Continuation,
            3,
            2,
        ))),
        luna_sync::BoardRowWrite::Outcome(Box::new(OutcomeReference {
            detail: json!({ "filesChanged": 3, "additions": 42, "deletions": 7 }),
            ..outcome("oc-diff", OutcomeKind::Diff, "Working tree diff")
        })),
        luna_sync::BoardRowWrite::Outcome(Box::new(OutcomeReference {
            url: Some("https://github.com/example/luna/pull/12".into()),
            state: Some("open".into()),
            observed_by: engine("source-control"),
            ..outcome("oc-pr", OutcomeKind::PullRequest, "Retry failed uploads")
        })),
        luna_sync::BoardRowWrite::Outcome(Box::new(OutcomeReference {
            state: Some("passed".into()),
            observed_by: BoardActor::Agent {
                task_id: EV_MAIN.into(),
                role: Some(BoardChatRole::Main),
            },
            ..outcome("oc-check", OutcomeKind::Check, "cargo test")
        })),
        luna_sync::BoardRowWrite::Outcome(Box::new(OutcomeReference {
            path: Some("target/retry-report.json".into()),
            observed_by: BoardActor::Reviewer {
                task_id: EV_REVIEW.into(),
                review_run_id: "rr-fixture-1".into(),
            },
            ..outcome("oc-artifact", OutcomeKind::Artifact, "retry-report.json")
        })),
        luna_sync::BoardRowWrite::Outcome(Box::new(OutcomeReference {
            state: Some("failed".into()),
            historical: true,
            execution_generation: 2,
            ..outcome("oc-check-old", OutcomeKind::Check, "cargo test")
        })),
        luna_sync::BoardRowWrite::ReviewRun {
            expected_version: None,
            run: Box::new(run(
                0,
                ReviewVerdict::ChangesRequested,
                vec![ReviewFinding {
                    id: "f-blocker".into(),
                    severity: ReviewSeverity::Blocker,
                    blocking: true,
                    file: Some("src/upload.rs".into()),
                    line: Some(42),
                    explanation: "Retries never stop after three failures.".into(),
                    required_action: "Stop after the third attempt.".into(),
                    evidence: vec![],
                    repeats_finding_id: None,
                }],
                ReviewCleanupState::NotStarted,
                ReviewRunOutcome::RepairRequested {
                    launch_snapshot_id: "ls-fixture-3".into(),
                    repair_round: 1,
                },
            )),
        },
        luna_sync::BoardRowWrite::ReviewRun {
            expected_version: None,
            run: Box::new(run(
                1,
                ReviewVerdict::Passed,
                vec![ReviewFinding {
                    id: "f-note".into(),
                    severity: ReviewSeverity::Note,
                    blocking: false,
                    file: Some("src/upload.rs".into()),
                    line: Some(57),
                    explanation: "Consider logging the retry count.".into(),
                    required_action: "Optional.".into(),
                    evidence: vec![],
                    repeats_finding_id: None,
                }],
                // This pass completed the ticket: cleanup is due, and the
                // engine's own cleanup reads the reviewer's archived state
                // and closed views back before reporting Archived/Closed.
                ReviewCleanupState::Pending,
                ReviewRunOutcome::Completed {
                    execution_generation: 3,
                },
            )),
        },
        luna_sync::BoardRowWrite::Intent {
            expected_version: None,
            intent: Box::new(BoardExecutionIntent {
                desired: BoardColumn::Ready,
                intent_revision: 3,
                execution_generation: 3,
                last_request_id: "fixture-seed".into(),
                working_task_id: Some(EV_MAIN.into()),
                first_prompt: FirstPromptDelivery::Delivered {
                    command_id: "cmd-fixture-1".into(),
                    message_id: "msg-fixture-1".into(),
                    at_ms: now - 59 * minute,
                },
                launch_snapshot_id: Some("ls-fixture-3".into()),
                review_passes: 2,
                repair_rounds: 1,
                observed_phase: BoardPhase::Complete,
                ..BoardExecutionIntent::initial(&item.id, now)
            }),
        },
        luna_sync::BoardRowWrite::IssueReference(Box::new(IssueReference {
            id: "ir-fixture-7".into(),
            work_item_id: item.id.clone(),
            project_id: item.project_id.clone(),
            provider: IssueProvider::GitHub,
            repository: IssueRepository {
                host: "github.com".into(),
                owner: "example".into(),
                name: "luna".into(),
                api_id: None,
            },
            number: 7,
            issue_api_id: None,
            canonical_url: "https://github.com/example/luna/issues/7".into(),
            account: Some("fixture".into()),
            role: IssueLinkRole::Source,
            remote_state: Some("open".into()),
            remote_title: Some("Uploads fail silently".into()),
            remote_updated_at: None,
            fetched_at_ms: now - 30 * minute,
            active: true,
            linked_by: user.clone(),
            linked_at_ms: now - 61 * minute,
            // A7's optional provenance fields (merge compile fix).
            account_id: None,
            import_snapshot: None,
            import_request_hash: None,
            source_changed_at_ms: None,
            source_id: None,
        })),
        luna_sync::BoardRowWrite::Dependency(Box::new(BoardDependency {
            work_item_id: item.id.clone(),
            blocked_by_id: blocker.into(),
            source: DependencySource::Manual,
            created_by: user,
            created_at_ms: now - 61 * minute,
        })),
    ];
    env.store
        .board_commit(
            luna_sync::BoardCommit {
                request_id: "fixture-seed-evidence".into(),
                payload_hash: "fixture".into(),
                committed_at_ms: now,
                item: luna_sync::BoardItemWrite::Replace {
                    expected_revision: item.revision,
                    item: Box::new(done),
                    placement: Some(luna_sync::BoardRankPlacement::Top),
                },
                rows,
                event: Some((
                    WorkItemEventKind::Completed {
                        execution_generation: 3,
                    },
                    true,
                )),
                receipt: None,
            },
            None,
        )
        .map_err(|error| anyhow!("seeding the evidence ticket: {error:?}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Entry points the Board page (A8) calls
// ---------------------------------------------------------------------------

fn open_new_ticket(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    window.update(cx, |shell, window, cx| {
        let page = shell
            .fixture_board_page()
            .ok_or_else(|| anyhow!("the Board page is not mounted"))?;
        page.update(cx, |page, cx| {
            page.open_new_ticket(Some(PROJECT.into()), window, cx)
        });
        anyhow::Ok(())
    })?
}

fn open_detail(window: Window, cx: &mut AsyncApp, id: &str) -> anyhow::Result<()> {
    let id = id.to_owned();
    window.update(cx, |shell, window, cx| {
        let page = shell
            .fixture_board_page()
            .ok_or_else(|| anyhow!("the Board page is not mounted"))?;
        page.update(cx, |page, cx| page.open_detail(id, window, cx));
        anyhow::Ok(())
    })?
}

/// Whether the Board page shows A6's Discuss with AI conversation.
fn authoring_mounted(window: Window, cx: &mut AsyncApp) -> anyhow::Result<bool> {
    draw(window, cx)?;
    window.update(cx, |shell, _, cx| {
        shell
            .fixture_board_page()
            .is_some_and(|page| page.read(cx).authoring_pane().is_some())
    })
}

fn route(window: Window, cx: &mut AsyncApp) -> anyhow::Result<String> {
    window.update(cx, |shell, _, _| shell.fixture_board_detail_route())
}

async fn wait_engine<T>(
    what: &str,
    cx: &mut AsyncApp,
    mut probe: impl AsyncFnMut() -> anyhow::Result<Option<T>>,
) -> anyhow::Result<T> {
    for _ in 0..150 {
        if let Some(value) = probe().await? {
            return Ok(value);
        }
        settle(cx, 100).await;
    }
    bail!("timed out waiting for {what}")
}

fn state_of(rb: &Value) -> &Value {
    &rb["state"]
}

/// The manual form's settings editor.
fn manual_settings(rb: &Value) -> &Value {
    &state_of(rb)["settings"]
}

/// The detail drawer's settings editor (edit mode).
fn detail_settings(rb: &Value) -> &Value {
    &state_of(rb)["edits"]["settings"]["settings"]
}

/// One profile's chat pickers (`implementation`, `review`): draft and UI.
fn pickers_of<'a>(settings: &'a Value, profile: &str) -> &'a Value {
    &settings["pickers"][profile]
}

fn chip_center(pickers: &Value, chip: &str) -> Option<(f32, f32)> {
    let rect = pickers["ui"]["chips"][chip].as_array()?;
    let at = |i: usize| rect.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32;
    (at(2) > 0.0).then(|| (at(0) + at(2) / 2.0, at(1) + at(3) / 2.0))
}

type SettingsOf = fn(&Value) -> &Value;

/// Open a profile's picker chip (`model`, `access`, `checkout`) with a real
/// click and wait for its menu.
async fn open_picker(
    window: Window,
    cx: &mut AsyncApp,
    settings: SettingsOf,
    profile: &str,
    chip: &str,
    kind: &str,
) -> anyhow::Result<Value> {
    let rb = surfaces(window, cx)?;
    let at = chip_center(pickers_of(settings(&rb), profile), chip)
        .ok_or_else(|| anyhow!("the {profile} {chip} picker was not painted"))?;
    click_at(window, cx, at).await?;
    wait_for(window, cx, &format!("the {profile} {kind} menu"), |rb| {
        pickers_of(settings(rb), profile)["ui"]["open"] == kind
    })
    .await
}

/// Move a picker's keyboard cursor (`active` or `settingActive`) to
/// `target` with Down/Up, reading it back after each key.
async fn cursor_to(
    window: Window,
    cx: &mut AsyncApp,
    settings: SettingsOf,
    profile: &str,
    field: &str,
    target: u64,
) -> anyhow::Result<()> {
    for _ in 0..60 {
        let rb = surfaces(window, cx)?;
        let at = pickers_of(settings(&rb), profile)["ui"][field]
            .as_u64()
            .unwrap_or(0);
        if at == target {
            return Ok(());
        }
        keys(window, cx, &[if at < target { "down" } else { "up" }]).await?;
    }
    bail!("the {profile} picker's {field} never reached {target}")
}

/// Pick a model the way a person does: open the model pill, type its name,
/// put the cursor on its row (its harness), Enter. The menu stays open.
async fn pick_model(
    window: Window,
    cx: &mut AsyncApp,
    settings: SettingsOf,
    profile: &str,
    harness: &str,
    model: &Model,
) -> anyhow::Result<()> {
    let open = surfaces(window, cx)?;
    if pickers_of(settings(&open), profile)["ui"]["open"] != "HarnessModel" {
        open_picker(window, cx, settings, profile, "model", "HarnessModel").await?;
    }
    // The search covers the viewed harness: its rail tab first.
    let rb = surfaces(window, cx)?;
    let tab = chip_center(
        pickers_of(settings(&rb), profile),
        &format!("rail-{harness}"),
    )
    .ok_or_else(|| anyhow!("the {profile} picker shows no {harness} rail tab"))?;
    click_at(window, cx, tab).await?;
    wait_for(window, cx, &format!("the {harness} rail"), |rb| {
        pickers_of(settings(rb), profile)["draft"]["harness"] == harness
    })
    .await?;
    keys(window, cx, &["cmd-a", "backspace"]).await?;
    type_text(window, cx, &model.label.to_lowercase()).await?;
    let rb = wait_for(window, cx, &format!("{} in the search", model.id), |rb| {
        pickers_of(settings(rb), profile)["ui"]["modelRows"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["id"] == model.id.as_str() && row["harness"] == harness)
            })
    })
    .await?;
    let index = pickers_of(settings(&rb), profile)["ui"]["modelRows"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .position(|row| row["id"] == model.id.as_str() && row["harness"] == harness)
        })
        .unwrap_or(0) as u64;
    cursor_to(window, cx, settings, profile, "active", index).await?;
    keys(window, cx, &["enter"]).await?;
    wait_for(window, cx, &format!("{} picked", model.id), |rb| {
        pickers_of(settings(rb), profile)["draft"]["model"] == model.id.as_str()
    })
    .await?;
    Ok(())
}

/// The reasoning tray's levels for the picked model (wire names).
fn tray_levels(pickers: &Value) -> Vec<String> {
    pickers["ui"]["settingGroups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| group["choices"].as_array().cloned().unwrap_or_default())
        .filter_map(|choice| choice["reasoning"].as_str().map(str::to_owned))
        .collect()
}

/// Pick a reasoning level in the open model menu's tray: cursor past the
/// model rows to the reasoning row, Right, cursor to the level, Enter.
async fn pick_reasoning(
    window: Window,
    cx: &mut AsyncApp,
    settings: SettingsOf,
    profile: &str,
    level: &str,
) -> anyhow::Result<()> {
    let rb = surfaces(window, cx)?;
    let ui = &pickers_of(settings(&rb), profile)["ui"];
    let rows = ui["modelRows"].as_array().map_or(0, Vec::len) as u64;
    let groups = ui["settingGroups"].as_array().cloned().unwrap_or_default();
    let (group, choice) = groups
        .iter()
        .enumerate()
        .find_map(|(g, group)| {
            group["choices"]
                .as_array()?
                .iter()
                .position(|choice| choice["reasoning"] == level)
                .map(|c| (g as u64, c as u64))
        })
        .ok_or_else(|| anyhow!("the {profile} tray offers no {level}: {groups:?}"))?;
    cursor_to(window, cx, settings, profile, "active", rows + group).await?;
    keys(window, cx, &["right"]).await?;
    wait_for(window, cx, "the reasoning tray", |rb| {
        pickers_of(settings(rb), profile)["ui"]["settingMenu"].is_string()
    })
    .await?;
    cursor_to(window, cx, settings, profile, "settingActive", choice).await?;
    keys(window, cx, &["enter"]).await?;
    wait_for(window, cx, &format!("{level} reasoning"), |rb| {
        pickers_of(settings(rb), profile)["draft"]["reasoning"] == level
    })
    .await?;
    Ok(())
}

/// Close whatever picker menu is open (Escape, as often as it takes).
async fn close_pickers(
    window: Window,
    cx: &mut AsyncApp,
    settings: SettingsOf,
    profile: &str,
) -> anyhow::Result<()> {
    for _ in 0..4 {
        let rb = surfaces(window, cx)?;
        if pickers_of(settings(&rb), profile)["ui"]["open"].is_null() {
            return Ok(());
        }
        keys(window, cx, &["escape"]).await?;
    }
    bail!("the {profile} picker menu did not close")
}

fn detail_of(rb: &Value) -> &Value {
    &rb["state"]["detail"]
}

// ---------------------------------------------------------------------------
// The scenario
// ---------------------------------------------------------------------------

async fn scenario(
    run: &mut Run,
    window: Window,
    cx: &mut AsyncApp,
    env: &Env,
) -> anyhow::Result<()> {
    cx.update(|cx| motion::set_reduced_motion(cx, true));
    settle(cx, 800).await;
    for _ in 0..50 {
        if route(window, cx)? == "page:board"
            && window.update(cx, |shell, _, _| shell.fixture_board_page().is_some())?
        {
            break;
        }
        draw(window, cx)?;
        settle(cx, 100).await;
    }
    run.check(
        "board-route-restored",
        json!("page:board"),
        json!(route(window, cx)?),
        route(window, cx)? == "page:board",
    )?;
    // The accessibility tree is built every frame from here on, so a debug
    // build panics on any duplicate element-derived node id (the panic hook
    // records it and exits nonzero).
    let armed = arm_a11y(window, cx)?;
    run.check(
        "a11y-duplicate-id-check-armed",
        json!({ "accessKitTree": true, "debugAssertions": true }),
        json!({ "accessKitTree": armed, "debugAssertions": cfg!(debug_assertions) }),
        armed && cfg!(debug_assertions),
    )?;
    let chats_before = chat_count(env)?;
    let options: BoardExecutionOptions = rpc(
        env,
        methods::BOARD_EXECUTION_OPTIONS,
        json!({ "projectId": PROJECT }),
    )
    .await?;
    let choice = pick_choice(&options).ok_or_else(|| {
        anyhow!(
            "the catalog has no available harness with two models of different reasoning \
             support: {}",
            serde_json::to_string(&options).unwrap_or_default()
        )
    })?;
    run.check(
        "catalog-from-engine",
        json!("an available harness with two models of different reasoning support"),
        json!({
            "harness": wire(choice.harness),
            "a": choice.a.id,
            "b": choice.b.id,
            "level": wire(choice.level),
            "permission": permission_key(choice.permission),
        }),
        true,
    )?;

    // 1. The chooser offers two distinct experiences; Discuss with AI has
    //    no title/description inputs.
    open_new_ticket(window, cx)?;
    let rb = wait_for(window, cx, "the chooser", |rb| {
        rb["surface"] == "chooser" && rect(rb, "board-chooser-ai").is_some()
    })
    .await?;
    let manual = rect(&rb, "board-chooser-manual");
    let ai = rect(&rb, "board-chooser-ai");
    run.check(
        "chooser-two-experiences",
        json!("Write manually and Discuss with AI are separate controls"),
        json!({ "manual": manual, "ai": ai }),
        manual.is_some() && ai.is_some() && manual != ai,
    )?;
    run.capture(window, cx, "d01-chooser")?;
    click(window, cx, "board-chooser-ai").await?;
    // Discuss with AI is A6's conversation, which the Board page mounts in
    // place of its body (the drawer closes): no title/description inputs.
    let mut mounted = false;
    for _ in 0..120 {
        mounted = authoring_mounted(window, cx)?;
        if mounted {
            break;
        }
        settle(cx, 100).await;
    }
    let rb = surfaces(window, cx)?;
    let inputs: Vec<&str> = ["manual-title", "manual-description"]
        .into_iter()
        .filter(|name| rect(&rb, name).is_some())
        .collect();
    run.check(
        "discuss-with-ai-has-no-form",
        json!({ "authoringMounted": true, "titleOrDescriptionInputs": [] }),
        json!({ "authoringMounted": mounted, "surface": rb["surface"], "titleOrDescriptionInputs": inputs }),
        mounted && rb["surface"].is_null() && inputs.is_empty(),
    )?;
    run.keep("discuss-with-ai", &rb);
    run.capture(window, cx, "d02-discuss-with-ai")?;
    // Close the conversation (as its Close button does) and return to the
    // board.
    window.update(cx, |shell, _, cx| {
        if let Some(pane) = shell
            .fixture_board_page()
            .and_then(|page| page.read(cx).authoring_pane())
        {
            pane.update(cx, |pane, cx| pane.close(cx));
        }
    })?;
    for _ in 0..120 {
        if !authoring_mounted(window, cx)? {
            break;
        }
        settle(cx, 100).await;
    }
    // A pointer press on the scrim closes the overlay.
    open_new_ticket(window, cx)?;
    let rb = wait_for(window, cx, "the chooser again", |rb| {
        rb["surface"] == "chooser" && rect(rb, "board-overlay").is_some()
    })
    .await?;
    let overlay = rect(&rb, "board-overlay").ok_or_else(|| anyhow!("no overlay painted"))?;
    click_at(
        window,
        cx,
        (overlay[0] + 12.0, overlay[1] + overlay[3] / 2.0),
    )
    .await?;
    let rb = surfaces(window, cx)?;
    run.check(
        "scrim-closes-overlay",
        Value::Null,
        rb["surface"].clone(),
        rb["surface"].is_null(),
    )?;

    // 2. Write manually.
    open_new_ticket(window, cx)?;
    wait_for(window, cx, "the chooser", |rb| {
        rb["surface"] == "chooser" && rect(rb, "board-chooser-manual").is_some()
    })
    .await?;
    click(window, cx, "board-chooser-manual").await?;
    let rb = wait_for(window, cx, "the manual form", |rb| {
        rb["surface"] == "manual" && rect(rb, "manual-title").is_some()
    })
    .await?;
    run.check(
        "manual-form-focuses-title",
        json!("title"),
        state_of(&rb)["focused"].clone(),
        state_of(&rb)["focused"] == "title",
    )?;
    click(window, cx, "manual-save").await?;
    let rb = wait_for(window, cx, "the empty-title error", |rb| {
        state_of(rb)["errors"]["title"].is_string()
    })
    .await?;
    let tickets = project_tickets(env).await?.items.len();
    run.check(
        "empty-title-rejected-in-form",
        json!({ "titleError": "shown", "tickets": 0, "chats": chats_before }),
        json!({
            "titleError": text(&rb, "manual-error-title"),
            "tickets": tickets,
            "chats": chat_count(env)?,
        }),
        !text(&rb, "manual-error-title").is_empty()
            && tickets == 0
            && chat_count(env)? == chats_before,
    )?;
    let rb = wait_for(window, cx, "the host's choices", |rb| {
        manual_settings(rb)["options"].is_object()
            && chip_center(pickers_of(manual_settings(rb), "implementation"), "model").is_some()
    })
    .await?;
    let composer_defaults = composer_defaults_snapshot(env);
    // The agent row is the chat's pickers (model, access, checkout); the
    // reviewer's has the model only. Both start on the project's settings.
    let settings = manual_settings(&rb);
    let implementation = pickers_of(settings, "implementation");
    let review = pickers_of(settings, "review");
    let effective_model = options
        .effective_implementation
        .as_ref()
        .and_then(|resolved| {
            let model = resolved.model.as_deref()?;
            options
                .harnesses
                .iter()
                .find(|entry| entry.harness == resolved.harness)
                .and_then(|entry| entry.models.iter().find(|m| m.id == model))
                .map(|m| m.label.clone())
        })
        .unwrap_or_default();
    run.check(
        "settings-are-the-chat-pickers",
        json!({
            "agent": ["model", "access", "checkout"],
            "reviewer": ["model"],
            "modes": ["Project defaults", "Project default"],
            "modelPill": effective_model,
        }),
        json!({
            "agent": implementation["ui"]["chips"],
            "reviewer": review["ui"]["chips"],
            "modes": [
                chip_label(&rb, "manual-impl-mode"),
                chip_label(&rb, "manual-review-mode"),
            ],
            "modelPill": implementation["ui"]["modelChip"],
            "agentCaption": text(&rb, "manual-impl-caption"),
        }),
        ["model", "access", "checkout"]
            .iter()
            .all(|chip| chip_center(implementation, chip).is_some())
            && chip_center(review, "model").is_some()
            && chip_center(review, "access").is_none()
            && chip_label(&rb, "manual-impl-mode") == "Project defaults"
            && chip_label(&rb, "manual-review-mode") == "Project default"
            && settings["draft"]["implementation"] == json!({})
            && implementation["ui"]["modelChip"] == effective_model.as_str()
            && text(&rb, "manual-impl-caption").starts_with("Inherits the project's settings"),
    )?;
    // The model menu lists only the host's catalog.
    let rb = open_picker(
        window,
        cx,
        manual_settings,
        "implementation",
        "model",
        "HarnessModel",
    )
    .await?;
    let rows = pickers_of(manual_settings(&rb), "implementation")["ui"]["modelRows"].clone();
    let invented: Vec<Value> = rows
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            !options.harnesses.iter().any(|entry| {
                wire(entry.harness) == row["harness"].as_str().unwrap_or_default()
                    && entry
                        .models
                        .iter()
                        .any(|model| model.id == row["id"].as_str().unwrap_or_default())
            })
        })
        .cloned()
        .collect();
    run.check(
        "model-choices-from-catalog",
        json!({ "invented": [] }),
        json!({ "rows": rows, "invented": invented }),
        invented.is_empty() && rows.as_array().is_some_and(|rows| !rows.is_empty()),
    )?;
    // Model a by search + Enter; its reasoning tray offers a's levels only.
    pick_model(
        window,
        cx,
        manual_settings,
        "implementation",
        &wire(choice.harness),
        &choice.a,
    )
    .await?;
    let rb = surfaces(window, cx)?;
    let mut shown = tray_levels(pickers_of(manual_settings(&rb), "implementation"));
    shown.sort();
    shown.dedup();
    let mut expected: Vec<String> = choice.a.reasoning_levels.iter().map(|l| wire(*l)).collect();
    expected.sort();
    expected.dedup();
    run.check(
        "reasoning-only-supported",
        json!(expected),
        json!(shown),
        shown == expected,
    )?;
    pick_reasoning(
        window,
        cx,
        manual_settings,
        "implementation",
        &wire(choice.level),
    )
    .await?;
    close_pickers(window, cx, manual_settings, "implementation").await?;
    // Access through the chat's access menu (Approve for me, Full access).
    let permission_row = match choice.permission {
        PermissionMode::Full => Some(1),
        PermissionMode::Auto => Some(0),
        _ => None,
    };
    if let Some(row) = permission_row {
        open_picker(
            window,
            cx,
            manual_settings,
            "implementation",
            "access",
            "Permission",
        )
        .await?;
        cursor_to(window, cx, manual_settings, "implementation", "active", row).await?;
        keys(window, cx, &["enter"]).await?;
    }
    let rb = surfaces(window, cx)?;
    let draft = manual_settings(&rb)["draft"]["implementation"].clone();
    run.check(
        "selected-real-settings",
        json!({
            "harness": wire(choice.harness),
            "model": choice.a.id,
            "reasoning": wire(choice.level),
            "permissionMode": permission_row.map(|_| permission_key(choice.permission)),
            "mode": "Custom",
        }),
        json!({ "draft": draft, "mode": chip_label(&rb, "manual-impl-mode") }),
        draft["harness"] == wire(choice.harness)
            && draft["model"] == choice.a.id
            && draft["reasoning"] == wire(choice.level)
            && (permission_row.is_none()
                || draft["permissionMode"] == permission_key(choice.permission))
            && chip_label(&rb, "manual-impl-mode") == "Custom",
    )?;
    run.capture(window, cx, "d03-manual-settings")?;
    // Model b does not support the level: the draft drops it and says so.
    pick_model(
        window,
        cx,
        manual_settings,
        "implementation",
        &wire(choice.harness),
        &choice.b,
    )
    .await?;
    let rb = surfaces(window, cx)?;
    let draft = manual_settings(&rb)["draft"]["implementation"].clone();
    let notices = manual_settings(&rb)["notices"].clone();
    let mut shown = tray_levels(pickers_of(manual_settings(&rb), "implementation"));
    shown.sort();
    shown.dedup();
    let mut expected: Vec<String> = choice.b.reasoning_levels.iter().map(|l| wire(*l)).collect();
    expected.sort();
    expected.dedup();
    let told = notices.as_array().is_some_and(|notices| {
        notices.iter().any(|notice| {
            notice
                .as_str()
                .is_some_and(|notice| notice.contains(reasoning_label(choice.level)))
        })
    });
    run.check(
        "model-change-resets-incompatible-reasoning",
        json!({ "reasoning": null, "notice": "names the reset level", "tray": expected }),
        json!({ "reasoning": draft["reasoning"], "notices": notices, "tray": shown }),
        draft["model"] == choice.b.id && draft["reasoning"].is_null() && told && shown == expected,
    )?;
    let saved_level = choice.b.reasoning_levels.first().copied();
    if let Some(level) = saved_level {
        pick_reasoning(window, cx, manual_settings, "implementation", &wire(level)).await?;
    }
    close_pickers(window, cx, manual_settings, "implementation").await?;
    let rb = surfaces(window, cx)?;
    // The reviewer (none configured) follows the implementation's picks.
    let reviewer = &pickers_of(manual_settings(&rb), "review")["ui"];
    let expected_suffix = saved_level.map(reasoning_label);
    run.check(
        "reviewer-fallback-when-none-configured",
        json!({
            "caption": REVIEW_FALLBACK,
            "pill": [choice.b.label, expected_suffix],
        }),
        json!({
            "shown": text(&rb, "manual-review-caption"),
            "catalogFallsBack": review_falls_back(&options),
            "pill": [reviewer["modelChip"], reviewer["modelChipSuffix"]],
        }),
        text(&rb, "manual-review-caption") == REVIEW_FALLBACK
            && reviewer["modelChip"] == choice.b.label.as_str()
            && expected_suffix.is_none_or(|suffix| reviewer["modelChipSuffix"] == suffix),
    )?;
    run.check(
        "ticket-picks-leave-chat-defaults",
        json!(composer_defaults),
        json!(composer_defaults_snapshot(env)),
        composer_defaults_snapshot(env) == composer_defaults,
    )?;
    // Title and brief by keyboard; Enter in the title moves to the brief.
    let rb = surfaces(window, cx)?;
    let waiting = state_of(&rb)["generateTitle"].clone();
    click(window, cx, "manual-title").await?;
    type_text(window, cx, "retry failed uploads").await?;
    keys(window, cx, &["enter"]).await?;
    let rb = surfaces(window, cx)?;
    run.check(
        "enter-moves-to-description",
        json!("description"),
        state_of(&rb)["focused"].clone(),
        state_of(&rb)["focused"] == "description",
    )?;
    type_text(window, cx, "uploads that fail are retried three times").await?;
    keys(window, cx, &["enter"]).await?;
    type_text(window, cx, "done when the retry test passes").await?;
    // A press in the empty lower part of the multi-line box (below its
    // text, in the box's padding) focuses the description; typing lands.
    click(window, cx, "manual-title").await?;
    press_box_bottom(window, cx, "manual-description").await?;
    let rb = surfaces(window, cx)?;
    let focused_low = state_of(&rb)["focused"].clone();
    type_text(window, cx, " fast").await?;
    // The acceptance criteria section is part of the same box.
    click(window, cx, "manual-title").await?;
    click(window, cx, "manual-add-criteria").await?;
    click(window, cx, "manual-title").await?;
    press_box_bottom(window, cx, "manual-description").await?;
    let rb = surfaces(window, cx)?;
    let focused_criteria = state_of(&rb)["focused"].clone();
    type_text(window, cx, "the retry test passes").await?;
    let rb = surfaces(window, cx)?;
    let description = state_of(&rb)["description"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let expected_description = "uploads that fail are retried three times\ndone when the retry \
                                test passes fast\n\n## Acceptance criteria\n- the retry test passes";
    run.check(
        "description-box-lower-press-focuses",
        json!({ "focused": ["description", "description"], "description": expected_description }),
        json!({
            "focused": [focused_low, focused_criteria],
            "description": description,
        }),
        focused_low == "description"
            && focused_criteria == "description"
            && description == expected_description,
    )?;
    run.check(
        "typed-brief",
        json!({ "title": "retry failed uploads", "description": expected_description }),
        json!({ "title": state_of(&rb)["title"], "description": description }),
        state_of(&rb)["title"] == "retry failed uploads" && description == expected_description,
    )?;
    // 19. With the brief written, Generate title drafts the title with the
    //     title model; nothing is saved.
    let tickets_before = project_tickets(env).await?.items.len();
    let rb = surfaces(window, cx)?;
    let ready = state_of(&rb)["generateTitle"].clone();
    click(window, cx, "manual-generate-title").await?;
    let rb = wait_for(window, cx, "the generated title", |rb| {
        state_of(rb)["title"] == GENERATED_TITLE && state_of(rb)["generateTitle"] == "ready"
    })
    .await?;
    run.check(
        "generate-title-from-the-brief",
        json!({ "before": "waiting", "ready": "ready", "title": GENERATED_TITLE, "saved": 0 }),
        json!({
            "before": waiting,
            "ready": ready,
            "title": state_of(&rb)["title"],
            "saved": project_tickets(env).await?.items.len() - tickets_before,
        }),
        waiting == "waiting"
            && ready == "ready"
            && state_of(&rb)["title"] == GENERATED_TITLE
            && project_tickets(env).await?.items.len() == tickets_before,
    )?;
    // 16. The brief is the chat composer in form mode: the attach control,
    //     the agent's pickers in its controls row, no Send of its own; `@`
    //     offers the project's files and Enter accepts one.
    let rb = surfaces(window, cx)?;
    let brief = state_of(&rb)["descriptionComposer"].clone();
    run.check(
        "brief-is-the-chat-composer",
        json!({ "form": true, "send": null, "chips": ["access", "checkout", "model"] }),
        json!({ "form": brief["form"], "send": brief["send"], "chips": brief["pickers"]["chips"] }),
        brief["form"] == true
            && brief["send"].is_null()
            && ["model", "access", "checkout"]
                .iter()
                .all(|chip| !brief["pickers"]["chips"][*chip].is_null()),
    )?;
    click(window, cx, "manual-title").await?;
    press_box_bottom(window, cx, "manual-description").await?;
    type_text(window, cx, "\n@uplo").await.ok();
    let rb = wait_for(window, cx, "the @ file completion", |rb| {
        state_of(rb)["descriptionComposer"]["mention"]["results"]
            .as_array()
            .is_some_and(|paths| paths.iter().any(|path| path == "src/upload.rs"))
    })
    .await?;
    let offered = state_of(&rb)["descriptionComposer"]["mention"]["results"].clone();
    // Put the cursor on the file, then Enter accepts it (no line break).
    let index = offered
        .as_array()
        .and_then(|paths| paths.iter().position(|path| path == "src/upload.rs"))
        .unwrap_or(0);
    for _ in 0..index {
        keys(window, cx, &["down"]).await?;
    }
    keys(window, cx, &["enter"]).await?;
    let rb = wait_for(window, cx, "the mentioned file in the brief", |rb| {
        state_of(rb)["description"]
            .as_str()
            .is_some_and(|text| text.contains("src/upload.rs"))
            && state_of(rb)["descriptionComposer"]["mention"]["query"].is_null()
    })
    .await?;
    let with_mention = state_of(&rb)["description"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    // 17. An image dropped on the brief is staged in its composer.
    let screenshot = run.dir.join("brief-screenshot.png");
    let mut image = image::RgbaImage::new(24, 16);
    for (x, _, pixel) in image.enumerate_pixels_mut() {
        *pixel = image::Rgba([200, 60 + (x as u8) * 6, 40, 255]);
    }
    image.save(&screenshot)?;
    let target = rect(&surfaces(window, cx)?, "manual-description")
        .ok_or_else(|| anyhow!("the brief was not painted"))?;
    let at = point(px(target[0] + target[2] / 2.0), px(target[1] + 30.0));
    for event in [
        gpui::FileDropEvent::Entered {
            position: at,
            paths: gpui::ExternalPaths(vec![screenshot.clone()].into()),
        },
        gpui::FileDropEvent::Pending { position: at },
        gpui::FileDropEvent::Submit { position: at },
    ] {
        input(window, cx, PlatformInput::FileDrop(event))?;
        draw(window, cx)?;
        settle(cx, 80).await;
    }
    let rb = wait_for(window, cx, "the staged brief image", |rb| {
        state_of(rb)["descriptionComposer"]["attachments"]
            .as_array()
            .is_some_and(|names| names.len() == 1)
    })
    .await?;
    run.check(
        "brief-takes-files-and-images",
        json!({ "offered": "src/upload.rs", "mention": "in the brief", "staged": 1 }),
        json!({
            "offered": offered,
            "brief": with_mention,
            "staged": state_of(&rb)["descriptionComposer"]["attachments"],
        }),
        with_mention.contains("src/upload.rs")
            && state_of(&rb)["descriptionComposer"]["attachments"]
                .as_array()
                .is_some_and(|names| names.len() == 1),
    )?;
    // A double click saves once.
    let save = reveal(window, cx, "manual-save").await?;
    let at = center(save);
    hover(window, cx, at)?;
    draw(window, cx)?;
    press(window, cx, at, true)?;
    press(window, cx, at, false)?;
    press(window, cx, at, true)?;
    press(window, cx, at, false)?;
    let rb = wait_for(window, cx, "the receipt", |rb| {
        state_of(rb)["phase"] == "saved"
    })
    .await?;
    run.keep("receipt", &rb);
    let receipt: BoardMutationReceipt = serde_json::from_value(state_of(&rb)["receipt"].clone())?;
    let page = project_tickets(env).await?;
    run.check(
        "save-to-backlog-once-no-chat",
        json!({ "tickets": 1, "chats": chats_before, "column": "backlog" }),
        json!({ "tickets": page.items.len(), "chats": chat_count(env)?, "column": receipt.column }),
        page.items.len() == 1
            && chat_count(env)? == chats_before
            && receipt.column == BoardColumn::Backlog,
    )?;
    let implementation = text(&rb, "manual-receipt-implementation").to_owned();
    let mut wanted = vec![
        choice.harness_label.clone(),
        choice.b.label.clone(),
        permission_label(choice.permission).to_owned(),
    ];
    if let Some(level) = saved_level {
        wanted.push(reasoning_label(level).to_owned());
    }
    run.check(
        "receipt-shows-key-no-chat-and-settings",
        json!({ "key": receipt.key, "chat": "No chat was created", "implementation": wanted }),
        json!({
            "title": text(&rb, "manual-receipt-title"),
            "chat": text(&rb, "manual-receipt-chat"),
            "implementation": implementation,
            "review": text(&rb, "manual-receipt-review"),
        }),
        text(&rb, "manual-receipt-title").contains(&receipt.key)
            && text(&rb, "manual-receipt-chat").contains("No chat was created")
            && wanted
                .iter()
                .all(|part| implementation.contains(part.as_str()))
            && text(&rb, "manual-receipt-review").contains("implementation model"),
    )?;
    let saved = read_ticket(env, &receipt.work_item_id).await?;
    let profile = &saved.view.item.implementation;
    run.check(
        "engine-saved-the-selected-settings",
        json!({
            "harness": wire(choice.harness),
            "model": choice.b.id,
            "reasoning": saved_level.map(wire),
            "permissionMode": permission_key(choice.permission),
            "launches": 0,
            "workingChat": null,
        }),
        json!({
            "implementation": profile,
            "review": saved.view.item.review,
            "launches": saved.launches.len(),
            "workingChat": saved.view.working_chat,
        }),
        profile.harness == Some(choice.harness)
            && profile.model.as_deref() == Some(choice.b.id.as_str())
            && profile.reasoning == saved_level
            && profile.permission_mode == Some(choice.permission)
            && saved.view.item.review == Default::default()
            && saved.launches.is_empty()
            && saved.view.working_chat.is_none(),
    )?;
    let item = &saved.view.item;
    run.check(
        "engine-saved-the-brief-image-and-mention",
        json!({ "attachments": 1, "underUploads": true, "mention": true, "composerCleared": true }),
        json!({
            "attachments": item.attachments,
            "description": item.description,
        }),
        item.attachments.len() == 1
            && item.attachments[0].contains("uploads")
            && item.description.contains("src/upload.rs"),
    )?;
    let saved_attachments = item.attachments.clone();
    run.capture(window, cx, "d04-receipt")?;
    let manual_id = receipt.work_item_id.clone();
    let manual_key = receipt.key.clone();
    // The receipt takes the keyboard the saved fields held.
    let rb = wait_for(window, cx, "the receipt's keyboard focus", |rb| {
        state_of(rb)["focused"] == "receipt"
    })
    .await
    .unwrap_or_else(|_| rb.clone());
    run.check(
        "receipt-takes-the-keyboard",
        json!("receipt"),
        state_of(&rb)["focused"].clone(),
        state_of(&rb)["focused"] == "receipt",
    )?;

    // 3. The drawer shows the saved settings and nothing unrecorded. Enter
    // on the focused Open ticket opens it (a keyboard press, no click).
    key_press(window, cx, "enter").await?;
    let rb = wait_for(window, cx, "the ticket's drawer", |rb| {
        rb["surface"] == "detail"
            && state_of(rb)["loaded"] == "ready"
            && state_of(rb)["optionsLoaded"] == true
    })
    .await?;
    let requested = text(&rb, "detail-settings-requested").to_owned();
    run.check(
        "detail-shows-saved-settings",
        json!({ "requested": wanted, "resolved": "Not resolved yet…", "observed": "Not observed…" }),
        json!({
            "requested": requested,
            "resolved": text(&rb, "detail-settings-resolved"),
            "observed": text(&rb, "detail-settings-observed"),
        }),
        wanted.iter().all(|part| requested.contains(part.as_str()))
            && text(&rb, "detail-settings-resolved").starts_with("Not resolved yet")
            && text(&rb, "detail-settings-observed").starts_with("Not observed"),
    )?;
    run.check(
        "send-update-not-offered-before-start",
        json!(false),
        detail_of(&rb)["sendUpdateOffered"].clone(),
        detail_of(&rb)["sendUpdateOffered"] == false && rect(&rb, "detail-send-update").is_none(),
    )?;
    run.capture(window, cx, "d05-detail")?;

    // 3b. Changing settings re-renders every catalog row: switching harness
    //     Claude → Codex → OpenCode and back in both profiles, with each
    //     harness's option-bearing model selected, never paints two controls
    //     with one element id (the a11y tree rejects duplicates).
    click(window, cx, "detail-settings-edit").await?;
    wait_for(window, cx, "the settings editor", |rb| {
        detail_settings(rb)["options"].is_object()
            && chip_center(pickers_of(detail_settings(rb), "review"), "model").is_some()
    })
    .await?;
    // The reviewer becomes the ticket's own through its mode menu.
    click(window, cx, "detail-review-mode").await?;
    click(window, cx, "detail-review-mode-1").await?;
    wait_for(window, cx, "a custom reviewer", |rb| {
        chip_label(rb, "detail-review-mode") == "Custom reviewer"
    })
    .await?;
    let catalog_model = |harness: HarnessId, id: &str| {
        options
            .harnesses
            .iter()
            .find(|entry| entry.harness == harness)
            .and_then(|entry| entry.models.iter().find(|model| model.id == id))
            .cloned()
    };
    let mut visits = Vec::new();
    for (key, harness) in [
        ("claude-code", HarnessId::ClaudeCode),
        ("codex", HarnessId::Codex),
        ("opencode", HarnessId::Opencode),
        ("claude-code", HarnessId::ClaudeCode),
        ("codex", HarnessId::Codex),
    ] {
        let wanted = model_with_options(key);
        let model = catalog_model(harness, wanted)
            .ok_or_else(|| anyhow!("{wanted} is not in the {key} catalog"))?;
        for profile in ["implementation", "review"] {
            pick_model(window, cx, detail_settings, profile, key, &model).await?;
            wait_for(
                window,
                cx,
                &format!("{key} in the {profile} profile"),
                |rb| {
                    detail_settings(rb)["draft"][profile]["harness"] == key
                        && detail_settings(rb)["draft"][profile]["model"] == wanted
                },
            )
            .await?;
            close_pickers(window, cx, detail_settings, profile).await?;
            // Two full frames with the tree built.
            draw(window, cx)?;
            draw(window, cx)?;
        }
        let rb = surfaces(window, cx)?;
        visits.push(json!({
            "harness": key,
            "implementation": pickers_of(detail_settings(&rb), "implementation")["ui"]["modelChip"],
            "review": pickers_of(detail_settings(&rb), "review")["ui"]["modelChip"],
        }));
    }
    let rb = surfaces(window, cx)?;
    run.keep("harness-switch", &rb);
    let still_armed = arm_a11y(window, cx)?;
    run.check(
        "harness-switch-without-duplicate-ids",
        json!({ "visits": 5, "a11yTree": true }),
        json!({ "visits": visits, "a11yTree": still_armed }),
        visits.len() == 5 && still_armed,
    )?;
    run.capture(window, cx, "d05b-harness-switch")?;
    click(window, cx, "detail-settings-cancel").await?;
    wait_for(window, cx, "the settings editor to close", |rb| {
        state_of(rb)["edits"]["settings"].is_null()
    })
    .await?;

    // 4. Rename through the engine; Enter submits.
    let before = read_ticket(env, &manual_id).await?.view.item.revision;
    click(window, cx, "detail-rename").await?;
    wait_for(window, cx, "the title editor", |rb| {
        state_of(rb)["edits"]["title"].is_object()
    })
    .await?;
    keys(window, cx, &["cmd-a"]).await?;
    type_text(window, cx, "retry failed uploads safely").await?;
    keys(window, cx, &["enter"]).await?;
    wait_for(window, cx, "the rename", |rb| {
        state_of(rb)["edits"]["title"].is_null()
    })
    .await?;
    let renamed = read_ticket(env, &manual_id).await?.view.item;
    run.check(
        "rename-through-the-engine",
        json!({ "title": "retry failed uploads safely", "revision": format!("> {before}") }),
        json!({ "title": renamed.title, "revision": renamed.revision }),
        renamed.title == "retry failed uploads safely" && renamed.revision > before,
    )?;

    // 5. A stale brief edit surfaces the conflict and keeps my text.
    click(window, cx, "detail-edit-brief").await?;
    wait_for(window, cx, "the brief editor", |rb| {
        state_of(rb)["edits"]["brief"].is_object()
    })
    .await?;
    // Move the keyboard away (a press on the drawer header's status line),
    // then press the empty lower part of the brief box: it focuses the
    // brief and the typed text lands at its end.
    let rb = surfaces(window, cx)?;
    let drawer = rect(&rb, "board-drawer").ok_or_else(|| anyhow!("no drawer painted"))?;
    let close = rect(&rb, "detail-close").ok_or_else(|| anyhow!("no Close painted"))?;
    click_at(window, cx, (drawer[0] + 30.0, close[1] + close[3] / 2.0)).await?;
    let blurred = surfaces(window, cx)?;
    press_box_bottom(window, cx, "detail-brief-input").await?;
    let rb = surfaces(window, cx)?;
    run.check(
        "brief-box-lower-press-focuses",
        json!({ "before": false, "after": true }),
        json!({
            "before": state_of(&blurred)["edits"]["brief"]["focused"],
            "after": state_of(&rb)["edits"]["brief"]["focused"],
        }),
        state_of(&blurred)["edits"]["brief"]["focused"] == false
            && state_of(&rb)["edits"]["brief"]["focused"] == true,
    )?;
    keys(window, cx, &["cmd-down"]).await?;
    type_text(window, cx, " plus logging").await?;
    let current = read_ticket(env, &manual_id).await?.view.item;
    let concurrent: BoardMutationReceipt = rpc(
        env,
        methods::BOARD_UPDATE,
        json!({
            "requestId": "fixture-concurrent-edit",
            "workItemId": manual_id,
            "expectedRevision": current.revision,
            "changes": { "description": "changed in another window" },
        }),
    )
    .await?;
    click(window, cx, "detail-brief-save").await?;
    let rb = wait_for(window, cx, "the conflict", |rb| {
        state_of(rb)["edits"]["brief"]["conflictRevision"].is_u64()
    })
    .await?;
    let engine_text = read_ticket(env, &manual_id).await?.view.item.description;
    let mine = state_of(&rb)["edits"]["brief"]["text"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    run.check(
        "stale-brief-edit-conflicts",
        json!({
            "conflictRevision": concurrent.revision,
            "engine": "changed in another window",
            "mineKept": "… plus logging",
        }),
        json!({
            "conflictRevision": state_of(&rb)["edits"]["brief"]["conflictRevision"],
            "engine": engine_text,
            "mine": mine,
            "banner": text(&rb, "detail-conflict-brief"),
        }),
        state_of(&rb)["edits"]["brief"]["conflictRevision"] == concurrent.revision
            && engine_text == "changed in another window"
            && mine.ends_with(" plus logging")
            && text(&rb, "detail-conflict-brief").contains("changed since you started editing"),
    )?;
    run.capture(window, cx, "d06-conflict")?;
    click(window, cx, "detail-conflict-keep-brief").await?;
    wait_for(window, cx, "keep mine", |rb| {
        state_of(rb)["edits"]["brief"].is_null()
    })
    .await?;
    let kept = read_ticket(env, &manual_id).await?.view.item;
    run.check(
        "keep-mine-saves-over-the-current-revision",
        json!({ "description": mine, "revision": format!("> {}", concurrent.revision) }),
        json!({ "description": kept.description, "revision": kept.revision }),
        kept.description == mine.trim_end() && kept.revision > concurrent.revision,
    )?;

    // 6. Start is the engine move (as drag): one first start.
    click(window, cx, "detail-action-start").await?;
    let started = wait_engine("the first start", cx, || async {
        let detail = read_ticket(env, &manual_id).await?;
        Ok((detail.view.item.column == BoardColumn::Ready
            && detail
                .launches
                .iter()
                .any(|launch| launch.kind == BoardInstructionKind::FirstStart))
        .then_some(detail))
    })
    .await?;
    let rb = wait_for(window, cx, "the running ticket", |rb| {
        detail_of(rb)["dispatch"]["starts"] == 1 && detail_of(rb)["runActive"] == true
    })
    .await?;
    let firsts = started
        .launches
        .iter()
        .filter(|launch| launch.kind == BoardInstructionKind::FirstStart)
        .count();
    run.check(
        "start-dispatches-first-start-once",
        json!({ "engineFirstStarts": 1, "uiStarts": 1, "summary": "1 start · 0 continuations" }),
        json!({
            "engineFirstStarts": firsts,
            "uiStarts": detail_of(&rb)["dispatch"]["starts"],
            "summary": text(&rb, "detail-dispatch-summary"),
            "phase": detail_of(&rb)["phase"],
        }),
        firsts == 1
            && detail_of(&rb)["dispatch"]["starts"] == 1
            && text(&rb, "detail-dispatch-summary") == "1 start · 0 continuations",
    )?;
    let first = started
        .launches
        .iter()
        .find(|launch| launch.kind == BoardInstructionKind::FirstStart)
        .cloned();
    run.check(
        "first-start-delivers-the-brief-image",
        json!({ "attachments": saved_attachments, "trailer": true }),
        json!({
            "attachments": first.as_ref().map(|launch| launch.attachments.clone()),
            "trailer": first.as_ref().map(|launch| launch.instruction_text.contains("Attached images (local files")),
        }),
        first.as_ref().is_some_and(|launch| {
            launch.attachments == saved_attachments
                && saved_attachments
                    .iter()
                    .all(|path| launch.instruction_text.contains(path.as_str()))
                && launch
                    .instruction_text
                    .contains("Attached images (local files")
        }),
    )?;
    run.check(
        "send-update-not-offered-without-an-unsent-revision",
        json!(false),
        detail_of(&rb)["sendUpdateOffered"].clone(),
        detail_of(&rb)["sendUpdateOffered"] == false,
    )?;
    run.capture(window, cx, "d07-running")?;

    // 7. While the run is active, a saved edit stays out of the chat until
    //    Send update to chat.
    click(window, cx, "detail-edit-brief").await?;
    wait_for(window, cx, "the brief editor", |rb| {
        state_of(rb)["edits"]["brief"].is_object()
    })
    .await?;
    let rb = surfaces(window, cx)?;
    let note = text(&rb, "detail-brief-note").to_owned();
    keys(window, cx, &["cmd-down"]).await?;
    type_text(window, cx, " and a metric").await?;
    click(window, cx, "detail-brief-save").await?;
    let rb = wait_for(window, cx, "Send update offered", |rb| {
        state_of(rb)["edits"]["brief"].is_null() && detail_of(rb)["sendUpdateOffered"] == true
    })
    .await?;
    run.check(
        "send-update-offered-while-running",
        json!({ "offered": true, "runActive": true, "note": "A run is active…" }),
        json!({
            "offered": detail_of(&rb)["sendUpdateOffered"],
            "runActive": detail_of(&rb)["runActive"],
            "note": note,
        }),
        detail_of(&rb)["runActive"] == true
            && rect(&rb, "detail-send-update").is_some()
            && note.starts_with("A run is active"),
    )?;
    click(window, cx, "detail-send-update").await?;
    let updated = wait_engine("the brief update dispatch", cx, || async {
        let detail = read_ticket(env, &manual_id).await?;
        Ok(detail
            .launches
            .iter()
            .any(|launch| launch.kind == BoardInstructionKind::BriefUpdate)
            .then_some(detail))
    })
    .await?;
    let rb = wait_for(window, cx, "the update in dispatch history", |rb| {
        detail_of(rb)["dispatch"]["updates"] == 1
    })
    .await?;
    run.check(
        "send-update-dispatches-a-brief-update",
        json!({ "engineBriefUpdates": 1, "uiUpdates": 1, "notice": "Update sent…" }),
        json!({
            "engineBriefUpdates": updated.launches.iter().filter(|l| l.kind == BoardInstructionKind::BriefUpdate).count(),
            "uiUpdates": detail_of(&rb)["dispatch"]["updates"],
            "summary": text(&rb, "detail-dispatch-summary"),
            "notice": text(&rb, "detail-notice"),
        }),
        updated
            .launches
            .iter()
            .filter(|launch| launch.kind == BoardInstructionKind::BriefUpdate)
            .count()
            == 1
            && detail_of(&rb)["dispatch"]["updates"] == 1,
    )?;
    run.keep("running-ticket", &rb);
    run.capture(window, cx, "d08-update-sent")?;

    // 8. Evidence and review render from the engine's records.
    let created: BoardMutationReceipt = rpc(
        env,
        methods::BOARD_CREATE,
        json!({
            "requestId": "fixture-evidence-ticket",
            "projectId": PROJECT,
            "title": "retry failed uploads (finished)",
            "description": "Uploads that fail are retried three times.",
        }),
    )
    .await?;
    for (id, title) in [
        (EV_MAIN, "Retry failed uploads"),
        (EV_REVIEW, "Review · retry failed uploads"),
    ] {
        let _: Value = rpc(
            env,
            "Mutate",
            json!({
                "op": "createChat",
                "chatId": id,
                "spaceId": PROJECT,
                "cwd": env.project_path,
            }),
        )
        .await?;
        let _: Value = rpc(
            env,
            "Mutate",
            json!({ "op": "renameChat", "chatId": id, "title": title }),
        )
        .await?;
    }
    // The reviewer is archived the way the app archives a chat: the
    // ordinary setArchived task command fenced by a fresh task snapshot,
    // then an independent readback.
    let archive = archive_chat(env, EV_REVIEW).await?;
    let reviewer_archived = env
        .workspace
        .chat(EV_REVIEW)?
        .is_some_and(|chat| chat.archived);
    run.check(
        "reviewer-archived-through-the-engine",
        json!({ "status": "applied", "archived": true }),
        json!({
            "status": archive.status,
            "reason": archive.reason,
            "archived": reviewer_archived,
        }),
        archive.status == luna_proto::CommandReceiptStatus::Applied && reviewer_archived,
    )?;
    let chats = env.workspace.read_chats()?;
    cx.update(|cx| {
        env.state.update(cx, |state, cx| {
            state.chats = chats;
            cx.notify();
        })
    });
    let item = read_ticket(env, &created.work_item_id).await?.view.item;
    seed_evidence(env, &item, &manual_id, &choice)?;
    // The engine's cleanup (A5) runs on its own: wait until its readback
    // reports the reviewer archived (Archived) or archived with its views
    // closed (Closed). Nothing here writes that state.
    let cleaned = wait_engine("the reviewer cleanup", cx, || async {
        let detail = read_ticket(env, &created.work_item_id).await?;
        Ok(detail
            .review_runs
            .iter()
            .find(|run| run.id == "rr-fixture-1")
            .map(|run| run.cleanup.state)
            .filter(|state| {
                matches!(
                    state,
                    ReviewCleanupState::Archived | ReviewCleanupState::Closed
                )
            }))
    })
    .await?;
    open_detail(window, cx, &created.work_item_id)?;
    let rb = wait_for(window, cx, "the evidence ticket", |rb| {
        rb["surface"] == "detail"
            && state_of(rb)["optionsLoaded"] == true
            && detail_of(rb)["reviewRuns"] == 2
            && detail_of(rb)["outcomes"] == 5
            && !text(rb, "detail-review-repairs").is_empty()
            && text(rb, "detail-review-cleanup").starts_with("Cleanup: Archived")
    })
    .await?;
    run.keep("evidence-ticket", &rb);
    let outcome_texts: Vec<String> = (0..4)
        .map(|index| text(&rb, &format!("detail-outcome-{index}")).to_owned())
        .collect();
    let outcome_metas: Vec<String> = (0..4)
        .map(|index| text(&rb, &format!("detail-outcome-{index}-meta")).to_owned())
        .collect();
    let has = |needle: &str| outcome_texts.iter().any(|text| text.starts_with(needle));
    // Read the engine after the UI: the UI may lag Archived → Closed, never
    // run ahead of the engine.
    let engine_cleanup = read_ticket(env, &created.work_item_id)
        .await?
        .review_runs
        .iter()
        .find(|run| run.id == "rr-fixture-1")
        .map(|run| run.cleanup.state);
    let shown_cleanup = text(&rb, "detail-review-cleanup").to_owned();
    let cleanup_ok = match shown_cleanup.as_str() {
        CLEANUP_ARCHIVED => matches!(
            engine_cleanup,
            Some(ReviewCleanupState::Archived | ReviewCleanupState::Closed)
        ),
        CLEANUP_CLOSED => engine_cleanup == Some(ReviewCleanupState::Closed),
        _ => false,
    };
    run.check(
        "review-from-engine-records",
        json!({
            "repairs": "Repairs used: 1 of 2 · Review passes: 2",
            "phase": "Round 1 … Completed",
            "reviewer": choice.a.label,
            "finding": "Note · Not blocking · src/upload.rs:57",
            "earlier": "… Changes requested · 1 finding (1 blocking)",
            "cleanup": [CLEANUP_ARCHIVED, CLEANUP_CLOSED],
            "engineCleanupSettled": cleaned,
            "reviewDetails": true,
        }),
        json!({
            "repairs": text(&rb, "detail-review-repairs"),
            "phase": text(&rb, "detail-review-phase"),
            "reviewer": text(&rb, "detail-review-reviewer"),
            "finding": text(&rb, "detail-finding-0"),
            "earlier": text(&rb, "detail-review-earlier-0"),
            "cleanup": shown_cleanup,
            "engineCleanupNow": engine_cleanup,
            "reviewDetails": rect(&rb, "detail-review-details:rr-fixture-1").is_some(),
        }),
        text(&rb, "detail-review-repairs") == "Repairs used: 1 of 2 · Review passes: 2"
            && text(&rb, "detail-review-phase").starts_with("Round 1")
            && text(&rb, "detail-review-phase").ends_with("Completed")
            && text(&rb, "detail-review-reviewer").contains(&choice.a.label)
            && text(&rb, "detail-finding-0") == "Note · Not blocking · src/upload.rs:57"
            && text(&rb, "detail-review-earlier-0").contains("Changes requested")
            && text(&rb, "detail-review-earlier-0").contains("(1 blocking)")
            && cleanup_ok
            && rect(&rb, "detail-review-details:rr-fixture-1").is_some(),
    )?;
    run.check(
        "evidence-from-engine-records",
        json!({
            "done": "Done: the configured checks and review passed…",
            "outcomes": ["Diff · 3 files · +42 · −7", "Pull request · …", "Check · cargo test · Passed", "Artifact · retry-report.json"],
            "actor": "Recorded by …",
            "historical": "1 earlier result kept as history.",
        }),
        json!({
            "done": text(&rb, "detail-evidence-done"),
            "outcomes": outcome_texts,
            "metas": outcome_metas,
            "historical": text(&rb, "detail-evidence-historical"),
        }),
        text(&rb, "detail-evidence-done").starts_with("Done:")
            && has("Diff · 3 files · +42 · −7")
            && has("Pull request · Retry failed uploads · Open")
            && has("Check · cargo test · Passed")
            && has("Artifact · retry-report.json")
            && outcome_metas.iter().all(|meta| meta.starts_with("Recorded by "))
            && text(&rb, "detail-evidence-historical") == "1 earlier result kept as history.",
    )?;
    run.check(
        "dispatch-history-counts",
        json!("1 start · 2 continuations"),
        json!({
            "summary": text(&rb, "detail-dispatch-summary"),
            "first": text(&rb, "detail-dispatch-0"),
            "preview": text(&rb, "detail-dispatch-0-preview"),
            "meta": text(&rb, "detail-dispatch-0-meta"),
        }),
        text(&rb, "detail-dispatch-summary") == "1 start · 2 continuations"
            && text(&rb, "detail-dispatch-0-preview").starts_with("LUNA board")
            && text(&rb, "detail-dispatch-0-meta").contains("instruction hash sha256:"),
    )?;
    run.check(
        "source-issue-and-blocker",
        json!({ "issue": "example/luna#7 · source", "blocker": manual_key }),
        json!({
            "issue": text(&rb, "detail-issue-0"),
            "blocker": text(&rb, "detail-blocker-0"),
            "writeback": rect(&rb, "detail-issue-0-writeback").is_some(),
        }),
        text(&rb, "detail-issue-0").starts_with("example/luna#7 · source")
            && text(&rb, "detail-blocker-0").starts_with(&manual_key)
            && rect(&rb, "detail-issue-0-writeback").is_some(),
    )?;
    run.capture(window, cx, "d09-evidence-top")?;
    reveal(window, cx, "detail-review-details:rr-fixture-1").await?;
    run.capture(window, cx, "d10-review")?;
    reveal(window, cx, "detail-dispatch-toggle:ls-fixture-1").await?;
    run.capture(window, cx, "d11-evidence-dispatch")?;

    // 9. Review details opens the archived reviewer chat; it stays archived.
    click(window, cx, "detail-review-details:rr-fixture-1").await?;
    let mut opened = (String::new(), None);
    for _ in 0..50 {
        let selected = cx.update(|cx| env.state.read(cx).selected_chat.clone());
        opened = (route(window, cx)?, selected);
        if opened.0 == "chat" && opened.1.as_deref() == Some(EV_REVIEW) {
            break;
        }
        settle(cx, 100).await;
    }
    let archived = env
        .workspace
        .chat(EV_REVIEW)?
        .is_some_and(|chat| chat.archived);
    run.check(
        "review-details-opens-the-archived-reviewer",
        json!({ "route": "chat", "selected": EV_REVIEW, "archived": true }),
        json!({ "route": opened.0, "selected": opened.1, "archived": archived }),
        opened.0 == "chat" && opened.1.as_deref() == Some(EV_REVIEW) && archived,
    )?;
    run.capture(window, cx, "d12-review-details")?;

    // 10. Back on the Board the drawer is still open.
    window.update(cx, |shell, _, cx| shell.fixture_board_detail_open_board(cx))?;
    let rb = wait_for(window, cx, "the drawer again", |rb| {
        rb["surface"] == "detail"
    })
    .await?;
    run.check(
        "drawer-survives-go-to-chat-and-back",
        json!({ "surface": "detail", "workItemId": created.work_item_id }),
        json!({ "surface": rb["surface"], "workItemId": state_of(&rb)["workItemId"] }),
        state_of(&rb)["workItemId"] == created.work_item_id,
    )?;

    // 11. A narrow window (a second Shell on the same state): the drawer
    //     takes the page width, nothing paints outside it, Escape closes it.
    let state = env.state.clone();
    let boot = env.boot.clone();
    let narrow = cx.update(|cx| open_shell(cx, state, boot, NARROW))?;
    settle(cx, 600).await;
    arm_a11y(narrow, cx)?;
    narrow.update(cx, |shell, _, cx| shell.fixture_board_detail_open_board(cx))?;
    for _ in 0..50 {
        if route(narrow, cx)? == "page:board"
            && narrow.update(cx, |shell, _, _| shell.fixture_board_page().is_some())?
        {
            break;
        }
        draw(narrow, cx)?;
        settle(cx, 100).await;
    }
    open_detail(narrow, cx, &created.work_item_id)?;
    wait_for(narrow, cx, "the narrow drawer", |rb| {
        rb["surface"] == "detail" && state_of(rb)["loaded"] == "ready"
    })
    .await?;
    settle(cx, 300).await;
    let rb = surfaces(narrow, cx)?;
    run.keep("narrow", &rb);
    let overlay = rect(&rb, "board-overlay").ok_or_else(|| anyhow!("no overlay painted"))?;
    let drawer = rect(&rb, "board-drawer").ok_or_else(|| anyhow!("no drawer painted"))?;
    let outside: Vec<String> = rb["probe"]["bounds"]
        .as_object()
        .map(|bounds| {
            bounds
                .iter()
                .filter(|(name, _)| name.starts_with("detail-"))
                .filter_map(|(name, _)| rect(&rb, name).map(|r| (name.clone(), r)))
                // Only what is vertically inside the drawer is on screen.
                .filter(|(_, r)| r[1] >= drawer[1] && r[1] + r[3] <= drawer[1] + drawer[3])
                .filter(|(_, r)| {
                    r[0] < drawer[0] - 0.5 || r[0] + r[2] > drawer[0] + drawer[2] + 0.5
                })
                .map(|(name, _)| name)
                .collect()
        })
        .unwrap_or_default();
    run.check(
        "narrow-window-drawer-fits",
        json!({ "drawerFillsThePage": true, "controlsOutside": [] }),
        json!({ "overlay": overlay, "drawer": drawer, "controlsOutside": outside }),
        (drawer[0] - overlay[0]).abs() <= 1.0
            && (drawer[0] + drawer[2] - (overlay[0] + overlay[2])).abs() <= 1.0
            && outside.is_empty(),
    )?;
    run.capture(narrow, cx, "d13-narrow")?;
    // The header (key, status, title, Close) stays pinned while the body
    // scrolls: scroll the body far down, then Close is still in view.
    reveal(narrow, cx, "detail-dispatch-toggle:ls-fixture-1").await?;
    let rb = surfaces(narrow, cx)?;
    let header = rect(&rb, "detail-header");
    let close = rect(&rb, "detail-close");
    let body = rect(&rb, "detail-scroll");
    let scrolled = rect(&rb, "detail-body")
        .zip(body)
        .is_some_and(|(content, frame)| content[1] < frame[1] - 1.0);
    run.check(
        "close-stays-pinned-while-the-body-scrolls",
        json!({ "bodyScrolled": true, "closeInsideHeader": true, "headerAboveBody": true }),
        json!({ "header": header, "close": close, "body": body, "bodyScrolled": scrolled }),
        scrolled
            && header
                .zip(close)
                .is_some_and(|(header, close)| inside(close, header) && inside(close, drawer))
            && header
                .zip(body)
                .is_some_and(|(header, body)| header[1] + header[3] <= body[1] + 0.5),
    )?;
    run.capture(narrow, cx, "d14-narrow-scrolled")?;
    // Escape: a press on the header's status line (left of Close) focuses
    // the drawer, then Escape closes it.
    let close = close.ok_or_else(|| anyhow!("detail-close was not painted"))?;
    click_at(narrow, cx, (drawer[0] + 30.0, close[1] + close[3] / 2.0)).await?;
    keys(narrow, cx, &["escape"]).await?;
    let rb = surfaces(narrow, cx)?;
    run.check(
        "escape-closes-the-drawer",
        Value::Null,
        rb["surface"].clone(),
        rb["surface"].is_null(),
    )?;
    // The pinned Close, pressed without scrolling back up, closes it too.
    open_detail(narrow, cx, &created.work_item_id)?;
    wait_for(narrow, cx, "the drawer again", |rb| {
        rb["surface"] == "detail" && state_of(rb)["loaded"] == "ready"
    })
    .await?;
    reveal(narrow, cx, "detail-dispatch-toggle:ls-fixture-1").await?;
    let rb = surfaces(narrow, cx)?;
    let close = rect(&rb, "detail-close").ok_or_else(|| anyhow!("detail-close was not painted"))?;
    click_at(narrow, cx, center(close)).await?;
    let rb = surfaces(narrow, cx)?;
    run.check(
        "pinned-close-closes-the-drawer",
        Value::Null,
        rb["surface"].clone(),
        rb["surface"].is_null(),
    )?;
    let _ = gpui::AnyWindowHandle::from(narrow).update(cx, |_, window, _| window.remove_window());
    Ok(())
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

fn main() -> anyhow::Result<()> {
    // SAFETY: set first thing in `main`, before the Tokio runtime, the GPUI
    // platform or any other thread exists, and never changed afterwards; the
    // scripted provider only reads it.
    unsafe {
        std::env::set_var("LUNA_MOCK_DELAY_MS", MOCK_DELAY_MS);
    }
    let output = match std::env::args().nth(1) {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/board/detail"),
    };
    std::fs::create_dir_all(&output)?;
    let output = output.canonicalize()?;
    for stale in [
        "manifest.json",
        "readback.json",
        "board-detail.pass",
        "failure.txt",
        "failure.png",
    ] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    // A panic anywhere (a debug a11y duplicate-id assertion in a frame) is a
    // failed run: record it and exit nonzero instead of unwinding into GPUI.
    let panic_dir = output.clone();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = std::fs::write(panic_dir.join("failure.txt"), format!("panic: {info}\n"));
        default_hook(info);
        std::process::exit(1);
    }));
    let root = tempfile::tempdir()?;
    let ui_data = root.path().join("ui");
    let engine_data = root.path().join("engine");
    let project = root.path().join("project");
    let codex_home = root.path().join("providers/codex");
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(project.join("src"))?;
    std::fs::write(
        project.join("src/upload.rs"),
        "pub fn upload() -> Result<(), RetryError> {\n    todo!()\n}\n",
    )?;
    std::fs::create_dir_all(&codex_home)?;
    std::fs::write(
        codex_home.join("auth.json"),
        json!({ "OPENAI_API_KEY": "fixture-only" }).to_string(),
    )?;
    let runtime = tokio::runtime::Runtime::new()?;
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
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
    }));
    registry.register(Arc::new(claude_peer()));
    registry.register(Arc::new(codex_peer()));
    registry.register(Arc::new(opencode_peer()));
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(
                &engine_data,
                "board-detail-fixture",
                "board-detail-fixture",
            ),
            Arc::new(registry),
            HarnessId::Mock,
            None,
            AgentAccountsConfig {
                data_dir: engine_data.clone(),
                claude_config_dir: root.path().join("providers/claude"),
                claude_config_file: root.path().join("providers/claude.json"),
                codex_home: codex_home.clone(),
            },
        )
    })?;
    let mut auth = AuthConfig::new("", root.path().join("auth"));
    auth.dev_user_id = "board-detail-fixture".into();
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let device = core.device_id.clone();
    let project_path = project.to_string_lossy().to_string();
    core.workspace.create_space(
        PROJECT,
        &device,
        &project_path,
        Some(PROJECT_NAME.into()),
        true,
    )?;
    let reader = {
        let _entered = runtime.enter();
        Arc::new(luna_rpc::memory_client(core.rpc_service()))
    };
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
    let spaces = core.workspace.read_spaces()?;
    let chats = core.workspace.read_chats()?;
    let workspace = core.workspace.clone();
    let store = core.store.clone();
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 59_993,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let failed_ui = failed.clone();
    gpui_platform::headless().with_assets(icons::Assets).run(move |cx| {
        gpui_tokio::init(cx);
        gpui_base::init(cx);
        let mut settings = settings::UiSettings::default();
        settings.sidebar_auto_settle_after_days = None;
        // Restart restoration reopens the Board page.
        settings.last_sidebar_page = Some("board".into());
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
        app_menus::register_close_window(cx);
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
        let _ = state.update(cx, |state, cx| state.fixture_watch_sessions(cx));
        let window: Window = open_shell(cx, state.clone(), boot.clone(), WIDTH).unwrap();
        let _ = window.update(cx, |_, window, _| window.activate_window());
        let env = Env {
            reader,
            workspace,
            store,
            state: state.clone(),
            device: device.clone(),
            project_path,
            boot,
        };
        cx.spawn(async move |cx| {
            let mut run = Run::new(output.clone());
            let result = scenario(&mut run, window, cx, &env).await;
            match &result {
                Ok(()) => {
                    let written = run.write("pass", None).and_then(|_| {
                        std::fs::write(
                            output.join("board-detail.pass"),
                            format!(
                                "PASS: {} checks, {} captures. Headless production Shell against an \
                                 in-process engine with real pointer and keyboard input: the New \
                                 ticket chooser keeps Write manually and Discuss with AI apart (no \
                                 title/description in the AI pane); Write manually offers only the \
                                 engine catalog's harnesses, models, supported reasoning and access, \
                                 resets incompatible reasoning on a model change with a notice, says \
                                 the implementation model reviews when no reviewer is configured, \
                                 rejects an empty title in the form, saves to Backlog once (double \
                                 click) with zero chats and a receipt naming the saved settings; the \
                                 drawer shows the saved requested settings and nothing unrecorded, \
                                 renames through the engine, surfaces a stale-revision brief conflict \
                                 and saves mine only on request, starts with one first start, offers \
                                 Send update only while running with an unsent revision and records \
                                 the brief update; review, evidence, dispatch counts, source issue \
                                 and blocker render from engine records; Review details opens the \
                                 archived reviewer without unarchiving it; the drawer fits a narrow \
                                 window and Escape closes it\n",
                                run.steps.len(),
                                run.captures.len()
                            ),
                        )
                        .map_err(anyhow::Error::from)
                    });
                    if let Err(error) = written {
                        eprintln!("board-detail fixture could not write evidence: {error:#}");
                        failed_ui.store(true, std::sync::atomic::Ordering::SeqCst);
                        let _ = std::fs::write(
                            output.join("failure.txt"),
                            format!("could not write evidence: {error:#}\n"),
                        );
                        // The GPUI run loop can end the process on quit
                        // with status 0; a failed run must not.
                        std::process::exit(1);
                    }
                }
                Err(error) => {
                    eprintln!("board-detail fixture failed: {error:#}");
                    let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                    let _ = capture(window, cx, &output.join("failure.png"));
                    if let Ok(rb) = surfaces(window, cx) {
                        run.keep("failure", &rb);
                    }
                    let _ = run.write("fail", Some(format!("{error:#}")));
                    failed_ui.store(true, std::sync::atomic::Ordering::SeqCst);
                    // The GPUI run loop can end the process on quit with
                    // status 0; a failed run must not.
                    std::process::exit(1);
                }
            }
            let _ = gpui::AnyWindowHandle::from(window).update(cx, |_, window, _| window.remove_window());
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
    runtime.block_on(core.shutdown());
    if failed.load(std::sync::atomic::Ordering::SeqCst) {
        std::process::exit(1);
    }
    Ok(())
}
