//! Headless GPUI E2E for Codex goals in the composer (`goal-card-fixture`).
//!
//! Scenarios come from `evidence/goal-parity/failure-cases.md`, written before
//! the parity work. The production composer (Goal badge, goal-mode Send,
//! `/goal`, the goal bar and its editor), the production transcript ("Sent as
//! goal", "Goal achieved in …"), key bindings and the AccessKit tree run in a
//! hidden window. A synthetic engine answers the task-command path in memory
//! and holds every command until the fixture releases it, so pending states
//! are real. Engine progress (the goal message, the goal turn, Codex's goal
//! snapshots and the transcript marks) is seeded the way durable task state
//! reaches the UI. No LUNA app, provider, account or user data is involved:
//! this proves presentation and dispatch; the Codex protocol is proven by the
//! harness and engine suites.
use super::*;
use gpui::{AnyWindowHandle, AsyncApp, KeyUpEvent, Keystroke, PlatformInput};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_proto::{GoalReadback, TaskCommandRead, TaskCommandSnapshot};
use luna_rpc::{RpcReply, RpcService};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};

const CHAT: &str = "goal-fixture-chat";
const CLAUDE_CHAT: &str = "goal-fixture-claude";
/// A provider without native goals: no goal UI at all.
const OPENCODE_CHAT: &str = "goal-fixture-opencode";
/// Claude's goal is a condition its checker confirms after each turn.
const CLAUDE_CONDITION: &str = "DONE.txt exists in the project root";
/// `ReportFindings` rendered by the harness (`claude/findings.rs`), copied
/// verbatim from evidence/cc14-goals-review/e2e/harness/findings.json.
const CLAUDE_FINDINGS: &str = "1. **`src/auth.rs:42`** The token expiry check is inverted, so expired tokens are accepted\n   _correctness \u{b7} CONFIRMED_\n   Failure: A token that expired yesterday passes `is_valid()` and logs the user in\n2. **`src/db.rs`** Connection is never returned to the pool on error\n   _correctness \u{b7} PLAUSIBLE_\n   Failure: Ten failed queries exhaust the pool and every later request hangs\n\n1 malformed or extra finding(s) not shown.\n\n\n---\n_Provenance: Claude Code 2.1.286 `ReportFindings` \u{b7} high effort \u{b7} source `e3dcea5cbb8a` + uncommitted changes \u{b7} session `c1a0de00-0000-4000-8000-c5ce2bef53ff` \u{b7} call `toolu_rf1` \u{b7} payload sha256 `a22ee87bf52d9e05`_";
/// The user's report, verbatim.
const OBJECTIVE: &str = "yo what is this";
const EDITED: &str = "yo what is this — count to 99, one number per line";
const LONG_OBJECTIVE: &str = "Make the goal bar match Codex: pursue the objective with a live timer, \
keep Clear, Pause and Edit reachable from the keyboard, and show Goal achieved when Codex reports it.";

struct FixtureEngine {
    calls: Mutex<Vec<(String, Value)>>,
    /// One permit releases one held task command.
    gate: tokio::sync::Semaphore,
}

/// The fixture answers for the task it was asked about: the SDK refuses a
/// snapshot or receipt for another task (CORE-05).
fn snapshot(task_id: &str) -> TaskCommandSnapshot {
    TaskCommandSnapshot {
        task_id: task_id.into(),
        task_revision: 3,
        task_sequence: 4,
        binding_id: Some("fixture-binding".into()),
        binding_generation: Some(1),
        active_turn_id: None,
    }
}

#[async_trait::async_trait]
impl RpcService for FixtureEngine {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_owned(), params.clone()));
        match method {
            methods::GET_TASK_COMMAND_SNAPSHOT => RpcReply::value(&TaskCommandRead {
                snapshot: snapshot(params["taskId"].as_str().unwrap_or(CHAT)),
                events: Vec::new(),
                reload_required: false,
                next_task_sequence: None,
                has_more: false,
            }),
            methods::EXECUTE_TASK_COMMAND => {
                self.gate
                    .acquire()
                    .await
                    .map_err(|_| RpcError::Closed)?
                    .forget();
                RpcReply::value(&CommandReceipt {
                    command_id: params["commandId"].as_str().unwrap_or_default().into(),
                    task_id: params["taskId"].as_str().unwrap_or(CHAT).into(),
                    intent: params["intent"]["kind"].as_str().unwrap_or("goal").into(),
                    payload_hash: "fixture".into(),
                    status: CommandReceiptStatus::Applied,
                    snapshot: snapshot(params["taskId"].as_str().unwrap_or(CHAT)),
                    event_sequence: Some(5),
                    actor_id: "local".into(),
                    client_id: "local".into(),
                    client_session_id: "fixture".into(),
                    execution_environment: Some("host".into()),
                    correlation_id: None,
                    reason: None,
                    recovery_action: None,
                    accepted_at_ms: 1,
                    committed_at_ms: Some(2),
                    observed_at_ms: 2,
                })
            }
            methods::SAVE_COMPOSER_DRAFT => RpcReply::value(&ComposerDraftSaveOutcome::Saved),
            methods::GET_COMPOSER_DRAFT => {
                Ok(RpcReply::Value(json!({"draft": null, "submissions": []})))
            }
            methods::BEGIN_DRAFT_SUBMISSION | methods::SETTLE_DRAFT_SUBMISSION => {
                Ok(RpcReply::Value(json!({})))
            }
            methods::LIST_COMMANDS => RpcReply::value(&vec![
                // The provider's own `/goal` (Claude advertises one): LUNA's
                // goal command shadows it, so `/goal` is offered once.
                SlashCommand {
                    name: "goal".into(),
                    description: "Set a goal \u{2014} keep working until the condition is met"
                        .into(),
                    input_hint: None,
                    kind: SlashCommandKind::Command,
                },
                SlashCommand {
                    name: "review".into(),
                    description: "Review the current changes".into(),
                    input_hint: None,
                    kind: SlashCommandKind::Command,
                },
                SlashCommand {
                    name: "plan".into(),
                    description: "Plan before editing".into(),
                    input_hint: Some("task".into()),
                    kind: SlashCommandKind::Command,
                },
                // A skill: offered under `$`, never under `/`.
                SlashCommand {
                    name: "release-notes".into(),
                    description: "Draft release notes".into(),
                    input_hint: None,
                    kind: SlashCommandKind::Skill,
                },
            ]),
            other => Err(RpcError::UnknownMethod(other.into())),
        }
    }
}

impl FixtureEngine {
    fn executes(&self) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == methods::EXECUTE_TASK_COMMAND)
            .map(|(_, params)| params.clone())
            .collect()
    }
}

fn chat(id: &str, harness: &str, title: &str) -> anyhow::Result<luna_proto::Chat> {
    Ok(serde_json::from_value(json!({
        "id": id,
        "deviceId": "local",
        "title": title,
        "archived": false,
        "cwd": "/tmp/goal-card-fixture",
        "branch": "main",
        "config": {"harness": harness, "model": "gpt-6.1-sol", "sandbox": "workspace-write"},
        "createdAt": "2026-10-01T08:00:00Z",
    }))?)
}

fn goal(
    status: GoalStatus,
    objective: &str,
    used: i64,
    budget: Option<i64>,
    secs: i64,
) -> ThreadGoal {
    let now = chrono::Utc::now().timestamp();
    ThreadGoal {
        thread_id: "fixture-thread".into(),
        objective: objective.into(),
        status,
        token_budget: budget,
        tokens_used: used,
        time_used_seconds: secs,
        created_at: now - secs,
        updated_at: now - 20,
        evaluation: None,
    }
}

