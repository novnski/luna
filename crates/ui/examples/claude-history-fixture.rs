//! Headless GPUI E2E for CC-10 Claude history in the shared rewind dialog.
//!
//! Drives the production `Shell` in a hidden window against an in-process
//! `EngineCore` whose Claude provider is the production `ClaudeHarness`
//! pointed at the deterministic 2.1.286 peer
//! (`crates/harness/tests/fixtures/fake-claude-history.py`). Two real turns
//! edit `notes.txt` through the engine; the dialog then previews and restores
//! Claude's own file checkpoint and rewinds the same Claude session. Native
//! transcripts live under a temporary `.fake-claude-config`; no real Claude
//! configuration, model, network, LUNA Dev or visible window is involved.
//!
//! Failure cases: `$B/evidence/cc10-history/failure-cases.md`.
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example claude-history-fixture
//! Run:   target/debug/examples/claude-history-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes 01..04 PNG captures, readback.json and claude-history.pass.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowHandle, WindowOptions, px, size};
use luna_doc::{MessageRole, MessageStatus};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::{ClaudeHarness, Harness, NativeHistoryScope};
use luna_proto::{
    ChatConfig, CommandReceiptStatus, HarnessId, PermissionMode, ReasoningLevel, RunRequest,
    SandboxLevel, TaskCommandRead,
};
use luna_rpc::methods;
use luna_ui::*;

const PROJECT: &str = "cc10-ui-project";
const TASK: &str = "c10c10c1-0000-4000-8000-000000000001";

fn fake() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/fake-claude-history.py");
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path
}

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::ClaudeCode,
        provider_account_id: None,
        model: None,
        reasoning: Some(ReasoningLevel::Medium),
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn run_request(workspace: &Path, prompt: &str) -> RunRequest {
    RunRequest {
        review: None,
        input_items: Vec::new(),
        prompt: prompt.into(),
        harness: Some(HarnessId::ClaudeCode),
        provider_account_id: None,
        model: None,
        reasoning: Some(ReasoningLevel::Medium),
        model_options: Default::default(),
        cwd: workspace.to_string_lossy().into_owned(),
        sandbox: SandboxLevel::WorkspaceWrite,
        auto_approve: false,
        permission_mode: PermissionMode::Ask,
        parent_permission_ceiling: None,
        environment_permission_ceiling: None,
        permission_policy: None,
        resume: None,
        attachments: Vec::new(),
        worktree: None,
    }
}

/// Send one real Claude turn through the engine and wait for it to settle.
async fn send(
    core: &EngineCore,
    client: &luna_rpc::RpcClient,
    workspace: &Path,
    message_id: &str,
    prompt: &str,
) -> anyhow::Result<()> {
    let read: TaskCommandRead = client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": TASK }),
        )
        .await?;
    let mut command = serde_json::json!({
        "commandId": format!("cc10-ui-{message_id}"), "actorId": "fixture",
        "clientId": "fixture", "clientSessionId": "fixture", "taskId": TASK,
        "expectedTaskRevision": read.snapshot.task_revision,
        "intent": { "kind": "session", "command": {
            "kind": "run", "messageId": message_id, "request": run_request(workspace, prompt),
        } },
    });
    if let Some(generation) = read.snapshot.binding_generation {
        command["expectedBindingGeneration"] = generation.into();
    }
    let receipt: luna_proto::CommandReceipt = client
        .call_as(methods::EXECUTE_TASK_COMMAND, command)
        .await?;
    anyhow::ensure!(
        matches!(
            receipt.status,
            CommandReceiptStatus::Pending | CommandReceiptStatus::Applied
        ),
        "turn {message_id} rejected: {receipt:?}"
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let entries = core.doc_host.open(TASK)?.doc().read_entries()?;
            let answered = entries
                .iter()
                .skip_while(|entry| entry.id != message_id)
                .any(|entry| {
                    entry.role == MessageRole::Assistant
                        && entry.status == Some(MessageStatus::Complete)
                });
            let read: TaskCommandRead = client
                .call_as(
                    methods::GET_TASK_COMMAND_SNAPSHOT,
                    serde_json::json!({ "taskId": TASK }),
                )
                .await?;
            if answered && read.snapshot.active_turn_id.is_none() {
                return anyhow::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| {
        let home = workspace.join(".fake-claude-config");
        anyhow::anyhow!(
            "turn {message_id} did not settle; sessions: {:?}; fake argv: {}; fake errors: {}",
            core.workspace.read_sessions().ok(),
            std::fs::read_to_string(home.join("fake-claude-argv.jsonl")).unwrap_or_default(),
            std::fs::read_to_string(home.join("fake-claude-error.log")).unwrap_or_default(),
        )
    })?
}

