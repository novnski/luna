//! Headless GPUI E2E for Codex goals in the composer (`goal-card-fixture`).
//!
//! Scenarios come from `evidence/goal-parity/failure-cases.md`, written before
//! the parity work. The production composer (Goal switch, goal-mode Send,
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

fn snapshot() -> TaskCommandSnapshot {
    TaskCommandSnapshot {
        task_id: CHAT.into(),
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
                snapshot: snapshot(),
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
                    task_id: CHAT.into(),
                    intent: params["intent"]["kind"].as_str().unwrap_or("goal").into(),
                    payload_hash: "fixture".into(),
                    status: CommandReceiptStatus::Applied,
                    snapshot: snapshot(),
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
                SlashCommand {
                    name: "review".into(),
                    description: "Review the current changes".into(),
                    input_hint: None,
                },
                SlashCommand {
                    name: "plan".into(),
                    description: "Plan before editing".into(),
                    input_hint: Some("task".into()),
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

/// Every goal node AccessKit exposes (bar, controls, Goal switch, transcript
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
        .filter(|node| node["label"] != "Goal mode" && node["role"] != "Status")
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

/// Tab forward from the composer input (the Goal switch sits after it).
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
        },
        "memory://goal-card-fixture".into(),
    );
    let chats = vec![
        chat(CHAT, "codex", "Goal parity fixture")?,
        chat(CLAUDE_CHAT, "claude-code", "Claude fixture")?,
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

    // 1. No goal: no bar; the Goal switch sits beside the access chip, off.
    shots.push(shot(window, cx, output, "01-no-goal.png").await?);
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    anyhow::ensure!(
        bar_nodes(&nodes).is_empty()
            && node(&nodes, "Goal mode").is_some_and(|node| node["role"] == "Switch"
                && node["toggled"] == "False"
                && node["click"] == true
                && node["focus"] == true),
        "no-goal state wrong: {nodes:?}"
    );
    steps.push(json!({"step": "no-goal", "goalNodes": nodes, "card": card(&composer, cx)}));

    // 2. Goal mode from the keyboard: Tab to the switch, Space. Codex's
    // placeholder replaces "Do anything…".
    let walk = tab_walk(&composer, window, cx, |label| label == "Goal mode")?;
    key_press(window, cx, "space")?;
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    let on = card(&composer, cx);
    anyhow::ensure!(
        on["goalMode"] == true
            && on["placeholder"] == GOAL_PLACEHOLDER
            && input_placeholder(&tree).as_deref() == Some(GOAL_PLACEHOLDER)
            && node(&nodes, "Goal mode").is_some_and(|node| node["toggled"] == "True"),
        "Space on the Goal switch did not enter goal mode: {on} {nodes:?}"
    );
    shots.push(shot(window, cx, output, "02-goal-mode.png").await?);
    steps.push(json!({"step": "goal-mode-on", "tabWalk": walk, "goalNodes": nodes, "card": on}));

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
            && node(&nodes, "Goal").is_some_and(|node| node["role"] == "Group"),
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

    // 12. A Claude chat never shows the bar or the Goal switch, even with a
    // goal readback in state.
    {
        let state = state.clone();
        act(window, cx, move |_, cx| {
            state.update(cx, |state, cx| {
                state.selected_chat = Some(CLAUDE_CHAT.into());
                cx.notify();
            })
        })?;
    }
    let tree = a11y_tree(window, cx)?;
    let claude_available = cx.update(|cx| composer.read(cx).goal_mode_available(cx));
    let claude_nodes = goal_nodes(&tree);
    anyhow::ensure!(
        bar_nodes(&claude_nodes).is_empty()
            && node(&claude_nodes, "Goal mode").is_none()
            && !claude_available,
        "Claude chat exposes goal UI: {claude_nodes:?}"
    );
    steps.push(json!({"step": "claude-chat", "goalModeAvailable": claude_available, "goalNodes": claude_nodes}));

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