fn entry(id: &str, role: MessageRole, text: &str, streaming: bool) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role,
        parts: vec![MessagePart::Text {
            id: format!("{id}-text"),
            text: text.into(),
        }],
        created_at: chrono::Utc::now().timestamp_millis(),
        device_id: "local".into(),
        status: Some(if streaming {
            MessageStatus::Streaming
        } else {
            MessageStatus::Complete
        }),
        continuation_of: None,
    }
}

/// Durable engine state as it reaches the UI: transcript, Codex's goal
/// snapshot (observed `checked_ago_ms` ago), goal marks and the session.
#[allow(clippy::too_many_arguments)]
fn engine_state(
    state: &Entity<AppState>,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    transcript: Vec<SessionMessageEntry>,
    goal: Option<ThreadGoal>,
    checked_ago_ms: i64,
    marks: luna_proto::GoalMarks,
    working: bool,
) -> anyhow::Result<()> {
    let state = state.clone();
    act(window, cx, move |_, cx| {
        state.update(cx, |state, cx| {
            state.apply_transcript(transcript);
            state.context_control.goal = Some(GoalReadback {
                thread_id: "fixture-thread".into(),
                binding_generation: 1,
                goal,
                checked_at_ms: chrono::Utc::now().timestamp_millis() - checked_ago_ms,
                continuation_ready: true,
            });
            state.context_control.goal_marks = marks;
            state.sessions = vec![luna_proto::Session {
                last_completed_turn: None,
                chat_id: CHAT.into(),
                device_id: "local".into(),
                status: if working {
                    luna_proto::SessionStatus::Working
                } else {
                    luna_proto::SessionStatus::Idle
                },
                started_at: None,
                updated_at: chrono::Utc::now(),
                recovery: None,
            }];
            cx.notify();
        })
    })
}

fn claude_goal(secs: i64, checks: u32, reason: Option<&str>) -> ThreadGoal {
    let now = chrono::Utc::now().timestamp();
    ThreadGoal {
        thread_id: "claude-fixture-session".into(),
        objective: CLAUDE_CONDITION.into(),
        status: GoalStatus::Active,
        token_budget: None,
        tokens_used: 0,
        time_used_seconds: secs,
        created_at: now - secs,
        updated_at: now,
        evaluation: Some(luna_proto::GoalEvaluation {
            checks,
            last_reason: reason.map(str::to_owned),
        }),
    }
}

fn schedule(
    id: &str,
    kind: luna_proto::NativeScheduleKind,
    when: &str,
    prompt: &str,
    recurring: bool,
    state: luna_proto::NativeScheduleState,
) -> luna_proto::NativeSchedule {
    let now = chrono::Utc::now().timestamp_millis();
    luna_proto::NativeSchedule {
        id: id.into(),
        kind,
        schedule: when.into(),
        cron: (kind == luna_proto::NativeScheduleKind::Cron).then(|| "*/5 * * * *".into()),
        prompt: prompt.into(),
        recurring,
        durable: false,
        created_at_ms: now,
        next_fire_at_ms: (kind == luna_proto::NativeScheduleKind::Wakeup).then_some(now + 600_000),
        expires_at_ms: recurring.then_some(now + 7 * 86_400_000),
        state,
    }
}

/// A Claude chat's durable state as it reaches the UI. Headless Claude never
/// continues a goal on its own (`continuationReady: false`).
#[allow(clippy::too_many_arguments)]
fn claude_state(
    state: &Entity<AppState>,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    transcript: Vec<SessionMessageEntry>,
    goal: Option<ThreadGoal>,
    marks: luna_proto::GoalMarks,
    schedules: Vec<luna_proto::NativeSchedule>,
    working: bool,
) -> anyhow::Result<()> {
    let state = state.clone();
    act(window, cx, move |_, cx| {
        state.update(cx, |state, cx| {
            state.selected_chat = Some(CLAUDE_CHAT.into());
            state.apply_transcript(transcript);
            state.context_control.goal = Some(GoalReadback {
                thread_id: "claude-fixture-session".into(),
                binding_generation: 1,
                goal,
                checked_at_ms: chrono::Utc::now().timestamp_millis() - 2_000,
                continuation_ready: false,
            });
            state.context_control.goal_marks = marks;
            state.context_control.native_schedules = schedules;
            state.sessions = vec![luna_proto::Session {
                last_completed_turn: None,
                chat_id: CLAUDE_CHAT.into(),
                device_id: "local".into(),
                status: if working {
                    luna_proto::SessionStatus::Working
                } else {
                    luna_proto::SessionStatus::Idle
                },
                started_at: None,
                updated_at: chrono::Utc::now(),
                recovery: None,
            }];
            cx.notify();
        })
    })
}

fn act(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut Window, &mut App),
) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| f(window, cx))
}

fn composer_do(
    composer: &Entity<Composer>,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut Composer, &mut Context<Composer>),
) -> anyhow::Result<()> {
    let composer = composer.clone();
    act(window, cx, move |_, cx| composer.update(cx, f))
}

fn input_edit(
    composer: &Entity<Composer>,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut ComposerInput, &mut Window, &mut Context<ComposerInput>),
) -> anyhow::Result<()> {
    let input = cx.update(|cx| composer.read(cx).input.clone());
    act(window, cx, |window, cx| {
        input.update(cx, |input, cx| f(input, window, cx))
    })
}

fn type_into(
    input: Entity<ComposerInput>,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    text: &str,
) -> anyhow::Result<()> {
    let text = text.to_owned();
    act(window, cx, move |window, cx| {
        input.update(cx, |input, cx| {
            let end = input.text().encode_utf16().count();
            input.replace_text_in_range(Some(end..end), &text, window, cx)
        })
    })
}

/// Key press and release through the window: bindings, then listeners.
fn key_press(window: AnyWindowHandle, cx: &mut AsyncApp, stroke: &str) -> anyhow::Result<()> {
    let keystroke = Keystroke::parse(stroke)
        .map_err(|error| anyhow::anyhow!("keystroke {stroke}: {error:?}"))?;
    let release = keystroke.clone();
    act(window, cx, |window, cx| {
        window.dispatch_event(
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke,
                is_held: false,
                prefer_character_input: false,
            }),
            cx,
        );
    })?;
    act(window, cx, |window, cx| {
        window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke: release }), cx);
    })
}

fn draw(window: AnyWindowHandle, cx: &mut AsyncApp) -> anyhow::Result<()> {
    act(window, cx, |window, cx| {
        window.draw(cx).clear();
    })
}

async fn pause(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

async fn wait_for(
    cx: &mut AsyncApp,
    what: &str,
    mut done: impl FnMut(&mut AsyncApp) -> bool,
) -> anyhow::Result<()> {
    for _ in 0..400 {
        if done(cx) {
            return Ok(());
        }
        pause(cx, 10).await;
    }
    anyhow::bail!("timed out waiting for {what}")
}

/// Let entrance fades finish, then capture the real rendered frame.
async fn shot(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    output: &Path,
    name: &str,
) -> anyhow::Result<String> {
    draw(window, cx)?;
    pause(cx, 450).await;
    let path = output.join(name);
    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(&path)?;
        Ok(())
    })??;
    Ok(name.to_owned())
}

