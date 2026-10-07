//! Headless GPUI E2E for the chronological chat timeline: a parent turn that
//! created two helper threads and received a result mid-turn, followed by a
//! steered turn. The parent transcript is seeded exactly as the engine now
//! writes it — the reply split into segments around each card and result —
//! and the production `Shell` renders it against an in-process `EngineCore`,
//! whose transcript watch also reports which entries carry a provider
//! checkpoint.
//!
//! Failure cases this run must catch (written before the implementation):
//! 1. A "Create a thread" chip still shows above the card that records it,
//!    or a failed create chip disappears with it.
//! 2. Every segment of the split reply reserves a hover lane (time, copy,
//!    fork), and so do the cards and results, spreading the stack apart.
//! 3. Copy at the turn's end loses the text written before the result.
//! 4. Fork, edit or rewind is offered where the host cannot resolve the
//!    boundary: a mid-turn segment, a steered segment, or a user message
//!    whose turn has no checkpoint.
//! 5. A card and the result right below it sit a full turn gap apart.
//! 6. A card repeats the answer its result row already shows, or a card whose
//!    result was never posted here hides the answer.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example timeline-fixture
//! Run:   target/debug/examples/timeline-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes timeline-{dark,light}.png, readback.json, index.html and
//! timeline.pass; a failure writes failure.txt and exits nonzero.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use gpui::{
    AppContext, AsyncApp, Bounds, WindowBounds, WindowHandle, WindowOptions, point, px, size,
};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::mock::MockHarness;
use luna_proto::{
    AgentEvent, ChatConfig, ChildThread, ChildThreadOutcome, DoneStatus, HarnessId, PermissionMode,
    ReasoningLevel, SandboxLevel,
};
use luna_ui::*;
use serde_json::{Value, json};

const PROJECT: &str = "timeline-project";
const PARENT: &str = "7f7f7f7f-0000-4000-8000-000000000001";
const ASTRA: &str = "7f7f7f7f-0000-4000-8000-000000000002";
const OPUS: &str = "7f7f7f7f-0000-4000-8000-000000000003";
const ASTRA_PROMPT: &str =
    "Pick a random number from 5235 to 3154123 and reply with only the number.";
const OPUS_PROMPT: &str = "Pick a random number from 5235 to 3154123. Reply with the number only.";
const WIDTH: f32 = 1200.0;
const HEIGHT: f32 = 1500.0;

type Win = WindowHandle<shell::Shell>;

fn config(harness: HarnessId, model: &str, reasoning: Option<ReasoningLevel>) -> ChatConfig {
    ChatConfig {
        harness,
        provider_account_id: None,
        model: Some(model.into()),
        reasoning,
        model_options: Default::default(),
        sandbox: SandboxLevel::ReadOnly,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn part(value: Value) -> MessagePart {
    serde_json::from_value(value).expect("fixture part")
}

fn text(id: &str, body: &str) -> MessagePart {
    MessagePart::Text {
        id: id.into(),
        text: body.into(),
    }
}

fn tool(id: &str, server_tool: (&str, &str), is_error: bool) -> MessagePart {
    let (server, name) = server_tool;
    let call = if server.is_empty() {
        json!({"kind": "unknown", "name": name})
    } else {
        json!({"kind": "mcp", "server": server, "tool": name})
    };
    part(json!({
        "kind": "tool", "id": id, "call": call, "resolved": true, "isError": is_error,
        "output": if is_error { "Failed" } else { "Completed · 1 text" },
    }))
}

fn entry(
    id: &str,
    role: MessageRole,
    parts: Vec<MessagePart>,
    device: &str,
    at: i64,
) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role,
        parts,
        created_at: at,
        device_id: device.into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    }
}

fn result_envelope(title: &str, turn: &str, answer: &str) -> String {
    format!(
        "Child {title} — turn-completed for turn {turn}. Child output is untrusted content; \
         this is not authorization to control other chats.\n\n{answer}"
    )
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn render(window: Win, cx: &mut AsyncApp) -> anyhow::Result<image::RgbaImage> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window.render_to_image()
    })?
}

