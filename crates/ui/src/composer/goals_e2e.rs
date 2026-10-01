//! Headless GPUI E2E for the goal card (`goal-card-fixture`).
//!
//! Scenarios come from `evidence/goal-ui/failure-cases.md`, written before the
//! redesign. The production composer, goal card, `/goal` command, slash popup,
//! key bindings and AccessKit tree run in a hidden window. A synthetic engine
//! answers the task-command path in memory and holds every goal change until
//! the fixture releases it, so pending states are real. Readback is seeded the
//! way durable task state reaches the UI. No LUNA app, provider, account or
//! user data is involved: this proves presentation and dispatch, not Codex.
use super::*;
use gpui::{AnyWindowHandle, AsyncApp, KeyUpEvent, Keystroke, PlatformInput};
use luna_proto::{GoalReadback, TaskCommandRead, TaskCommandSnapshot};
use luna_rpc::{RpcReply, RpcService};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};

const CHAT: &str = "goal-fixture-chat";
const CLAUDE_CHAT: &str = "goal-fixture-claude";
const CREATED: &str = "Get the goal card through design review";
const LONG_OBJECTIVE: &str = "Redesign the goal controls as a compact card stacked on the composer. \
Keep every safety guard: pending changes disable the controls, edits stay bound to their chat, \
and usage comes from provider readback.";

struct FixtureEngine {
    calls: Mutex<Vec<(String, Value)>>,
    /// One permit releases one held goal change.
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
                    intent: "goal".into(),
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
        "config": {"harness": harness, "model": "fixture-model", "sandbox": "workspace-write"},
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
    ThreadGoal {
        thread_id: "fixture-thread".into(),
        objective: objective.into(),
        status,
        token_budget: budget,
        tokens_used: used,
        time_used_seconds: secs,
        created_at: 1,
        updated_at: 2,
    }
}

fn act(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut Window, &mut App),
) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| f(window, cx))
}

