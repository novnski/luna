//! Headless GPUI E2E for Claude native agents (CC-08 / E25).
//!
//! Drives the REAL `ClaudeHarness` (its fake CLI replays a 2.1.286-shaped
//! native agent graph: a foreground child with a nested child, a nested
//! background child steered by `SendMessage`, a child-owned background job,
//! and two background root children that settle after the root's eager
//! result) through an in-process `EngineCore`, then the production `Shell`:
//! the Agents surface, the child's transcript with its nested agent chips,
//! and the nested child's own transcript. It never contacts a provider,
//! touches LUNA Dev or its data directory, or opens a visible window.
//!
//! Failure cases: `$B/evidence/cc08-native-agents/failure-cases.md`.
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example native-agents-fixture
//! Run:   target/debug/examples/native-agents-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes 01..04 PNG captures, readback.json and native-agents.pass.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowHandle, WindowOptions, px, size};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry, SubagentStatus};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::ClaudeHarness;
use luna_proto::{
    ChatConfig, HarnessId, PermissionMode, RunRequest, SandboxLevel, Session, SessionStatus,
};
use luna_ui::*;

const PROJECT: &str = "native-agents-project";
const TASK: &str = "c8c8c8c8-0000-4000-8000-000000000001";
const PROMPT: &str =
    "Map the parser module and delegate the lexer. [fixture scenario:native-nested]";

fn sub(spawn: &str) -> String {
    format!("{TASK}--sub--{spawn}")
}

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::ClaudeCode,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::ReadOnly,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn capture(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    path: PathBuf,
) -> anyhow::Result<()> {
    // Draw through the untyped handle: a typed update would lease the Shell
    // that the frame is about to render.
    let window = gpui::AnyWindowHandle::from(window);
    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })??;
    Ok(())
}

fn readback(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
) -> anyhow::Result<serde_json::Value> {
    window.update(cx, |shell, _, cx| shell.fixture_agents_readback(cx))
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn root_chip_status(core: &EngineCore, id: &str) -> Option<SubagentStatus> {
    let entries = core.doc_host.open(TASK).ok()?.doc().read_entries().ok()?;
    entries
        .iter()
        .flat_map(|entry| &entry.parts)
        .find_map(|part| match part {
            MessagePart::Tool {
                id: got,
                subagent_status,
                ..
            } if got == id => *subagent_status,
            _ => None,
        })
}

/// Dispatch the native scenario through the engine and wait for the root,
/// its wake turn and every background child to settle.
async fn run_scenario(core: &EngineCore, project: &Path) -> anyhow::Result<()> {
    let request = RunRequest {
        goal: None,
        review: None,
        input_items: Vec::new(),
        prompt: PROMPT.into(),
        harness: None,
        provider_account_id: None,
        model: None,
        reasoning: None,
        model_options: Default::default(),
        cwd: project.display().to_string(),
        sandbox: SandboxLevel::ReadOnly,
        auto_approve: true,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        attachments: vec![],
        worktree: None,
        resume: None,
    };
    core.sessions
        .dispatch(
            TASK,
            HarnessId::ClaudeCode,
            request,
            Some("user-prompt".into()),
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let settled = matches!(
                root_chip_status(core, "toolu_C"),
                Some(SubagentStatus::Interrupted)
            ) && matches!(
                root_chip_status(core, "toolu_D"),
                Some(SubagentStatus::Failed)
            );
            let idle = core
                .sessions
                .session_status(TASK)
                .is_some_and(|status| status.status == SessionStatus::Idle);
            if settled && idle {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("native scenario did not settle"))
}

fn row<'a>(value: &'a serde_json::Value, id: &str) -> anyhow::Result<&'a serde_json::Value> {
    value["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        .ok_or_else(|| anyhow::anyhow!("Agents row {id} missing: {value}"))
}

fn open_doc<'a>(value: &'a serde_json::Value, doc: &str) -> Option<&'a serde_json::Value> {
    value["subagents"]
        .as_array()
        .and_then(|docs| docs.iter().find(|entry| entry["doc"] == doc))
}