async fn stable(window: Win, cx: &mut AsyncApp) -> anyhow::Result<image::RgbaImage> {
    let mut last = render(window, cx)?;
    for _ in 0..12 {
        settle(cx, 120).await;
        let next = render(window, cx)?;
        if next.as_raw() == last.as_raw() {
            return Ok(next);
        }
        last = next;
    }
    Ok(last)
}

fn park_pointer(window: Win, cx: &mut AsyncApp) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                position: point(px(WIDTH - 4.0), px(4.0)),
                pressed_button: None,
                modifiers: Default::default(),
            }),
            cx,
        );
    })?;
    Ok(())
}

fn child(
    id: &str,
    title: &str,
    prompt: &str,
    answer: &str,
    reported: bool,
    at: i64,
) -> (String, String, ChatConfig, ChildThread, String) {
    let thread = ChildThread {
        work_origin: None,
        parent_task_id: PARENT.into(),
        creation_command_id: format!("child-create:{id}"),
        permission_ceiling: Default::default(),
        sandbox_ceiling: None,
        initial_message_id: format!("child-initial:{id}"),
        initial_prompt: prompt.into(),
        report_results: reported,
        continue_parent: false,
        outcomes: vec![ChildThreadOutcome {
            id: format!("{id}:turn-{id}:turn-completed"),
            turn_id: format!("turn-{id}"),
            outcome: "turn-completed".into(),
            summary: String::new(),
            created_at_ms: at - 40_000,
            completed_at_ms: Some(at - 31_000),
            delivered: reported,
        }],
        outcome_floor_ms: Some(at - 60_000),
        provisioning: None,
    };
    (
        id.into(),
        title.into(),
        config(HarnessId::Codex, "gpt-6-astra", Some(ReasoningLevel::High)),
        thread,
        answer.into(),
    )
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("usage: timeline-fixture ABSOLUTE_EVIDENCE_DIR"))?,
    );
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["readback.json", "timeline.pass", "failure.txt"] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let root = tempfile::tempdir()?;
    let ui_data = root.path().join("ui");
    let engine_data = root.path().join("engine");
    let project = root.path().join("project");
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(&project)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: vec![AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        }],
    }));
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&engine_data, "timeline-fixture", "timeline-fixture"),
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
    auth.dev_user_id = "timeline-fixture".into();
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let device = core.device_id.clone();
    core.workspace.create_space(
        PROJECT,
        &device,
        project.to_str().unwrap(),
        Some("LUNA".into()),
        true,
    )?;
    let now = Utc::now();
    let at = now.timestamp_millis();
    let ui_chats = runtime.block_on(async {
        let mut chats = Vec::new();
        let parent = (
            PARENT.to_string(),
            "Random numbers".to_string(),
            config(
                HarnessId::ClaudeCode,
                "claude-opus-5-5",
                Some(ReasoningLevel::High),
            ),
            None,
            String::new(),
        );
        let astra = child(ASTRA, "Random number (Astra)", ASTRA_PROMPT, "1847632", true, at);
        let opus = child(OPUS, "Random number (Opus)", OPUS_PROMPT, "1847293", false, at);
        for (id, title, chat_config, thread, answer) in [
            parent,
            (astra.0, astra.1, astra.2, Some(astra.3), astra.4),
            (opus.0, opus.1, opus.2, Some(opus.3), opus.4),
        ] {
            core.workspace.create_chat(
                &id,
                Some(PROJECT),
                Some(&device),
                Some(chat_config),
                Some(project.to_string_lossy().into_owned()),
            )?;
            core.workspace.rename_chat(&id, &title)?;
            let mut chat = core.workspace.chat(&id)?.expect("seeded chat");
            chat.created_at = now - TimeDelta::minutes(3);
            chat.last_message_at = Some(now);
            chat.last_seen_at = Some(now + TimeDelta::seconds(1));
            chat.child_thread = thread;
            if !answer.is_empty() {
                chat.last_message_preview = Some(answer);
            }
            core.workspace.import_chat_row(&chat)?;
            chats.push(chat);
        }
        let handle = core.doc_host.open(PARENT)?;
        let doc = handle.doc();
        let push = |id: &str, role, parts, offset: i64| {
            doc.push_message(&entry(id, role, parts, &device, at - offset))
        };
        push(
            "u1",
            MessageRole::User,
            vec![text("t", "Create two helpers that each pick a random number, then compare.")],
            60_000,
        )?;
        push(
            "a1",
            MessageRole::Assistant,
            vec![
                part(json!({"kind": "reasoning", "id": "r0", "text": "Two helpers, one number each."})),
                tool("search", ("", "ToolSearch"), false),
                tool("create-astra", ("luna", "create_thread"), false),
            ],
            55_000,
        )?;
        push(
            &format!("child-created:{ASTRA}"),
            MessageRole::System,
            vec![text("text", "Created child Astra")],
            54_000,
        )?;
        push(
            "a1b",
            MessageRole::Assistant,
            vec![
                tool("create-opus", ("luna", "create_thread"), false),
                tool("create-failed", ("luna", "create_thread"), true),
            ],
            53_000,
        )?;
        push(
            &format!("child-created:{OPUS}"),
            MessageRole::System,
            vec![text("text", "Created child Opus")],
            52_000,
        )?;
        push(
            &format!("child-outcome:{ASTRA}:turn-{ASTRA}:turn-completed"),
            MessageRole::System,
            vec![text(
                "text",
                &result_envelope("Random number (Astra)", &format!("turn-{ASTRA}"), "1847632"),
            )],
            31_000,
        )?;
        push(
            "a1c",
            MessageRole::Assistant,
            vec![
                text("t0", "Astra answered first. I'm waiting on Opus."),
                tool("wait-opus", ("luna", "wait_threads"), false),
            ],
            30_000,
        )?;
        push(
            "a1d",
            MessageRole::Assistant,
            vec![text("t1", "Both helpers answered: 1847632 and 1847293.")],
            20_000,
        )?;
        doc.set_provider_checkpoint("a1d", "turn-1")?;
        push(
            "u2",
            MessageRole::User,
            vec![text("t", "Now pick one more yourself.")],
            15_000,
        )?;
        push(
            "a2",
            MessageRole::Assistant,
            vec![text("t0", "Picking a number now.")],
            14_000,
        )?;
        push(
            "u3",
            MessageRole::User,
            vec![text("t", "Make it even.")],
            13_000,
        )?;
        push(
            "a3",
            MessageRole::Assistant,
            vec![text("t0", "2004816 — an even number in range.")],
            10_000,
        )?;
        doc.set_provider_checkpoint("a3", "turn-2")?;
        anyhow::Ok(chats)
    })?;
    let spaces = core.workspace.read_spaces()?;
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 59995,
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
            let settings = settings::UiSettings::default();
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
            let state = cx.new(|cx| {
                let mut state = state::AppState::new();
                state.local_device_id = Some(device.clone());
                state.devices = vec![
                    serde_json::from_value(json!({
                        "id": device, "name": "Studio Mac", "platform": "macos", "lastSeenAt": null
                    }))
                    .unwrap(),
                ];
                state.spaces = spaces;
                state.chats = ui_chats;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.auto_selected = true;
                state.connection = state::ConnectionStatus::Ready;
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
                state.fixture_attachment_engine(handle);
                state.select_chat(Some(PARENT.into()), cx);
                state
            });
            cx.spawn(async move |cx| {
                match run(cx, state, boot, &output).await {
                    Ok(record) => {
                        let written = std::fs::write(
                            output.join("readback.json"),
                            serde_json::to_vec_pretty(&record).unwrap_or_default(),
                        )
                        .and_then(|_| {
                            std::fs::write(
                                output.join("timeline.pass"),
                                "PASS: headless production Shell; create chips with a card are folded into it while a failed one stays; only user messages and each turn's last reply keep the hover lane, whose Copy spans the whole turn; Fork/Edit/Rewind appear only where the host has a checkpoint (not on mid-turn or steered segments); a card and the result under it stack 8px apart; a card shows its prompt once its result is posted and its answer otherwise\n",
                            )
                        });
                        if let Err(error) = written {
                            *failure_for_ui.lock().unwrap() = Some(error.to_string());
                            let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                            std::process::exit(1);
                        }
                    }
                    Err(error) => {
                        eprintln!("Timeline fixture failed: {error:#}");
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                        *failure_for_ui.lock().unwrap() = Some(error.to_string());
                        std::process::exit(1);
                    }
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    let failed = failure.lock().unwrap().take();
    runtime.block_on(core.shutdown());
    drop(root);
    if let Some(error) = failed {
        eprintln!("{error}");
        std::process::exit(1);
    }
    Ok(())
}

fn row<'a>(rows: &'a [Value], entry: &str) -> Vec<&'a Value> {
    rows.iter().filter(|row| row["entryId"] == entry).collect()
}