#[cfg(target_os = "macos")]
fn activate_accessibility(window: &Window) -> anyhow::Result<()> {
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow::anyhow!("window handle unavailable: {error}"))?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        anyhow::bail!("fixture window is not an AppKit window");
    };
    let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    // SAFETY: the view belongs to this live window on the main thread; each
    // returned pointer is checked before it is messaged.
    let ns_window: *mut AnyObject = unsafe { objc2::msg_send![view, window] };
    anyhow::ensure!(!ns_window.is_null(), "fixture view has no NSWindow");
    let content: *mut AnyObject = unsafe { objc2::msg_send![ns_window, contentView] };
    anyhow::ensure!(!content.is_null(), "fixture NSWindow has no content view");
    let _children: *mut AnyObject = unsafe { objc2::msg_send![content, accessibilityChildren] };
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn activate_accessibility(_: &Window) -> anyhow::Result<()> {
    anyhow::bail!("AccessKit activation readback is implemented for macOS only")
}

fn a11y_tree(window: AnyWindowHandle, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    draw(window, cx)?;
    draw(window, cx)?;
    let json = window
        .update(cx, |_, window, _| window.debug_a11y_tree_json())?
        .ok_or_else(|| anyhow::anyhow!("AccessKit tree was not built (activation failed)"))?;
    Ok(serde_json::from_str(&json)?)
}

fn focused_label(tree: &Value) -> Option<String> {
    let id = tree["gpui_focus"].as_str()?;
    tree["nodes"][id]["aria"]["label"]
        .as_str()
        .map(str::to_owned)
}

fn focus_input(
    composer: &Entity<Composer>,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let composer = composer.clone();
    act(window, cx, move |window, cx| {
        let focus = composer.read(cx).input.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    })
}

/// Shift-Tab from the composer input: the card sits above it, so reverse
/// traversal reaches its controls first. Returns every focused label.
fn reverse_tab_walk(
    composer: &Entity<Composer>,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    until: impl Fn(&str) -> bool,
) -> anyhow::Result<Vec<String>> {
    focus_input(composer, window, cx)?;
    let mut walk = Vec::new();
    for _ in 0..24 {
        act(window, cx, |window, cx| window.focus_prev(cx))?;
        let tree = a11y_tree(window, cx)?;
        let label = focused_label(&tree).unwrap_or_else(|| "<unlabelled>".into());
        let stop = until(&label);
        walk.push(label);
        if stop {
            return Ok(walk);
        }
    }
    anyhow::bail!("keyboard traversal never reached the goal control: {walk:?}")
}

const GOAL_BUTTONS: &[&str] = &[
    "Clear goal",
    "Edit goal",
    "Pause goal",
    "Resume goal",
    "Refresh goal status",
    "Undo goal edits",
    "Close goal editor",
    "Save goal",
    "Dismiss goal message",
];

fn has_action(aria: &Value, action: &str) -> bool {
    aria["on_action"]
        .as_array()
        .is_some_and(|actions| actions.iter().any(|value| value == action))
}

/// Every goal node AccessKit exposes (bar, controls, Goal badge, transcript
/// marks), sorted by label.
fn goal_nodes(tree: &Value) -> Vec<Value> {
    let mut nodes: Vec<Value> = tree["nodes"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, node)| &node["aria"])
        .filter(|aria| {
            aria["label"].as_str().is_some_and(|label| {
                label == "Goal"
                    || label == "Goal mode"
                    || label == "Sent as goal"
                    || label == "Edit goal"
                    || label.starts_with("Pursuing goal")
                    || label.starts_with("Goal ")
                    || GOAL_BUTTONS.contains(&label)
            })
        })
        .map(|aria| {
            json!({
                "label": aria["label"],
                "role": aria["role"],
                "click": has_action(aria, "Click"),
                "focus": has_action(aria, "Focus"),
                "toggled": aria["toggled"],
                "description": aria["description"],
            })
        })
        .collect();
    nodes.sort_by(|a, b| a["label"].as_str().cmp(&b["label"].as_str()));
    nodes
}

fn node<'a>(nodes: &'a [Value], label: &str) -> Option<&'a Value> {
    nodes.iter().find(|node| node["label"] == label)
}

fn node_starting<'a>(nodes: &'a [Value], prefix: &str) -> Option<&'a Value> {
    nodes.iter().find(|node| {
        node["label"]
            .as_str()
            .is_some_and(|label| label.starts_with(prefix))
    })
}

/// The bar exists only while an unfinished goal (or its change) is shown.
fn bar_nodes(nodes: &[Value]) -> Vec<&Value> {
    nodes
        .iter()
        .filter(|node| {
            node["label"] != "Goal mode"
                && node["label"] != "Sent as goal"
                && node["role"] != "Status"
        })
        .collect()
}

fn input_placeholder(tree: &Value) -> Option<String> {
    tree["nodes"]
        .as_object()?
        .values()
        .map(|node| &node["aria"])
        .find(|aria| aria["role"] == "MultilineTextInput" && aria["placeholder"].is_string())
        .and_then(|aria| aria["placeholder"].as_str().map(str::to_owned))
}

/// Tab forward from the composer input (the Goal badge sits after it).
fn tab_walk(
    composer: &Entity<Composer>,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    until: impl Fn(&str) -> bool,
) -> anyhow::Result<Vec<String>> {
    focus_input(composer, window, cx)?;
    let mut walk = Vec::new();
    for _ in 0..24 {
        act(window, cx, |window, cx| window.focus_next(cx))?;
        let tree = a11y_tree(window, cx)?;
        let label = focused_label(&tree).unwrap_or_else(|| "<unlabelled>".into());
        let stop = until(&label);
        walk.push(label);
        if stop {
            return Ok(walk);
        }
    }
    anyhow::bail!("keyboard traversal never reached the control: {walk:?}")
}

fn card(composer: &Entity<Composer>, cx: &mut AsyncApp) -> Value {
    cx.update(|cx| {
        let this = composer.read(cx);
        json!({
            "input": this.input.read(cx).text(),
            "placeholder": this.input.read(cx).placeholder.to_string(),
            "goalMode": this.goal_mode_on(cx),
            "pending": this.goal_ui.pending.as_ref().map(|pending| format!("{:?}", pending.kind)),
            "editor": this.goal_ui.editor.as_ref().map(|editor| json!({
                "chat": editor.chat_id,
                "objective": editor.objective.read(cx).text(),
                "budget": editor.budget.read(cx).text(),
            })),
            "error": this.goal_ui.error.as_ref().map(|(_, error)| error.clone()),
            "failure": this.failure.as_ref().map(|failure| failure.to_string()),
            "slashOpen": this.slash.token.is_some(),
            "slashCommands": this.slash_commands(cx).iter().map(|command| command.name.clone()).collect::<Vec<_>>(),
        })
    })
}

fn sent(executes: &[Value], index: usize) -> anyhow::Result<&Value> {
    executes
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("command {index} was not dispatched: {executes:?}"))
}