fn readback(
    state: &Entity<AppState>,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    goal: Option<ThreadGoal>,
    continuation_ready: bool,
) -> anyhow::Result<()> {
    let state = state.clone();
    act(window, cx, move |_, cx| {
        state.update(cx, |state, cx| {
            state.context_control.goal = Some(GoalReadback {
                thread_id: "fixture-thread".into(),
                binding_generation: 1,
                goal,
                checked_at_ms: chrono::Utc::now().timestamp_millis() - 120_000,
                continuation_ready,
            });
            cx.notify();
        })
    })
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

const GOAL_BUTTONS: &[&str] = &[
    "Clear goal",
    "Edit goal",
    "Pause goal",
    "Resume goal",
    "Mark goal complete",
    "Refresh goal status",
    "Cancel goal edit",
    "Save goal",
    "Set goal",
    "Dismiss goal message",
];

fn has_action(aria: &Value, action: &str) -> bool {
    aria["on_action"]
        .as_array()
        .is_some_and(|actions| actions.iter().any(|value| value == action))
}

/// Every goal node AccessKit exposes, keyed by label.
fn goal_nodes(tree: &Value) -> Vec<Value> {
    let mut nodes: Vec<Value> = tree["nodes"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, node)| &node["aria"])
        .filter(|aria| {
            aria["label"].as_str().is_some_and(|label| {
                label == "Goal" || label.starts_with("Goal, ") || GOAL_BUTTONS.contains(&label)
            })
        })
        .map(|aria| {
            json!({
                "label": aria["label"],
                "role": aria["role"],
                "click": has_action(aria, "Click"),
                "focus": has_action(aria, "Focus"),
                "expanded": aria["expanded"],
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

fn toggle(nodes: &[Value]) -> Option<&Value> {
    nodes.iter().find(|node| {
        node["label"]
            .as_str()
            .is_some_and(|label| label.starts_with("Goal, "))
    })
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

fn card(composer: &Entity<Composer>, cx: &mut AsyncApp) -> Value {
    cx.update(|cx| {
        let this = composer.read(cx);
        json!({
            "input": this.input.read(cx).text(),
            "pending": this.goal_ui.pending.as_ref().map(|pending| format!("{:?}", pending.kind)),
            "editor": this.goal_ui.editor.as_ref().map(|editor| json!({
                "chat": editor.chat_id,
                "creating": editor.creating,
                "objective": editor.objective.read(cx).text(),
                "budget": editor.budget.read(cx).text(),
            })),
            "error": this.goal_ui.error.as_ref().map(|(_, error)| error.clone()),
            "expanded": this.goal_ui.expanded.iter().cloned().collect::<Vec<_>>(),
            "slashOpen": this.slash.token.is_some(),
            "slashCommands": this.slash_commands(cx).iter().map(|command| command.name.clone()).collect::<Vec<_>>(),
            "slashFiltered": this.slash.filtered.len(),
        })
    })
}

/// Drive every goal-card scenario; returns the JSON evidence document and
/// writes the PNG captures into `output`.
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
    // chat proves the card and `/goal` stay Codex-only.
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
        chat(CHAT, "codex", "Goal card fixture")?,
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
    readback(&state, window, cx, None, false)?;

    // 1. No goal: nothing renders below or around the composer.
    shots.push(shot(window, cx, output, "01-no-goal.png").await?);
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    anyhow::ensure!(nodes.is_empty(), "no-goal state exposes goal UI: {nodes:?}");
    steps.push(json!({"step": "no-goal", "goalNodes": nodes, "card": card(&composer, cx)}));

    // 2. `/goal` alone opens the inline create editor and clears the draft.
    focus_input(&composer, window, cx)?;
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_text_in_range(None, "/goal", window, cx)
    })?;
    // The first Enter accepts the open `/goal` completion, the second sends.
    let completing = card(&composer, cx);
    key_press(window, cx, "enter")?;
    let completed = card(&composer, cx);
    key_press(window, cx, "enter")?;
    let opened = card(&composer, cx);
    steps.push(
        json!({"step": "goal-command-typed", "completing": completing, "completed": completed}),
    );
    anyhow::ensure!(
        opened["editor"]["creating"] == true && opened["input"] == "",
        "/goal did not open the create editor: {opened}"
    );
    let objective_input = cx.update(|cx| {
        composer
            .read(cx)
            .goal_ui
            .editor
            .as_ref()
            .map(|editor| (editor.objective.clone(), editor.budget.clone()))
    });
    let Some((objective_input, budget_input)) = objective_input else {
        anyhow::bail!("create editor has no inputs");
    };
    draw(window, cx)?;
    type_into(
        objective_input.clone(),
        window,
        cx,
        "Keep the goal card polished",
    )?;
    shots.push(shot(window, cx, output, "08-create-editor.png").await?);
    let tree = a11y_tree(window, cx)?;
    let editor_nodes = goal_nodes(&tree);
    let editor_focus = focused_label(&tree);
    anyhow::ensure!(
        node(&editor_nodes, "Set goal").is_some_and(|node| node["click"] == true)
            && node(&editor_nodes, "Cancel goal edit").is_some(),
        "create editor controls missing: {editor_nodes:?}"
    );
    steps.push(json!({"step": "create-editor", "goalNodes": editor_nodes, "focused": editor_focus, "card": card(&composer, cx)}));

    // An invalid budget never dispatches and explains itself in the card.
    type_into(budget_input.clone(), window, cx, "0")?;
    let budget_focus = budget_input.clone();
    act(window, cx, move |window, cx| {
        let focus = budget_focus.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    })?;
    key_press(window, cx, "enter")?;
    pause(cx, 50).await;
    let invalid = card(&composer, cx);
    anyhow::ensure!(
        rpc.executes().is_empty()
            && invalid["error"]
                .as_str()
                .is_some_and(|error| error.contains("positive token budget")),
        "invalid budget dispatched or was silent: {invalid}"
    );
    shots.push(shot(window, cx, output, "09-create-invalid-budget.png").await?);
    steps.push(
        json!({"step": "create-invalid-budget", "card": invalid, "executes": rpc.executes().len()}),
    );

    // Escape cancels; with no goal the card disappears again.
    key_press(window, cx, "escape")?;
    let cancelled = card(&composer, cx);
    let tree = a11y_tree(window, cx)?;
    anyhow::ensure!(
        cancelled["editor"].is_null() && goal_nodes(&tree).is_empty(),
        "Escape did not dismiss the create editor: {cancelled}"
    );
    steps.push(json!({"step": "create-cancelled", "card": cancelled}));

    // 3. The slash popup offers LUNA's `/goal` first on a Codex chat.
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_text_in_range(None, "/", window, cx)
    })?;
    wait_for(cx, "slash commands", |cx| {
        cx.update(|cx| {
            let this = composer.read(cx);
            !this.slash.loading && this.slash.token.is_some()
        })
    })
    .await?;
    let popup = card(&composer, cx);
    anyhow::ensure!(
        popup["slashOpen"] == true
            && popup["slashCommands"][0] == GOAL_COMMAND
            && popup["slashFiltered"].as_u64().unwrap_or_default() >= 1,
        "/goal missing from the Codex slash popup: {popup}"
    );
    shots.push(shot(window, cx, output, "07-create-slash-popup.png").await?);
    steps.push(json!({"step": "slash-popup", "card": popup}));
    input_edit(&composer, window, cx, |_, _, cx| {
        cx.emit(ComposerInputEvent::MentionAccept)
    })?;
    let accepted = cx.update(|cx| composer.read(cx).input.read(cx).text().to_owned());
    anyhow::ensure!(
        accepted.trim_end() == "/goal",
        "accepting /goal produced {accepted:?}"
    );
    let objective = if accepted.ends_with(' ') {
        CREATED.to_owned()
    } else {
        format!(" {CREATED}")
    };
    input_edit(&composer, window, cx, move |input, window, cx| {
        let end = input.text().encode_utf16().count();
        input.replace_text_in_range(Some(end..end), &objective, window, cx)
    })?;
    let command = format!("/goal {CREATED}");
    focus_input(&composer, window, cx)?;
    key_press(window, cx, "enter")?;
    wait_for(cx, "the held create command", |_| rpc.executes().len() == 1).await?;
    let created = rpc.executes()[0].clone();
    let operation = &created["intent"]["operation"];
    anyhow::ensure!(
        created["intent"]["kind"] == "goal"
            && created["taskId"] == CHAT
            && operation["action"] == "set"
            && operation["objective"] == CREATED
            && operation["status"] == "active"
            && operation["tokenBudget"]["mode"] == "unlimited",
        "/goal did not dispatch one goal set: {created}"
    );
    let pending = card(&composer, cx);
    anyhow::ensure!(
        pending["pending"] == "Save" && pending["input"] == command.as_str(),
        "create pending state wrong (text must stay until applied): {pending}"
    );
    shots.push(shot(window, cx, output, "10-create-pending.png").await?);
    // A second Enter while pending must not dispatch again.
    key_press(window, cx, "enter")?;
    pause(cx, 120).await;
    anyhow::ensure!(
        rpc.executes().len() == 1,
        "second Enter dispatched another goal change"
    );
    steps.push(json!({"step": "create-pending", "card": pending, "intent": created["intent"], "executesAfterSecondEnter": rpc.executes().len()}));
    rpc.gate.add_permits(1);
    wait_for(cx, "create applied", |cx| {
        cx.update(|cx| composer.read(cx).goal_ui.pending.is_none())
    })
    .await?;
    let applied = card(&composer, cx);
    anyhow::ensure!(
        applied["input"] == "" && applied["error"].is_null(),
        "applied /goal left the command text or an error: {applied}"
    );
    let sent_messages = rpc
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, params)| {
            method == methods::EXECUTE_TASK_COMMAND && params["intent"]["kind"] != "goal"
        })
        .count();
    anyhow::ensure!(sent_messages == 0, "/goal was also sent as a message");
    readback(
        &state,
        window,
        cx,
        Some(goal(GoalStatus::Active, CREATED, 0, None, 4)),
        true,
    )?;
    shots.push(shot(window, cx, output, "11-create-applied.png").await?);
    steps.push(json!({"step": "create-applied", "card": applied}));

    // 4. Active, collapsed: header only, no chip, Clear reachable.
    readback(
        &state,
        window,
        cx,
        Some(goal(GoalStatus::Active, LONG_OBJECTIVE, 12_345, None, 252)),
        false,
    )?;
    shots.push(shot(window, cx, output, "02-active-collapsed.png").await?);
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    anyhow::ensure!(
        node(&nodes, "Goal").is_some_and(|node| node["role"] == "Group")
            && toggle(&nodes)
                .is_some_and(|node| node["expanded"] == false && node["click"] == true)
            && node(&nodes, "Clear goal")
                .is_some_and(|node| node["click"] == true && node["focus"] == true),
        "collapsed card semantics wrong: {nodes:?}"
    );
    let walk_to_toggle =
        reverse_tab_walk(&composer, window, cx, |label| label.starts_with("Goal, "))?;
    anyhow::ensure!(
        walk_to_toggle.iter().any(|label| label == "Clear goal"),
        "Clear goal is not keyboard reachable: {walk_to_toggle:?}"
    );
    steps.push(
        json!({"step": "active-collapsed", "goalNodes": nodes, "reverseTabWalk": walk_to_toggle}),
    );

    // 5. Expand from the keyboard (Enter on the focused toggle), with budget.
    key_press(window, cx, "enter")?;
    readback(
        &state,
        window,
        cx,
        Some(goal(
            GoalStatus::Active,
            LONG_OBJECTIVE,
            18_240,
            Some(50_000),
            754,
        )),
        false,
    )?;
    shots.push(shot(window, cx, output, "03-active-expanded-budget.png").await?);
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    anyhow::ensure!(
        toggle(&nodes).is_some_and(|node| node["expanded"] == true)
            && [
                "Edit goal",
                "Pause goal",
                "Mark goal complete",
                "Refresh goal status",
                "Clear goal"
            ]
            .iter()
            .all(|label| node(&nodes, label)
                .is_some_and(|node| node["click"] == true && node["focus"] == true)),
        "expanded card semantics wrong: {nodes:?}"
    );
    let walk_expanded =
        reverse_tab_walk(&composer, window, cx, |label| label.starts_with("Goal, "))?;
    steps.push(json!({"step": "active-expanded-budget", "goalNodes": nodes, "reverseTabWalk": walk_expanded}));

    // 6. Edit from the keyboard: the editor replaces the body, prefilled.
    reverse_tab_walk(&composer, window, cx, |label| label == "Edit goal")?;
    key_press(window, cx, "enter")?;
    let editing = card(&composer, cx);
    anyhow::ensure!(
        editing["editor"]["creating"] == false
            && editing["editor"]["objective"] == LONG_OBJECTIVE
            && editing["editor"]["budget"] == "50000",
        "edit editor not prefilled: {editing}"
    );
    shots.push(shot(window, cx, output, "12-edit-editor.png").await?);
    steps.push(json!({"step": "edit-editor", "card": editing}));
    key_press(window, cx, "escape")?;
    anyhow::ensure!(
        card(&composer, cx)["editor"].is_null(),
        "Escape did not close the edit editor"
    );

    // 7. Paused: chip in the collapsed header; Resume in the body.
    composer_do(&composer, window, cx, |this, cx| {
        this.toggle_goal_expanded(cx)
    })?;
    readback(
        &state,
        window,
        cx,
        Some(goal(
            GoalStatus::Paused,
            LONG_OBJECTIVE,
            18_240,
            Some(50_000),
            754,
        )),
        false,
    )?;
    shots.push(shot(window, cx, output, "04-paused-collapsed.png").await?);
    composer_do(&composer, window, cx, |this, cx| {
        this.toggle_goal_expanded(cx)
    })?;
    shots.push(shot(window, cx, output, "04b-paused-expanded.png").await?);
    let tree = a11y_tree(window, cx)?;
    let nodes = goal_nodes(&tree);
    anyhow::ensure!(
        toggle(&nodes).is_some_and(|node| node["label"] == "Goal, paused")
            && node(&nodes, "Resume goal").is_some()
            && node(&nodes, "Pause goal").is_none(),
        "paused semantics wrong: {nodes:?}"
    );
    steps.push(json!({"step": "paused", "goalNodes": nodes}));

    // 8. Budget reached (provider status), expanded with a full meter.
    readback(
        &state,
        window,
        cx,
        Some(goal(
            GoalStatus::BudgetLimited,
            LONG_OBJECTIVE,
            50_112,
            Some(50_000),
            1_984,
        )),
        false,
    )?;
    shots.push(shot(window, cx, output, "05-budget-reached.png").await?);
    let tree = a11y_tree(window, cx)?;
    steps.push(json!({"step": "budget-reached", "goalNodes": goal_nodes(&tree)}));

    // 9. Complete, collapsed.
    composer_do(&composer, window, cx, |this, cx| {
        this.toggle_goal_expanded(cx)
    })?;
    readback(
        &state,
        window,
        cx,
        Some(goal(GoalStatus::Complete, CREATED, 64_008, None, 4_210)),
        false,
    )?;
    shots.push(shot(window, cx, output, "05b-complete-collapsed.png").await?);

    // 10. Stacked with the queue tray: goal above, queue below, composer last.
    let queued: luna_doc::QueuedMessage = serde_json::from_value(json!({
        "id": "goal-fixture-queued",
        "text": "Also tighten the spacing in the header row",
        "issuedBy": "local",
        "issuedAt": 7,
    }))?;
    readback(
        &state,
        window,
        cx,
        Some(goal(
            GoalStatus::Active,
            LONG_OBJECTIVE,
            18_240,
            Some(50_000),
            754,
        )),
        true,
    )?;
    {
        let state = state.clone();
        act(window, cx, move |_, cx| {
            state.update(cx, |state, cx| {
                state.queue = vec![queued];
                cx.notify();
            })
        })?;
    }
    shots.push(shot(window, cx, output, "13-goal-with-queue.png").await?);
    {
        let state = state.clone();
        act(window, cx, move |_, cx| {
            state.update(cx, |state, cx| {
                state.queue.clear();
                cx.notify();
            })
        })?;
    }

    // 11. Pending clear from the keyboard: spinner on X, every control off.
    reverse_tab_walk(&composer, window, cx, |label| label == "Clear goal")?;
    key_press(window, cx, "space")?;
    wait_for(cx, "the held clear command", |_| rpc.executes().len() == 2).await?;
    let clear = rpc.executes()[1].clone();
    anyhow::ensure!(
        clear["intent"]["kind"] == "goal" && clear["intent"]["operation"]["action"] == "clear",
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
    // Neither the keyboard nor `/goal` may start a second change now.
    key_press(window, cx, "enter")?;
    input_edit(&composer, window, cx, |input, window, cx| {
        input.replace_text_in_range(None, "/goal Another objective", window, cx)
    })?;
    focus_input(&composer, window, cx)?;
    key_press(window, cx, "enter")?;
    pause(cx, 120).await;
    anyhow::ensure!(
        rpc.executes().len() == 2,
        "a goal change started while another was pending"
    );
    steps.push(json!({"step": "pending-clear", "goalNodes": nodes, "intent": clear["intent"], "card": card(&composer, cx)}));
    rpc.gate.add_permits(1);
    wait_for(cx, "clear applied", |cx| {
        cx.update(|cx| composer.read(cx).goal_ui.pending.is_none())
    })
    .await?;
    input_edit(&composer, window, cx, |input, _, cx| input.set_text("", cx))?;
    readback(&state, window, cx, None, false)?;
    let tree = a11y_tree(window, cx)?;
    anyhow::ensure!(
        goal_nodes(&tree).is_empty(),
        "cleared goal still renders: {:?}",
        goal_nodes(&tree)
    );
    steps.push(json!({"step": "cleared", "card": card(&composer, cx)}));

    // 12. Light theme.
    act(window, cx, |_, cx| {
        crate::appearance::set_mode(crate::appearance::AppearanceMode::Light, cx)
    })?;
    readback(
        &state,
        window,
        cx,
        Some(goal(
            GoalStatus::Active,
            LONG_OBJECTIVE,
            18_240,
            Some(50_000),
            754,
        )),
        false,
    )?;
    composer_do(&composer, window, cx, |this, cx| {
        this.goal_ui.expanded.insert(CHAT.into());
        cx.notify();
    })?;
    shots.push(shot(window, cx, output, "14-light-expanded.png").await?);
    composer_do(&composer, window, cx, |this, cx| {
        this.toggle_goal_expanded(cx)
    })?;
    readback(
        &state,
        window,
        cx,
        Some(goal(
            GoalStatus::Paused,
            LONG_OBJECTIVE,
            18_240,
            Some(50_000),
            754,
        )),
        false,
    )?;
    shots.push(shot(window, cx, output, "15-light-paused-collapsed.png").await?);

    // 13. A Claude chat never shows the card or offers `/goal`, even with a
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
    readback(
        &state,
        window,
        cx,
        Some(goal(GoalStatus::Active, LONG_OBJECTIVE, 1, None, 1)),
        false,
    )?;
    let tree = a11y_tree(window, cx)?;
    let claude = card(&composer, cx);
    let claude_goal = cx.update(|cx| composer.read(cx).goal_command_available(cx));
    anyhow::ensure!(
        goal_nodes(&tree).is_empty() && !claude_goal,
        "Claude chat exposes goal UI: {claude}"
    );
    steps.push(json!({"step": "claude-chat", "goalCommandAvailable": claude_goal, "goalNodes": goal_nodes(&tree)}));

    let calls: Vec<Value> = rpc
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(method, params)| {
            json!({
                "method": method,
                "intent": params.get("intent"),
            })
        })
        .collect();
    Ok(json!({
        "fixture": "goal-card",
        "scope": "headless GPUI presentation + dispatch; synthetic engine, no provider proof",
        "shots": shots,
        "steps": steps,
        "rpc": calls,
    }))
}