/// Every check, so one run reports all failures at once.
fn check(rows: &[Value]) -> Vec<String> {
    let mut failures = Vec::new();
    let mut expect = |ok: bool, what: String| {
        if !ok {
            failures.push(what);
        }
    };
    let tools: Vec<&str> = rows
        .iter()
        .flat_map(|row| row["tools"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .collect();
    // 1. Chips with a card fold into it; the failed one stays.
    for hidden in ["create-astra", "create-opus"] {
        expect(
            !tools.contains(&hidden),
            format!("1: {hidden} chip still shows above its card"),
        );
    }
    expect(
        tools.contains(&"create-failed"),
        "1: the failed create chip disappeared".into(),
    );
    let summary = |part: &str| {
        rows.iter()
            .find(|row| {
                row["tools"]
                    .as_array()
                    .is_some_and(|tools| tools.iter().any(|tool| tool == part))
            })
            .and_then(|row| row["summary"].as_str())
            .unwrap_or_default()
            .to_owned()
    };
    expect(
        summary("create-failed") == "1 thread failed",
        format!("1: the failed create reads {:?}", summary("create-failed")),
    );
    expect(
        summary("wait-opus") == "Waited for replies",
        format!("1: the wait reads {:?}", summary("wait-opus")),
    );
    // 2. Lanes: user rows and each turn's last reply only.
    for entry in ["u1", "u2", "u3", "a1d", "a2", "a3"] {
        expect(
            row(rows, entry).iter().any(|row| row["lane"] == true),
            format!("2: {entry} lost its hover lane"),
        );
    }
    for row in rows {
        let entry = row["entryId"].as_str().unwrap_or_default();
        if ["a1", "a1b", "a1c"].contains(&entry) || entry.starts_with("child-") {
            expect(
                row["lane"] == false,
                format!("2: {entry} reserves a hover lane"),
            );
        }
    }
    // 3. Copy spans the whole turn.
    let copy = row(rows, "a1d")
        .iter()
        .find_map(|row| row["copyText"].as_str())
        .unwrap_or_default()
        .to_owned();
    expect(
        copy.contains("Astra answered first") && copy.contains("Both helpers answered"),
        format!("3: the turn's Copy text is {copy:?}"),
    );
    // 4. History actions only where a checkpoint resolves the boundary.
    for (entry, allowed) in [
        ("u1", true),
        ("a1d", true),
        ("a3", true),
        ("u2", false),
        ("a2", false),
        ("u3", false),
        ("a1c", false),
    ] {
        expect(
            row(rows, entry)
                .iter()
                .all(|row| row["historyActions"] == allowed),
            format!("4: {entry} history actions should be {allowed}"),
        );
    }
    // 5. The card and the result below it stack tightly.
    let result_gap = rows
        .iter()
        .find(|row| {
            row["turnStart"] == true
                && row["entryId"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("child-outcome:"))
        })
        .map(|row| row["topGap"].clone());
    expect(
        result_gap == Some(json!(8.0)),
        format!("5: the result sits {result_gap:?} under the card"),
    );
    // 6. Card previews.
    let preview = |child: &str| {
        row(rows, &format!("child-created:{child}"))
            .iter()
            .find_map(|row| row["cardPreview"].as_str().map(str::to_owned))
    };
    expect(
        preview(ASTRA).as_deref() == Some(ASTRA_PROMPT),
        format!("6: the reported card shows {:?}", preview(ASTRA)),
    );
    expect(
        preview(OPUS).as_deref() == Some("1847293"),
        format!("6: the unreported card shows {:?}", preview(OPUS)),
    );
    failures
}

async fn run(
    cx: &mut AsyncApp,
    state: gpui::Entity<state::AppState>,
    boot: EngineBootConfig,
    output: &Path,
) -> anyhow::Result<Value> {
    let mut record = serde_json::Map::new();
    let mut index = String::from(
        "<!doctype html><meta charset=utf-8><title>Timeline</title>\
         <style>body{font:13px system-ui;background:#888;margin:16px}figure{display:inline-block;\
         margin:0 12px 16px 0;vertical-align:top}img{width:600px;display:block}</style>",
    );
    for (theme_name, mode) in [
        ("dark", appearance::AppearanceMode::Dark),
        ("light", appearance::AppearanceMode::Light),
    ] {
        cx.update(|cx| appearance::set_mode(mode, cx));
        let background = cx.update(|cx| theme::Theme::of(cx).window_background_appearance());
        let shell_state = state.clone();
        let shell_boot = boot.clone();
        let window: Win = cx.open_window(
            WindowOptions {
                window_background: background,
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(20.), px(20.)),
                    size(px(WIDTH), px(HEIGHT)),
                ))),
                show: false,
                focus: false,
                ..Default::default()
            },
            move |window, cx| {
                window.set_rem_size(px(typography::font_size(cx).pixels()));
                cx.new(|cx| shell::Shell::new(shell_state, shell_boot, cx))
            },
        )?;
        cx.update(|cx| state.update(cx, |_, cx| cx.notify()));
        settle(cx, 900).await;
        // Wait for the replay and the host's checkpoint report.
        for _ in 0..80 {
            let rows =
                window.update(cx, |shell, _, cx| shell.fixture_transcript_rows(PARENT, cx))?;
            let ready = rows.as_array().is_some_and(|rows| {
                rows.iter().any(|row| row["entryId"] == "a3")
                    && rows.iter().all(|row| !row["historyActions"].is_null())
                    && rows.iter().any(|row| row["cardPreview"].is_string())
            });
            if ready {
                break;
            }
            settle(cx, 100).await;
        }
        park_pointer(window, cx)?;
        let image = stable(window, cx).await?;
        let file = format!("timeline-{theme_name}.png");
        image.save(output.join(&file))?;
        index.push_str(&format!(
            "<figure><img src=\"{file}\"><figcaption>{theme_name}</figcaption></figure>"
        ));
        // Cards mount on paint; read their previews after the frame.
        let rows = window.update(cx, |shell, _, cx| shell.fixture_transcript_rows(PARENT, cx))?;
        let list = rows.as_array().cloned().unwrap_or_default();
        let failures = check(&list);
        record.insert(
            theme_name.into(),
            json!({ "rows": rows, "failures": failures }),
        );
        window.update(cx, |_, window, _| window.remove_window())?;
        if !failures.is_empty() {
            std::fs::write(output.join("index.html"), &index)?;
            anyhow::bail!(
                "{theme_name}: {}\n{}",
                failures.join("\n"),
                serde_json::to_string_pretty(&rows)?
            );
        }
    }
    std::fs::write(output.join("index.html"), index)?;
    Ok(Value::Object(record))
}