fn doc_chip<'a>(doc: &'a serde_json::Value, id: &str) -> anyhow::Result<&'a serde_json::Value> {
    doc["chips"]
        .as_array()
        .and_then(|chips| chips.iter().find(|chip| chip["id"] == id))
        .ok_or_else(|| anyhow::anyhow!("chip {id} missing: {doc}"))
}

fn main() -> anyhow::Result<()> {
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: native-agents-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["readback.json", "native-agents.pass", "failure.txt"] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let root = tempfile::tempdir()?;
    let ui_data = root.path().join("ui");
    let engine_data = root.path().join("engine");
    let project = root.path().join("project");
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(&project)?;
    let fake_cli =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures/fake-claude.sh");
    let runtime = tokio::runtime::Runtime::new()?;
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(ClaudeHarness::new().with_executable(fake_cli)));
    let accounts = AgentAccountsConfig {
        data_dir: engine_data.clone(),
        claude_config_dir: root.path().join("providers/claude"),
        claude_config_file: root.path().join("providers/claude.json"),
        codex_home: root.path().join("providers/codex"),
    };
    std::fs::create_dir_all(root.path().join("providers"))?;
    std::fs::write(
        &accounts.claude_config_file,
        r#"{"oauthAccount":{"accountUuid":"synthetic-native-agents"}}"#,
    )?;
    accounts.disable_claude_keychain_fallback()?;
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(
                &engine_data,
                "native-agents-fixture",
                "native-agents-fixture",
            ),
            Arc::new(registry),
            HarnessId::ClaudeCode,
            None,
            accounts,
        )
    })?;
    let mut auth = AuthConfig::new("", root.path().join("auth"));
    auth.dev_user_id = "native-agents-fixture".into();
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let device = core.device_id.clone();
    core.workspace.create_space(
        PROJECT,
        &device,
        project.to_str().unwrap(),
        Some("Parser".into()),
        true,
    )?;
    // Doc hosts and the run spawn onto the engine's Tokio runtime.
    let chat = runtime.block_on(async {
        core.workspace.create_chat(
            TASK,
            Some(PROJECT),
            Some(&device),
            Some(config()),
            Some(project.to_string_lossy().into_owned()),
        )?;
        core.workspace.rename_chat(TASK, "Map the parser")?;
        core.doc_host
            .open(TASK)?
            .doc()
            .push_message(&SessionMessageEntry {
                id: "user-prompt".into(),
                role: MessageRole::User,
                parts: vec![MessagePart::Text {
                    id: "user-prompt-text".into(),
                    text: PROMPT.into(),
                }],
                created_at: chrono::Utc::now().timestamp_millis(),
                device_id: device.clone(),
                status: Some(MessageStatus::Complete),
                continuation_of: None,
            })?;
        run_scenario(&core, &project).await?;
        anyhow::Ok(core.workspace.chat(TASK)?.expect("fixture chat"))
    })?;
    let spaces = core.workspace.read_spaces()?;
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 59997,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::ClaudeCode,
    };
    let sessions = vec![Session {
        chat_id: TASK.into(),
        device_id: device.clone(),
        status: SessionStatus::Idle,
        started_at: None,
        updated_at: chrono::Utc::now(),
        last_completed_turn: None,
        recovery: None,
    }];
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
                    serde_json::from_value(serde_json::json!({
                        "id": device, "name": "Studio Mac", "platform": "macos", "lastSeenAt": null
                    }))
                    .unwrap(),
                ];
                state.spaces = spaces;
                state.chats = vec![chat];
                state.sessions = sessions;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.auto_selected = true;
                state.connection = state::ConnectionStatus::Ready;
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
                state.fixture_attachment_engine(handle);
                state.select_chat(Some(TASK.into()), cx);
                state
            });
            let window = cx
                .open_window(
                    WindowOptions {
                        window_background: theme::Theme::of(cx).window_background_appearance(),
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(20.), px(20.)),
                            size(px(1440.), px(900.)),
                        ))),
                        show: false,
                        focus: false,
                        ..Default::default()
                    },
                    |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
                )
                .unwrap();
            cx.spawn(async move |cx| {
                let result: anyhow::Result<serde_json::Value> = async {
                    let mut record = serde_json::Map::new();
                    // 1. Root transcript loads; the Agents surface lists the
                    // three ROOT children with provider-reported activity.
                    // The Shell adopts the selected chat asynchronously; a
                    // surface opened before that is a no-op, so retry.
                    let mut agents = serde_json::Value::Null;
                    settle(cx, 900).await;
                    for _ in 0..80 {
                        if agents["rightActive"] != "Agents" {
                            window.update(cx, |shell, _, cx| shell.fixture_open_agents(cx))?;
                        }
                        settle(cx, 100).await;
                        agents = readback(window, cx)?;
                        if agents["total"] == 3 && agents["rightActive"] == "Agents" {
                            break;
                        }
                    }
                    settle(cx, 400).await;
                    capture(window, cx, output.join("01-agents-surface.png"))?;
                    anyhow::ensure!(agents["rightActive"] == "Agents", "Agents surface inactive: {agents}");
                    anyhow::ensure!(agents["total"] == 3, "expected the 3 root agents only: {agents}");
                    let a = row(&agents, "toolu_A")?;
                    anyhow::ensure!(a["label"] == "Done" && a["activity"] == "Parser mapped", "A row: {a}");
                    let metrics = a["metrics"].as_str().unwrap_or_default();
                    for expected in ["claude-haiku-4-5-20251001", "2,400 tokens", "3 tool uses", "9s"] {
                        anyhow::ensure!(metrics.contains(expected), "A metrics lack {expected}: {a}");
                    }
                    anyhow::ensure!(!metrics.contains(" in · "), "task usage shown as an input/output split: {a}");
                    anyhow::ensure!(a["docRef"] == sub("toolu_A"), "A transcript ref: {a}");
                    let c = row(&agents, "toolu_C")?;
                    anyhow::ensure!(
                        c["label"] == "Stopped"
                            && c["activity"] == "Stopped: the Claude worker restarted"
                            && c["metrics"].as_str().is_some_and(|m| m.contains("background")),
                        "C row: {c}"
                    );
                    let d = row(&agents, "toolu_D")?;
                    anyhow::ensure!(d["label"] == "Failed" && d["activity"] == "Docs site unreachable", "D row: {d}");
                    let root_text = agents["rootText"].as_str().unwrap_or_default();
                    for leaked in ["Splitting", "Lexer has", "Token kinds", "Build is green", "ghost", "spec workflow"] {
                        anyhow::ensure!(!root_text.contains(leaked), "{leaked} leaked into the root: {root_text}");
                    }
                    for nested in ["toolu_B", "toolu_E", "toolu_A_bg"] {
                        anyhow::ensure!(
                            !agents["rootChips"].as_array().is_some_and(|chips| chips.iter().any(|chip| chip["id"] == nested)),
                            "nested chip {nested} in the root"
                        );
                    }
                    record.insert("agents".into(), agents);

                    // 2. A's transcript: forwarded text and thinking, nested
                    // agents linked to their own transcripts, and its
                    // background job's completion on the job's chip.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_open_subagent(sub("toolu_A"), "Map the parser".into(), cx)
                    })?;
                    let mut child = serde_json::Value::Null;
                    for _ in 0..80 {
                        settle(cx, 100).await;
                        child = readback(window, cx)?;
                        if open_doc(&child, &sub("toolu_A")).and_then(|doc| doc["rows"].as_u64()).unwrap_or(0) >= 2 {
                            break;
                        }
                    }
                    settle(cx, 500).await;
                    capture(window, cx, output.join("02-child-transcript.png"))?;
                    let a_doc = open_doc(&child, &sub("toolu_A"))
                        .ok_or_else(|| anyhow::anyhow!("A transcript not open: {child}"))?;
                    let a_text = a_doc["text"].as_str().unwrap_or_default();
                    for expected in ["Map the parser module", "The lexer is separable", "Splitting the work.", "Parser mapped: lexer and parser stages."] {
                        anyhow::ensure!(a_text.contains(expected), "A transcript lacks {expected}: {a_doc}");
                    }
                    let b = doc_chip(a_doc, "toolu_B")?;
                    anyhow::ensure!(
                        b["spawn"] == true && b["ref"] == sub("toolu_B") && b["status"] == "done"
                            && b["activity"]["model"] == "claude-sonnet-5",
                        "nested B chip: {b}"
                    );
                    let e = doc_chip(a_doc, "toolu_E")?;
                    anyhow::ensure!(e["ref"] == sub("toolu_E") && e["status"] == "done" && e["activity"]["background"] == true, "nested E chip: {e}");
                    let job = doc_chip(a_doc, "toolu_A_bg")?;
                    anyhow::ensure!(
                        job["spawn"] == false && job["ref"].is_null()
                            && job["output"].as_str().is_some_and(|o| o.contains("Background job completed")),
                        "A's background job chip: {job}"
                    );
                    record.insert("child".into(), child);

                    // 3. The nested child's own transcript.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_open_subagent(sub("toolu_B"), "Read the lexer".into(), cx)
                    })?;
                    let mut nested = serde_json::Value::Null;
                    for _ in 0..80 {
                        settle(cx, 100).await;
                        nested = readback(window, cx)?;
                        if open_doc(&nested, &sub("toolu_B")).and_then(|doc| doc["rows"].as_u64()).unwrap_or(0) >= 2 {
                            break;
                        }
                    }
                    settle(cx, 500).await;
                    capture(window, cx, output.join("03-nested-transcript.png"))?;
                    let b_doc = open_doc(&nested, &sub("toolu_B"))
                        .ok_or_else(|| anyhow::anyhow!("B transcript not open: {nested}"))?;
                    anyhow::ensure!(
                        b_doc["text"].as_str().is_some_and(|t| t.contains("Summarize lexer.rs states.") && t.contains("Lexer has three states.") && !t.contains("Splitting")),
                        "B transcript: {b_doc}"
                    );
                    record.insert("nested".into(), nested);

                    // 4. The nested background child with its steer.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_open_subagent(sub("toolu_E"), "List token kinds".into(), cx)
                    })?;
                    let mut steered = serde_json::Value::Null;
                    for _ in 0..80 {
                        settle(cx, 100).await;
                        steered = readback(window, cx)?;
                        if open_doc(&steered, &sub("toolu_E")).and_then(|doc| doc["rows"].as_u64()).unwrap_or(0) >= 3 {
                            break;
                        }
                    }
                    settle(cx, 500).await;
                    capture(window, cx, output.join("04-nested-steered.png"))?;
                    let e_doc = open_doc(&steered, &sub("toolu_E"))
                        .ok_or_else(|| anyhow::anyhow!("E transcript not open: {steered}"))?;
                    anyhow::ensure!(
                        e_doc["roles"] == serde_json::json!(["User", "User", "Assistant"])
                            && e_doc["text"].as_str().is_some_and(|t| t.contains("Include keywords too.")),
                        "E transcript: {e_doc}"
                    );
                    record.insert("steered".into(), steered);
                    Ok(serde_json::Value::Object(record))
                }
                .await;
                match result {
                    Ok(record) => {
                        let written = std::fs::write(
                            output.join("readback.json"),
                            serde_json::to_vec_pretty(&record).unwrap_or_default(),
                        )
                        .and_then(|_| {
                            std::fs::write(
                                output.join("native-agents.pass"),
                                "PASS: real ClaudeHarness (fake CLI, 2.1.286 frame shapes) -> EngineCore -> production Shell; Agents surface shows reported model/usage/background/outcome for root children only; nested children linked from their owning child's transcript; child background job attributed to its owner; nothing attached to the root\n",
                            )
                        });
                        if let Err(error) = written {
                            *failure_for_ui.lock().unwrap() = Some(error.to_string());
                        }
                    }
                    Err(error) => {
                        eprintln!("Native agents fixture failed: {error:#}");
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                        *failure_for_ui.lock().unwrap() = Some(error.to_string());
                        // AppKit termination from inside `run` exits 0; fail explicitly.
                        std::process::exit(1);
                    }
                }
                let _ = window.update(cx, |_, window, _| window.remove_window());
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    let failed = failure.lock().unwrap().take();
    runtime.block_on(core.shutdown());
    drop(root);
    if let Some(error) = failed {
        // AppKit termination from inside `run` can exit 0; fail explicitly.
        eprintln!("{error}");
        std::process::exit(1);
    }
    Ok(())
}