fn capture(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
    path: PathBuf,
) -> anyhow::Result<()> {
    let window = gpui::AnyWindowHandle::from(window);
    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })??;
    Ok(())
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

async fn wait_for(
    cx: &mut AsyncApp,
    what: &str,
    mut done: impl FnMut(&mut AsyncApp) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    for _ in 0..200 {
        if done(cx)? {
            return Ok(());
        }
        settle(cx, 50).await;
    }
    anyhow::bail!("timed out waiting for {what}")
}

fn native_turns(workspace: &Path, session: &str) -> anyhow::Result<Vec<String>> {
    let harness = ClaudeHarness::new().with_executable(fake());
    let scope = NativeHistoryScope {
        session_id: session.into(),
        cwd: workspace.to_string_lossy().into_owned(),
        provider_home: Some(
            workspace
                .join(".fake-claude-config")
                .to_string_lossy()
                .into_owned(),
        ),
    };
    Ok(futures::executor::block_on(harness.native_history_turns(scope))?.turn_starts)
}

fn main() -> anyhow::Result<()> {
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: claude-history-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["readback.json", "claude-history.pass", "failure.txt"] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let root = tempfile::tempdir()?;
    let ui_data = root.path().join("ui");
    let engine_data = root.path().join("engine");
    let workspace = root.path().join("project");
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(&workspace)?;
    let workspace = workspace.canonicalize()?;
    std::fs::write(workspace.join("notes.txt"), "alpha\n")?;
    // The engine's model cache builds its own `ClaudeHarness::new()` outside
    // the registry; pin every Claude launch to the fake and its config.
    unsafe {
        std::env::set_var("CLAUDE_CODE_EXECUTABLE", fake());
        std::env::set_var("CLAUDE_CONFIG_DIR", workspace.join(".fake-claude-config"));
    }
    let identity = root.path().join("claude.json");
    std::fs::write(
        &identity,
        r#"{"oauthAccount":{"accountUuid":"cc10-ui-fixture","emailAddress":"cc10@example.test"}}"#,
    )?;
    let runtime = tokio::runtime::Runtime::new()?;
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(ClaudeHarness::new().with_executable(fake())));
    let accounts = AgentAccountsConfig {
        data_dir: engine_data.join("provider-state"),
        claude_config_dir: workspace.join(".fake-claude-config"),
        claude_config_file: identity,
        codex_home: root.path().join("providers/codex"),
    };
    accounts.disable_claude_keychain_fallback()?;
    std::fs::write(
        workspace.join(".fake-claude-config/.credentials.json"),
        r#"{"claudeAiOauth":{"accessToken":"synthetic"}}"#,
    )?;
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&engine_data, "cc10-ui-fixture", "cc10-ui-fixture"),
            Arc::new(registry),
            HarnessId::ClaudeCode,
            None,
            accounts,
        )
    })?;
    let mut auth = AuthConfig::new("", root.path().join("auth"));
    auth.dev_user_id = "cc10-ui-fixture".into();
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let device = core.device_id.clone();
    core.workspace.create_space(
        PROJECT,
        &device,
        workspace.to_str().unwrap(),
        Some("Notes".into()),
        true,
    )?;
    runtime.block_on(async {
        core.workspace.create_chat(
            TASK,
            Some(PROJECT),
            Some(&device),
            Some(config()),
            Some(workspace.to_string_lossy().into_owned()),
        )?;
        core.workspace.rename_chat(TASK, "Claude notes edits")?;
        let client = luna_rpc::memory_client(core.rpc_service());
        send(&core, &client, &workspace, "u1", "edit:notes.txt=beta").await?;
        send(&core, &client, &workspace, "u2", "edit:notes.txt=gamma").await?;
        anyhow::Ok(())
    })?;
    let session = core
        .task_identity
        .view(TASK)
        .and_then(|view| view.current_binding)
        .map(|binding| binding.provider_task_id)
        .ok_or_else(|| anyhow::anyhow!("Claude binding did not materialize"))?;
    let turns_before = native_turns(&workspace, &session)?;
    anyhow::ensure!(
        turns_before.len() == 2,
        "seeded native turns: {turns_before:?}"
    );
    let chats = core.workspace.read_chats()?;
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
    let notes = workspace.join("notes.txt");
    let failure = Arc::new(Mutex::new(None::<String>));
    let failure_for_ui = failure.clone();
    let workspace_for_ui = workspace.clone();
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
                state.chats = chats;
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
                            size(px(1280.), px(860.)),
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
                    let transcript_ids = |cx: &mut AsyncApp| {
                        cx.update(|cx| {
                            state
                                .read(cx)
                                .transcript
                                .iter()
                                .map(|entry| entry.id.clone())
                                .collect::<Vec<_>>()
                        })
                    };
                    wait_for(cx, "the Claude transcript", |cx| {
                        Ok(transcript_ids(cx).iter().any(|id| id == "u2"))
                    })
                    .await?;
                    settle(cx, 400).await;

                    // 1. The shared rewind dialog, named for Claude, with the
                    // separate Claude file-checkpoint section.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_open_conversation_rewind(
                            TASK.into(),
                            "u2".into(),
                            "edit:notes.txt=gamma".into(),
                            cx,
                        )
                    })?;
                    wait_for(cx, "the rewind dialog", |cx| {
                        Ok(window.update(cx, |shell, _, _| shell.fixture_native_files_state())?
                            == Some("idle"))
                    })
                    .await?;
                    settle(cx, 300).await;
                    capture(window, cx, output.join("01-claude-rewind-dialog.png"))?;

                    // 2. Dry-run preview: lists the file, touches nothing.
                    window.update(cx, |shell, _, cx| shell.fixture_preview_native_files(cx))?;
                    wait_for(cx, "the file preview", |cx| {
                        let state = window.update(cx, |shell, _, _| shell.fixture_native_files_state())?;
                        anyhow::ensure!(state != Some("failed"), "preview failed");
                        Ok(state == Some("preview"))
                    })
                    .await?;
                    let after_preview = std::fs::read_to_string(&notes)?;
                    anyhow::ensure!(after_preview == "gamma\n", "preview changed files");
                    record.insert("notesAfterPreview".into(), after_preview.into());
                    settle(cx, 200).await;
                    capture(window, cx, output.join("02-claude-file-preview.png"))?;

                    // 3. Restore Claude's checkpoint for this message.
                    window.update(cx, |shell, _, cx| shell.fixture_apply_native_files(cx))?;
                    wait_for(cx, "the file restore", |cx| {
                        let state = window.update(cx, |shell, _, _| shell.fixture_native_files_state())?;
                        anyhow::ensure!(state != Some("failed"), "restore failed");
                        Ok(state == Some("restored"))
                    })
                    .await?;
                    let restored = std::fs::read_to_string(&notes)?;
                    anyhow::ensure!(restored == "beta\n", "restore result: {restored:?}");
                    record.insert("notesAfterRestore".into(), restored.into());
                    settle(cx, 200).await;
                    capture(window, cx, output.join("03-claude-files-restored.png"))?;

                    // 4. Rewind the conversation on the same Claude session.
                    window.update(cx, |shell, _, cx| shell.fixture_submit_conversation_rewind(cx))?;
                    wait_for(cx, "the rewound transcript", |cx| {
                        let open = window.update(cx, |shell, _, _| shell.fixture_rewind_dialog_open())?;
                        Ok(!open && !transcript_ids(cx).iter().any(|id| id == "u2"))
                    })
                    .await?;
                    settle(cx, 400).await;
                    capture(window, cx, output.join("04-after-claude-rewind.png"))?;
                    let turns_after = native_turns(&workspace_for_ui, &session)?;
                    anyhow::ensure!(
                        turns_after.as_slice() == &turns_before[..1],
                        "native turns after rewind: {turns_after:?}"
                    );
                    record.insert("session".into(), session.clone().into());
                    record.insert("nativeTurnsBefore".into(), serde_json::json!(turns_before));
                    record.insert("nativeTurnsAfter".into(), serde_json::json!(turns_after));
                    record.insert("transcriptAfter".into(), serde_json::json!(transcript_ids(cx)));
                    Ok(serde_json::Value::Object(record))
                }
                .await;
                match result {
                    Ok(record) => {
                        let _ = std::fs::write(
                            output.join("readback.json"),
                            serde_json::to_vec_pretty(&record).unwrap_or_default(),
                        );
                        let _ = std::fs::write(
                            output.join("claude-history.pass"),
                            "PASS: headless production Shell + EngineCore + production ClaudeHarness against the deterministic 2.1.286 peer; Claude rewind copy, native file preview/restore and same-session rewind\n",
                        );
                    }
                    Err(error) => {
                        eprintln!("Claude history fixture failed: {error:#}");
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                        *failure_for_ui.lock().unwrap() = Some(error.to_string());
                        let _ = capture(window, cx, output.join("failure.png"));
                    }
                }
                let _ = gpui::AnyWindowHandle::from(window)
                    .update(cx, |_, window, _| window.remove_window());
                if failure_for_ui.lock().unwrap().is_some() {
                    // AppKit termination from inside `run` exits 0; fail explicitly.
                    std::process::exit(1);
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