/// Drive every goal scenario; returns the JSON evidence document and writes
/// the PNG captures into `output`.
pub async fn run_goal_card_e2e(
    composer: Entity<Composer>,
    state: Entity<AppState>,
    window: AnyWindowHandle,
    output: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<Value> {
    let mut steps: Vec<Value> = Vec::new();
    let mut shots: Vec<String> = Vec::new();

    // Synthetic engine and chats. The Codex chat owns the goal; the Claude
    // chat proves goal UI stays Codex-only.
    let rpc = Arc::new(FixtureEngine {
        calls: Mutex::new(Vec::new()),
        gate: tokio::sync::Semaphore::new(0),
    });
    let service: Arc<dyn RpcService> = rpc.clone();
    let client = cx
        .update(|cx| gpui_tokio::Tokio::spawn(cx, async move { luna_rpc::memory_client(service) }))
        .await?;
    let engine = crate::state::EngineHandle::from_authenticated_remote(
        client,
        luna_proto::EngineInfo {
            device_id: "local".into(),
            workspace_scope: luna_proto::WorkspaceScope::Local,
            capabilities: Vec::new(),
            profile_id: None,
        },
        "memory://goal-card-fixture".into(),
    );
    let chats = vec![
        chat(CHAT, "codex", "Goal parity fixture")?,
        chat(CLAUDE_CHAT, "claude-code", "Claude fixture")?,
        chat(OPENCODE_CHAT, "opencode", "OpenCode fixture")?,
    ];
    {
        let state = state.clone();
        act(window, cx, move |window, cx| {
            state.update(cx, |state, cx| {
                state.set_test_engine(engine);
                state.local_device_id = Some("local".into());
                state.chats = chats;
                state.selected_chat = Some(CHAT.into());
                cx.notify();
            });
            if let Err(error) = activate_accessibility(window) {
                tracing::warn!(%error, "goal fixture accessibility activation failed");
            }
        })?;
    }
    engine_state(
        &state,
        window,
        cx,
        Vec::new(),
        None,
        0,
        Default::default(),
        false,
    )?;

    // 1. No goal and no `/goal` typed: no bar and no Goal badge.
    shots.push(shot(window, cx, output, "01-no-goal.png").await?);
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    anyhow::ensure!(
        bar_nodes(&nodes).is_empty() && node(&nodes, "Goal mode").is_none(),
        "no-goal state wrong: {nodes:?}"
    );
    steps.push(json!({"step": "no-goal", "goalNodes": nodes, "card": card(&composer, cx)}));

    // 2. Only an explicit bare `/goal` turns goal mode on: the first Enter
    // accepts the `/goal` completion, the second sends it, nothing is
    // dispatched, and the Goal badge appears with Codex's placeholder. Space
    // on the badge turns goal mode off and removes it; `/goal` brings it back.
    let enter_goal_mode = |cx: &mut AsyncApp| -> anyhow::Result<()> {
        focus_input(&composer, window, cx)?;
        input_edit(&composer, window, cx, |input, window, cx| {
            input.replace_text_in_range(None, "/goal", window, cx)
        })?;
        key_press(window, cx, "enter")?;
        key_press(window, cx, "enter")
    };
    enter_goal_mode(cx)?;
    pause(cx, 80).await;
    let walk = tab_walk(&composer, window, cx, |label| label == "Goal mode")?;
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    let on = card(&composer, cx);
    anyhow::ensure!(
        on["goalMode"] == true
            && on["input"] == ""
            && rpc.executes().is_empty()
            && on["placeholder"] == GOAL_PLACEHOLDER
            && input_placeholder(&tree).as_deref() == Some(GOAL_PLACEHOLDER)
            && node(&nodes, "Goal mode").is_some_and(|node| node["role"] == "Switch"
                && node["toggled"] == "True"
                && node["click"] == true
                && node["focus"] == true),
        "bare /goal did not show the Goal badge: {on} {nodes:?}"
    );
    key_press(window, cx, "space")?;
    let tree = a11y_tree(window, cx)?;
    let off = card(&composer, cx);
    anyhow::ensure!(
        off["goalMode"] == false
            && off["placeholder"] == "Do anything…"
            && node(&goal_nodes(&tree), "Goal mode").is_none(),
        "the Goal badge did not turn goal mode off: {off}"
    );
    enter_goal_mode(cx)?;
    pause(cx, 80).await;
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    let on = card(&composer, cx);
    anyhow::ensure!(
        on["goalMode"] == true
            && rpc.executes().is_empty()
            && node(&nodes, "Goal mode").is_some_and(|node| node["toggled"] == "True"),
        "bare /goal did not re-enter goal mode: {on} {nodes:?}"
    );
    shots.push(shot(window, cx, output, "02-goal-mode.png").await?);
    steps.push(json!({"step": "goal-mode-on", "tabWalk": walk, "goalNodes": nodes, "off": off, "card": on}));

    // 3. Send in goal mode: ONE session run carrying the goal start (never a
    // queued message, never a goal-only control), then goal mode resets.
    focus_input(&composer, window, cx)?;
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_text_in_range(None, OBJECTIVE, window, cx)
    })?;
    key_press(window, cx, "enter")?;
    wait_for(cx, "the goal send", |_| rpc.executes().len() == 1).await?;
    let executes = rpc.executes();
    let run = sent(&executes, 0)?;
    let request = &run["intent"]["command"]["request"];
    let message_id = run["commandId"].as_str().unwrap_or_default().to_owned();
    anyhow::ensure!(
        run["intent"]["kind"] == "session"
            && run["intent"]["command"]["kind"] == "run"
            && run["taskId"] == CHAT
            && request["goal"]["objective"] == OBJECTIVE
            && request["prompt"] == OBJECTIVE
            && request["goal"].get("tokenBudget").is_none()
            && !message_id.is_empty(),
        "goal-mode Send did not dispatch a goal start: {run}"
    );
    rpc.gate.add_permits(1);
    wait_for(cx, "the goal send to settle", |cx| {
        cx.update(|cx| !composer.read(cx).sending)
    })
    .await?;
    let after = card(&composer, cx);
    anyhow::ensure!(
        after["goalMode"] == false
            && after["placeholder"] == "Do anything…"
            && after["input"] == "",
        "goal mode or draft did not reset after the goal was handed off: {after}"
    );
    steps.push(json!({"step": "goal-sent", "intent": run["intent"], "card": after}));

    // 4. Codex starts working at once: the goal message is "Sent as goal",
    // the agent streams, and the bar pursues the goal with a live timer.
    let assistant = "goal-fixture-turn";
    let mut marks = luna_proto::GoalMarks::default();
    marks.mark_sent(&message_id);
    let working_transcript = vec![
        entry(&message_id, MessageRole::User, OBJECTIVE, false),
        entry(
            assistant,
            MessageRole::Assistant,
            "Looking at the workspace to see what this is…",
            true,
        ),
    ];
    engine_state(
        &state,
        window,
        cx,
        working_transcript.clone(),
        Some(goal(GoalStatus::Active, OBJECTIVE, 1_820, None, 0)),
        3_000,
        marks.clone(),
        true,
    )?;
    shots.push(shot(window, cx, output, "03-pursuing-goal.png").await?);
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    let summary = node_starting(&nodes, "Pursuing goal, ")
        .and_then(|node| node["label"].as_str())
        .map(str::to_owned)
        .unwrap_or_default();
    anyhow::ensure!(
        summary.starts_with(&format!("Pursuing goal, {OBJECTIVE}, "))
            && summary.ends_with("so far")
            && ["Edit goal", "Pause goal", "Clear goal"]
                .iter()
                .all(|label| {
                    node(&nodes, label)
                        .is_some_and(|node| node["click"] == true && node["focus"] == true)
                })
            && node(&nodes, "Goal").is_some_and(|node| node["role"] == "Group")
            && node(&nodes, "Sent as goal").is_some(),
        "pursuing bar semantics wrong: {nodes:?}"
    );
    // The timer is live: it advances while Codex works.
    pause(cx, 1_300).await;
    let tree = a11y_tree(window, cx)?;
    let later = node_starting(&goal_nodes(&tree), "Pursuing goal, ")
        .and_then(|node| node["label"].as_str())
        .map(str::to_owned)
        .unwrap_or_default();
    anyhow::ensure!(
        later != summary,
        "the goal timer did not advance: {summary} → {later}"
    );
    let walk = reverse_tab_walk(&composer, window, cx, |label| label == "Edit goal")?;
    anyhow::ensure!(
        walk.iter().any(|label| label == "Clear goal")
            && walk.iter().any(|label| label == "Pause goal"),
        "bar controls are not keyboard reachable: {walk:?}"
    );
    steps.push(json!({"step": "pursuing", "summary": summary, "summaryAfter": later, "reverseTabWalk": walk, "goalNodes": nodes}));

    // 5. Edit (Enter on the focused Edit control): Codex's Edit goal surface,
    // prefilled; Undo restores, Save sends one objective change.
    key_press(window, cx, "enter")?;
    let editing = card(&composer, cx);
    anyhow::ensure!(
        editing["editor"]["objective"] == OBJECTIVE && editing["editor"]["budget"] == "",
        "edit editor not prefilled: {editing}"
    );
    let inputs = cx.update(|cx| {
        composer
            .read(cx)
            .goal_ui
            .editor
            .as_ref()
            .map(|editor| editor.objective.clone())
    });
    let Some(objective_input) = inputs else {
        anyhow::bail!("goal editor has no objective input");
    };
    draw(window, cx)?;
    type_into(objective_input.clone(), window, cx, " — and more")?;
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    anyhow::ensure!(
        node(&nodes, "Undo goal edits").is_some_and(|node| node["click"] == true)
            && node(&nodes, "Save goal").is_some_and(|node| node["click"] == true),
        "editor controls missing: {nodes:?}"
    );
    shots.push(shot(window, cx, output, "04-edit-goal.png").await?);
    composer_do(&composer, window, cx, |this, cx| this.undo_goal_editor(cx))?;
    let undone = card(&composer, cx);
    anyhow::ensure!(
        undone["editor"]["objective"] == OBJECTIVE,
        "Undo did not restore Codex's objective: {undone}"
    );
    let edited = EDITED.trim_start_matches(OBJECTIVE).to_owned();
    type_into(objective_input.clone(), window, cx, &edited)?;
    let focus = objective_input.clone();
    act(window, cx, move |window, cx| {
        let focus = focus.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    })?;
    key_press(window, cx, "enter")?;
    wait_for(cx, "the held edit", |_| rpc.executes().len() == 2).await?;
    let executes = rpc.executes();
    let edit = sent(&executes, 1)?;
    let operation = &edit["intent"]["operation"];
    anyhow::ensure!(
        edit["intent"]["kind"] == "goal"
            && operation["action"] == "set"
            && operation["objective"] == EDITED
            && operation["status"].is_null()
            && operation["tokenBudget"]["mode"] == "unlimited",
        "Save did not send one objective change: {edit}"
    );
    rpc.gate.add_permits(1);
    wait_for(cx, "edit applied", |cx| {
        cx.update(|cx| composer.read(cx).goal_ui.pending.is_none())
    })
    .await?;
    anyhow::ensure!(
        card(&composer, cx)["editor"].is_null(),
        "applied edit left the editor open"
    );
    steps.push(json!({"step": "edit", "intent": edit["intent"], "undone": undone}));

    // 6. Pause (Codex keeps the running turn; no continuation after it), then
    // Resume (Codex starts a turn when idle) from the keyboard.
    engine_state(
        &state,
        window,
        cx,
        working_transcript.clone(),
        Some(goal(GoalStatus::Active, EDITED, 9_400, None, 21)),
        1_000,
        marks.clone(),
        true,
    )?;
    reverse_tab_walk(&composer, window, cx, |label| label == "Pause goal")?;
    key_press(window, cx, "space")?;
    wait_for(cx, "the held pause", |_| rpc.executes().len() == 3).await?;
    let executes = rpc.executes();
    let pause_intent = sent(&executes, 2)?["intent"].clone();
    anyhow::ensure!(
        pause_intent["operation"]["action"] == "set"
            && pause_intent["operation"]["status"] == "paused"
            && pause_intent["operation"]["objective"].is_null(),
        "Pause sent the wrong change: {pause_intent}"
    );
    rpc.gate.add_permits(1);
    wait_for(cx, "pause applied", |cx| {
        cx.update(|cx| composer.read(cx).goal_ui.pending.is_none())
    })
    .await?;
    engine_state(
        &state,
        window,
        cx,
        working_transcript.clone(),
        Some(goal(GoalStatus::Paused, EDITED, 9_400, None, 65)),
        1_000,
        marks.clone(),
        false,
    )?;
    shots.push(shot(window, cx, output, "05-paused.png").await?);
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    anyhow::ensure!(
        node_starting(&nodes, "Goal paused, ").is_some()
            && node(&nodes, "Resume goal").is_some_and(|node| node["click"] == true)
            && node(&nodes, "Pause goal").is_none(),
        "paused bar semantics wrong: {nodes:?}"
    );
    reverse_tab_walk(&composer, window, cx, |label| label == "Resume goal")?;
    key_press(window, cx, "enter")?;
    wait_for(cx, "the held resume", |_| rpc.executes().len() == 4).await?;
    let executes = rpc.executes();
    let resume_intent = sent(&executes, 3)?["intent"].clone();
    anyhow::ensure!(
        resume_intent["operation"]["status"] == "active",
        "Resume sent the wrong change: {resume_intent}"
    );
    rpc.gate.add_permits(1);
    wait_for(cx, "resume applied", |cx| {
        cx.update(|cx| composer.read(cx).goal_ui.pending.is_none())
    })
    .await?;
    steps.push(json!({"step": "pause-resume", "pause": pause_intent, "resume": resume_intent, "pausedNodes": nodes}));

    // 7. Clear from the keyboard, held: every goal control is disabled, and
    // neither the keyboard nor a goal send may start a second change.
    engine_state(
        &state,
        window,
        cx,
        working_transcript.clone(),
        Some(goal(GoalStatus::Active, EDITED, 9_400, None, 65)),
        1_000,
        marks.clone(),
        true,
    )?;
    reverse_tab_walk(&composer, window, cx, |label| label == "Clear goal")?;
    key_press(window, cx, "space")?;
    wait_for(cx, "the held clear", |_| rpc.executes().len() == 5).await?;
    let executes = rpc.executes();
    let clear = sent(&executes, 4)?["intent"].clone();
    anyhow::ensure!(
        clear["kind"] == "goal" && clear["operation"]["action"] == "clear",
        "Clear dispatched the wrong intent: {clear}"
    );
    shots.push(shot(window, cx, output, "06-pending-clear.png").await?);
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    let disabled = GOAL_BUTTONS
        .iter()
        .filter_map(|label| node(&nodes, label))
        .all(|node| node["click"] == false && node["focus"] == false);
    anyhow::ensure!(
        disabled && node(&nodes, "Clear goal").is_some(),
        "controls stayed active while a goal change was pending: {nodes:?}"
    );
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_text_in_range(None, "/goal Another objective", window, cx)
    })?;
    focus_input(&composer, window, cx)?;
    key_press(window, cx, "enter")?;
    pause(cx, 120).await;
    let blocked = card(&composer, cx);
    anyhow::ensure!(
        rpc.executes().len() == 5
            && blocked["failure"]
                .as_str()
                .is_some_and(|failure| failure.contains("current goal change")),
        "a goal send started while a goal change was pending: {blocked}"
    );
    rpc.gate.add_permits(1);
    wait_for(cx, "clear applied", |cx| {
        cx.update(|cx| composer.read(cx).goal_ui.pending.is_none())
    })
    .await?;
    input_edit(&composer, window, cx, |input, _, cx| input.set_text("", cx))?;
    composer_do(&composer, window, cx, |this, cx| {
        this.failure = None;
        cx.notify();
    })?;
    engine_state(
        &state,
        window,
        cx,
        working_transcript.clone(),
        None,
        0,
        marks.clone(),
        false,
    )?;
    let tree = a11y_tree(window, cx)?;
    anyhow::ensure!(
        bar_nodes(&goal_nodes(&tree)).is_empty(),
        "cleared goal still renders: {:?}",
        goal_nodes(&tree)
    );
    steps.push(json!({"step": "pending-clear", "intent": clear, "goalNodes": nodes, "blockedSend": blocked}));

    // 8. Codex completes the goal: the bar goes away and the turn reads
    // "Goal achieved in 36s".
    let mut done = marks.clone();
    done.mark_achieved(assistant, 36);
    let achieved_transcript = vec![
        entry(&message_id, MessageRole::User, OBJECTIVE, false),
        entry(
            assistant,
            MessageRole::Assistant,
            "Counted from 1 to 99, one number per line.\n\nReached 99. Goal complete in 36 seconds.",
            false,
        ),
    ];
    engine_state(
        &state,
        window,
        cx,
        achieved_transcript,
        Some(goal(GoalStatus::Complete, EDITED, 21_440, None, 36)),
        0,
        done,
        false,
    )?;
    shots.push(shot(window, cx, output, "07-goal-achieved.png").await?);
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    anyhow::ensure!(
        bar_nodes(&nodes).is_empty()
            && node(&nodes, "Goal achieved in 36s").is_some_and(|node| node["role"] == "Status"),
        "achieved goal semantics wrong: {nodes:?}"
    );
    steps.push(json!({"step": "achieved", "goalNodes": nodes}));

    // 9. `/goal` alone turns goal mode on (Codex's command); `/goal <text>`
    // sends a goal start directly.
    focus_input(&composer, window, cx)?;
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_text_in_range(None, "/goal", window, cx)
    })?;
    let completing = card(&composer, cx);
    // The first Enter accepts the open `/goal` completion, the second sends.
    key_press(window, cx, "enter")?;
    key_press(window, cx, "enter")?;
    pause(cx, 80).await;
    let bare = card(&composer, cx);
    anyhow::ensure!(
        bare["goalMode"] == true && bare["input"] == "" && rpc.executes().len() == 5,
        "bare /goal did not switch goal mode on without dispatch: {bare}"
    );
    composer_do(&composer, window, cx, |this, cx| {
        this.set_goal_mode(false, cx)
    })?;
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_text_in_range(None, "/goal Ship the release notes", window, cx)
    })?;
    focus_input(&composer, window, cx)?;
    key_press(window, cx, "enter")?;
    wait_for(cx, "the /goal send", |_| rpc.executes().len() == 6).await?;
    let executes = rpc.executes();
    let command = sent(&executes, 5)?;
    anyhow::ensure!(
        command["intent"]["command"]["request"]["goal"]["objective"] == "Ship the release notes"
            && command["intent"]["command"]["request"]["prompt"] == "Ship the release notes",
        "/goal <text> did not start a goal: {command}"
    );
    rpc.gate.add_permits(1);
    wait_for(cx, "the /goal send to settle", |cx| {
        cx.update(|cx| !composer.read(cx).sending)
    })
    .await?;
    steps.push(json!({"step": "goal-command", "completing": completing, "bare": bare, "intent": command["intent"]}));
    // E23 (cc00-baseline G-17): any discovery is scoped to this chat and its
    // effective mode, so the engine probes the run's own configuration.
    // LUNA's own `/goal` needs none; step 12 requires the Claude chat's call.
    let list_calls: Vec<Value> = rpc
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == methods::LIST_COMMANDS)
        .map(|(_, params)| params.clone())
        .collect();
    anyhow::ensure!(
        list_calls.iter().all(|scoped| scoped["chatId"] == CHAT
            && scoped["harness"] == "codex"
            && scoped["permissionMode"].is_string()),
        "ListCommands was not scoped to the chat and mode: {list_calls:?}"
    );
    steps.push(json!({"step": "slash-scope", "listCommands": list_calls}));

    // 10. A goal is text only: mentions are explained, never dropped.
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_text_in_range(None, "Summarize the readme", window, cx)
    })?;
    composer_do(&composer, window, cx, |this, cx| {
        this.input_items.insert(
            this.current_key.clone(),
            vec![luna_proto::ComposerInput::Mention {
                name: "README".into(),
                path: "/tmp/goal-card-fixture/README.md".into(),
            }],
        );
        this.set_goal_mode(true, cx);
    })?;
    focus_input(&composer, window, cx)?;
    key_press(window, cx, "enter")?;
    pause(cx, 120).await;
    let text_only = card(&composer, cx);
    anyhow::ensure!(
        rpc.executes().len() == 6
            && text_only["failure"]
                .as_str()
                .is_some_and(|failure| failure.contains("text only"))
            && text_only["input"] == "Summarize the readme",
        "a goal with a mention was dispatched or silent: {text_only}"
    );
    composer_do(&composer, window, cx, |this, cx| {
        this.input_items.clear();
        this.set_goal_mode(false, cx);
        this.failure = None;
    })?;
    input_edit(&composer, window, cx, |input, _, cx| input.set_text("", cx))?;
    steps.push(json!({"step": "text-only", "card": text_only}));

    // 11. Light theme: a long objective pursued, and the Goal switch on.
    act(window, cx, |_, cx| {
        crate::appearance::set_mode(crate::appearance::AppearanceMode::Light, cx)
    })?;
    engine_state(
        &state,
        window,
        cx,
        working_transcript.clone(),
        Some(goal(
            GoalStatus::Active,
            LONG_OBJECTIVE,
            18_240,
            Some(50_000),
            754,
        )),
        2_000,
        marks.clone(),
        true,
    )?;
    composer_do(&composer, window, cx, |this, cx| {
        this.set_goal_mode(true, cx)
    })?;
    shots.push(shot(window, cx, output, "08-light-pursuing.png").await?);
    composer_do(&composer, window, cx, |this, cx| {
        this.set_goal_mode(false, cx)
    })?;
    act(window, cx, |_, cx| {
        crate::appearance::set_mode(crate::appearance::AppearanceMode::Dark, cx)
    })?;

    // 12. Claude chat: no badge until `/goal`, then the same Goal badge and
    // one `/goal` (LUNA's shadows Claude's native entry), with Claude's
    // condition wording. `/` lists commands only and `$` skills only.
    claude_state(
        &state,
        window,
        cx,
        Vec::new(),
        None,
        Default::default(),
        Vec::new(),
        false,
    )?;
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    anyhow::ensure!(
        bar_nodes(&nodes).is_empty() && node(&nodes, "Goal mode").is_none(),
        "Claude no-goal state wrong: {nodes:?}"
    );
    enter_goal_mode(cx)?;
    pause(cx, 80).await;
    let walk = tab_walk(&composer, window, cx, |label| label == "Goal mode")?;
    let tree = a11y_tree(window, cx)?;
    let on = card(&composer, cx);
    anyhow::ensure!(
        on["goalMode"] == true
            && on["placeholder"] == CLAUDE_GOAL_PLACEHOLDER
            && input_placeholder(&tree).as_deref() == Some(CLAUDE_GOAL_PLACEHOLDER)
            && node(&goal_nodes(&tree), "Goal mode").is_some_and(|node| node["toggled"] == "True"
                && node["description"]
                    .as_str()
                    .is_some_and(|d| d.contains("checker confirms"))),
        "Claude goal mode wrong: {on}"
    );
    shots.push(shot(window, cx, output, "09-claude-goal-mode.png").await?);
    focus_input(&composer, window, cx)?;
    type_into(
        cx.update(|cx| composer.read(cx).input.clone()),
        window,
        cx,
        "/go",
    )?;
    draw(window, cx)?;
    // Claude's own command list (with its native `goal`) has to arrive for
    // the shadowing to be exercised.
    wait_for(cx, "Claude's command list", |cx| {
        card(&composer, cx)["slashCommands"]
            .as_array()
            .is_some_and(|names| names.iter().any(|name| name == "review"))
    })
    .await?;
    let slash = card(&composer, cx);
    let goal_entries = slash["slashCommands"]
        .as_array()
        .map(|names| names.iter().filter(|name| *name == "goal").count())
        .unwrap_or_default();
    let skill_in_commands = slash["slashCommands"]
        .as_array()
        .is_some_and(|names| names.iter().any(|name| name == "release-notes"));
    // `$` opens the same popup on skills only; accepting writes `$name`, and
    // Claude still receives `/name`. Prose that starts with `$` stays prose.
    input_edit(&composer, window, cx, |input, window, cx| {
        let end = input.text().encode_utf16().count();
        input.replace_text_in_range(Some(0..end), "$rel", window, cx)
    })?;
    draw(window, cx)?;
    let skills = card(&composer, cx);
    key_press(window, cx, "enter")?;
    let accepted = card(&composer, cx);
    let wire = cx.update(|cx| {
        let this = composer.read(cx);
        [
            this.skill_wire_text("$release-notes for 1.2".into(), cx),
            this.skill_wire_text("$HOME is unset".into(), cx),
            this.skill_wire_text("$5 off".into(), cx),
        ]
    });
    input_edit(&composer, window, cx, |input, window, cx| {
        let end = input.text().encode_utf16().count();
        input.replace_text_in_range(Some(0..end), "$5", window, cx)
    })?;
    let money = card(&composer, cx);
    anyhow::ensure!(
        !skill_in_commands
            && skills["slashOpen"] == true
            && skills["slashCommands"] == json!(["release-notes"])
            && accepted["input"] == "$release-notes "
            && accepted["slashOpen"] == false
            && wire == ["/release-notes for 1.2", "$HOME is unset", "$5 off"]
            && money["slashOpen"] == false,
        "skills and commands are not split by `$` and `/`: {slash} {skills} {accepted} {wire:?} {money}"
    );
    // E23: the Claude chat's discovery is scoped to it and its mode.
    let claude_scope = rpc
        .calls
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(method, _)| method == methods::LIST_COMMANDS)
        .map(|(_, params)| params.clone())
        .unwrap_or_default();
    anyhow::ensure!(
        claude_scope["chatId"] == CLAUDE_CHAT
            && claude_scope["harness"] == "claude-code"
            && claude_scope["permissionMode"].is_string(),
        "Claude ListCommands was not scoped to the chat and mode: {claude_scope}"
    );
    // Clear the whole `/go` draft (a `None` range is only the caret).
    input_edit(&composer, window, cx, |input, window, cx| {
        let end = input.text().encode_utf16().count();
        input.replace_text_in_range(Some(0..end), "", window, cx)
    })?;
    anyhow::ensure!(
        goal_entries == 1,
        "Claude `/goal` must be offered once: {slash}"
    );
    steps.push(json!({"step": "claude-goal-mode", "tabWalk": walk, "card": on, "slash": slash, "skills": skills, "accepted": accepted, "wire": wire, "listCommands": claude_scope}));

    // 13. Send in goal mode: ONE run carrying the goal for the Claude chat
    // (the harness turns it into Claude's own `/goal <condition>` line).
    let before = rpc.executes().len();
    focus_input(&composer, window, cx)?;
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_text_in_range(None, CLAUDE_CONDITION, window, cx)
    })?;
    key_press(window, cx, "enter")?;
    wait_for(cx, "the Claude goal send", |_| {
        rpc.executes().len() == before + 1
    })
    .await?;
    let executes = rpc.executes();
    let run = sent(&executes, before)?;
    let request = &run["intent"]["command"]["request"];
    let claude_message = run["commandId"].as_str().unwrap_or_default().to_owned();
    anyhow::ensure!(
        run["intent"]["kind"] == "session"
            && run["taskId"] == CLAUDE_CHAT
            && request["goal"]["objective"] == CLAUDE_CONDITION
            && request["goal"].get("tokenBudget").is_none(),
        "Claude goal send wrong: {run}"
    );
    rpc.gate.add_permits(1);
    wait_for(cx, "the Claude goal send to settle", |cx| {
        cx.update(|cx| !composer.read(cx).sending)
    })
    .await?;
    steps.push(json!({"step": "claude-goal-sent", "intent": run["intent"]}));

    // 14. Claude working toward it: checks and the checker's reason; no Edit
    // or Pause (Claude has neither); Clear waits for the turn boundary.
    let mut claude_marks = luna_proto::GoalMarks::default();
    claude_marks.mark_sent(&claude_message);
    let claude_turn = "claude-goal-turn";
    let claude_transcript = vec![
        entry(&claude_message, MessageRole::User, CLAUDE_CONDITION, false),
        entry(
            claude_turn,
            MessageRole::Assistant,
            "Checking the project, then creating DONE.txt\u{2026}",
            true,
        ),
    ];
    claude_state(
        &state,
        window,
        cx,
        claude_transcript.clone(),
        Some(claude_goal(52, 2, Some("DONE.txt does not exist yet"))),
        claude_marks.clone(),
        Vec::new(),
        true,
    )?;
    shots.push(shot(window, cx, output, "10-claude-pursuing.png").await?);
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    let summary = node_starting(&nodes, "Pursuing goal, ")
        .and_then(|node| node["label"].as_str())
        .unwrap_or_default()
        .to_owned();
    let checker = tree["nodes"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, node)| node["aria"]["label"].as_str())
        .find(|label| label.starts_with("Last check:"))
        .map(str::to_owned);
    let clear_waits = tree["nodes"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, node)| &node["aria"])
        .find(|aria| aria["label"] == CLAUDE_CLEAR_BETWEEN_TURNS)
        .cloned();
    anyhow::ensure!(
        summary.starts_with(&format!("Pursuing goal, {CLAUDE_CONDITION}, "))
            && summary.ends_with(", 2 checks")
            && node(&nodes, "Edit goal").is_none()
            && node(&nodes, "Pause goal").is_none()
            && checker.as_deref() == Some("Last check: DONE.txt does not exist yet")
            && clear_waits
                .as_ref()
                .is_some_and(|aria| !has_action(aria, "Click")),
        "Claude pursuing semantics wrong: summary={summary} checker={checker:?} clear={clear_waits:?} nodes={nodes:?}"
    );
    steps.push(json!({"step": "claude-pursuing", "summary": summary, "checker": checker, "clearWhileWorking": clear_waits, "goalNodes": nodes}));

    // 15. The loop ended with the goal still active (headless Claude waits
    // for the next message), and Claude's own schedules are listed with their
    // real lifetime (a cancelled one is hidden). Clear is now available.
    let schedules = vec![
        schedule(
            "a1b2c3d4",
            luna_proto::NativeScheduleKind::Cron,
            "every 5 minutes",
            "check the deploy",
            true,
            luna_proto::NativeScheduleState::Scheduled,
        ),
        schedule(
            "wakeup",
            luna_proto::NativeScheduleKind::Wakeup,
            "in 10m \u{2014} CI takes about ten minutes",
            "/loop check CI",
            false,
            luna_proto::NativeScheduleState::Scheduled,
        ),
        schedule(
            "e5f6a7b8",
            luna_proto::NativeScheduleKind::Cron,
            "at 15:30 on Oct 1",
            "push the release branch",
            false,
            luna_proto::NativeScheduleState::Cancelled,
        ),
    ];
    let idle_transcript = vec![
        entry(&claude_message, MessageRole::User, CLAUDE_CONDITION, false),
        entry(
            claude_turn,
            MessageRole::Assistant,
            "The build is still running; I'll check again.",
            false,
        ),
    ];
    claude_state(
        &state,
        window,
        cx,
        idle_transcript.clone(),
        Some(claude_goal(96, 3, Some("The build has not finished"))),
        claude_marks.clone(),
        schedules.clone(),
        false,
    )?;
    shots.push(shot(window, cx, output, "11-claude-waiting-schedules.png").await?);
    let tree = a11y_tree(window, cx)?;
    let labels: Vec<String> = tree["nodes"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, node)| node["aria"]["label"].as_str().map(str::to_owned))
        .collect();
    let group = tree["nodes"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, node)| &node["aria"])
        .find(|aria| aria["label"] == "2 prompts scheduled by Claude")
        .cloned();
    anyhow::ensure!(
        labels.iter().any(|l| l == "Last check: The build has not finished \u{b7} Claude checks again after your next message.")
            && labels.iter().any(|l| l == "every 5 minutes: check the deploy (recurring, session-only)")
            && labels.iter().any(|l| l.starts_with("in 10m"))
            && !labels.iter().any(|l| l.contains("push the release branch"))
            && group.as_ref().is_some_and(|g| g["description"].as_str().is_some_and(|d| d.contains("30 minutes"))),
        "Claude waiting/schedules semantics wrong: {labels:?} {group:?}"
    );
    let walk = reverse_tab_walk(&composer, window, cx, |label| label == "Clear goal")?;
    key_press(window, cx, "space")?;
    wait_for(cx, "the Claude clear", |_| {
        rpc.executes().len() == before + 2
    })
    .await?;
    let executes = rpc.executes();
    let clear = sent(&executes, before + 1)?;
    anyhow::ensure!(
        clear["taskId"] == CLAUDE_CHAT
            && clear["intent"]["kind"] == "goal"
            && clear["intent"]["operation"]["action"] == "clear",
        "Claude Clear dispatched wrong: {clear}"
    );
    rpc.gate.add_permits(1);
    wait_for(cx, "the Claude clear to settle", |cx| {
        cx.update(|cx| composer.read(cx).goal_ui.pending.is_none())
    })
    .await?;
    steps.push(json!({"step": "claude-waiting-clear", "reverseTabWalk": walk, "group": group, "intent": clear["intent"]}));

    // 16. Claude's checker confirmed the condition: the bar is gone, the turn
    // reads "Goal achieved in 41s", Claude's remaining schedule stays listed.
    let mut achieved = claude_marks.clone();
    achieved.mark_achieved(claude_turn, 41);
    let mut complete = claude_goal(41, 2, Some("DONE.txt exists in the project root"));
    complete.status = GoalStatus::Complete;
    let done_transcript = vec![
        entry(&claude_message, MessageRole::User, CLAUDE_CONDITION, false),
        entry(
            claude_turn,
            MessageRole::Assistant,
            "Created DONE.txt.",
            false,
        ),
    ];
    claude_state(
        &state,
        window,
        cx,
        done_transcript,
        Some(complete),
        achieved,
        schedules[..1].to_vec(),
        false,
    )?;
    shots.push(shot(window, cx, output, "12-claude-achieved.png").await?);
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    anyhow::ensure!(
        node_starting(&nodes, "Pursuing goal").is_none()
            && node(&nodes, "Goal achieved in 41s").is_some()
            && node(&nodes, "Sent as goal").is_some(),
        "Claude achieved state wrong: {nodes:?}"
    );
    steps.push(json!({"step": "claude-achieved", "goalNodes": nodes}));

    // 17. Claude's ReportFindings on the shared findings UI: the review
    // lifecycle chip and the findings with their provenance line.
    let mut parts = Vec::new();
    for event in [
        luna_proto::AgentEvent::NativeReview {
            id: "toolu_rf1".into(),
            completed: false,
            text: "Claude code review \u{b7} high effort".into(),
        },
        luna_proto::AgentEvent::NativeReview {
            id: "toolu_rf1".into(),
            completed: true,
            text: CLAUDE_FINDINGS.into(),
        },
    ] {
        luna_doc::fold_event_into_parts(&mut parts, &event);
    }
    let review_entry = SessionMessageEntry {
        id: "claude-review-turn".into(),
        role: MessageRole::Assistant,
        parts,
        created_at: chrono::Utc::now().timestamp_millis(),
        device_id: "local".into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    };
    claude_state(
        &state,
        window,
        cx,
        vec![
            entry(
                "claude-review",
                MessageRole::User,
                "/code-review main...HEAD",
                false,
            ),
            review_entry,
        ],
        None,
        Default::default(),
        Vec::new(),
        false,
    )?;
    shots.push(shot(window, cx, output, "13-claude-review-findings.png").await?);
    let tree = a11y_tree(window, cx)?;
    let text_nodes: Vec<String> = tree["nodes"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, node)| node["aria"]["label"].as_str().map(str::to_owned))
        .filter(|label| {
            label.contains("Review findings")
                || label.contains("Native code review")
                || label.contains("Provenance")
        })
        .collect();
    steps.push(json!({"step": "claude-review-findings", "labels": text_nodes}));

    // 18. A provider without native goals (OpenCode) never shows the bar or
    // the Goal badge, even with a goal readback in state.
    {
        let state = state.clone();
        act(window, cx, move |_, cx| {
            state.update(cx, |state, cx| {
                state.selected_chat = Some(OPENCODE_CHAT.into());
                cx.notify();
            })
        })?;
    }
    let tree = a11y_tree(window, cx)?;
    let other_available = cx.update(|cx| composer.read(cx).goal_mode_available(cx));
    let other_nodes = goal_nodes(&tree);
    anyhow::ensure!(
        bar_nodes(&other_nodes).is_empty()
            && node(&other_nodes, "Goal mode").is_none()
            && !other_available,
        "a provider without goals exposes goal UI: {other_nodes:?}"
    );
    steps.push(json!({"step": "opencode-chat", "goalModeAvailable": other_available, "goalNodes": other_nodes}));

    let calls: Vec<Value> = rpc
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == methods::EXECUTE_TASK_COMMAND)
        .map(|(method, params)| json!({"method": method, "intent": params.get("intent")}))
        .collect();
    Ok(json!({
        "fixture": "goal-card",
        "scope": "headless GPUI presentation + dispatch; synthetic engine, no provider proof",
        "shots": shots,
        "steps": steps,
        "rpc": calls,
    }))
}
