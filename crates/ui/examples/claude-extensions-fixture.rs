//! Headless GPUI E2E for CC-12 (Claude extensions, hooks and typed skills).
//!
//! Drives the production `Shell` transcript and the Providers-page
//! `NativeExtensionsPanel` against an in-process `EngineCore` whose Claude
//! adapter is the real `ClaudeHarness` pointed at the deterministic fake CLI
//! `crates/harness/tests/fixtures/fake-claude-extensions.sh` (2.1.286 frame
//! shapes) and a synthetic config/managed/project tree under a temp dir. It
//! never contacts Claude, touches LUNA Dev or its data, or opens a visible
//! window.
//!
//! Proves: hook frames → normalizer → engine → doc → Hook chips with
//! outcomes; a typed Claude skill selection queued through the engine reaches
//! the CLI as a native `/name` invocation; the effective-extensions view for
//! Full access and Ask (restricted) launches, verified by the fake's
//! initialize answer.
//!
//! Failure cases: `$B/evidence/cc12-extensions/failure-cases.md`.
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example claude-extensions-fixture
//! Run:   target/debug/examples/claude-extensions-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes 01..03 PNG captures, readback.json and claude-extensions.pass.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    AnyWindowHandle, AppContext, AsyncApp, Bounds, Context, Entity, Render, Window, WindowBounds,
    WindowHandle, WindowOptions, div, prelude::*, px, size,
};
use luna_doc::MessagePart;
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::ClaudeHarness;
use luna_proto::{ChatConfig, HarnessId, PermissionMode, SandboxLevel};
use luna_rpc::methods;
use luna_ui::*;
use serde_json::json;

const PROJECT: &str = "cc12-project";
const CHAT: &str = "c12c12c1-0000-4000-8000-000000000001";
const FINAL_TEXT: &str = "The cleanup was blocked by your project hook.";

fn write(path: &Path, text: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(path, text)?;
    Ok(())
}

/// The layout the isolated 2.1.286 probe used, plus a managed policy hook.
fn seed(config: &Path, managed: &Path, project: &Path) -> anyhow::Result<()> {
    let md = |d: &str| format!("---\ndescription: {d}\n---\nnever run\n");
    let skill = |n: &str, d: &str| format!("---\nname: {n}\ndescription: {d}\n---\nnever run\n");
    let hook = |m: &str| json!({"hooks": [{"type": "command", "command": format!("echo {m}")}]});
    write(
        &config.join("settings.json"),
        &json!({"outputStyle": "user-style", "hooks": {
            "SessionStart": [hook("cc12-user-session-start")],
            "UserPromptSubmit": [hook("cc12-user-prompt")]}})
        .to_string(),
    )?;
    write(
        &config.join("commands/cc12-user-only.md"),
        &md("cc12 user-only command"),
    )?;
    write(
        &config.join("commands/cc12-collide.md"),
        &md("cc12 USER collide"),
    )?;
    write(
        &config.join("skills/cc12-user-skill/SKILL.md"),
        &skill("cc12-user-skill", "cc12 user skill"),
    )?;
    write(
        &config.join("skills/cc12-skill-collide/SKILL.md"),
        &skill("cc12-skill-collide", "cc12 USER skill collide"),
    )?;
    write(
        &config.join("output-styles/user-style.md"),
        "---\nname: user-style\n---\nBe terse.\n",
    )?;
    let claude = project.join(".claude");
    write(
        &claude.join("settings.json"),
        &json!({"outputStyle": "Explanatory", "hooks": {
            "SessionStart": [hook("cc12-project-session-start")],
            "PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/guard.sh"}]}]}})
        .to_string(),
    )?;
    write(
        &claude.join("settings.local.json"),
        &json!({"outputStyle": "project-style"}).to_string(),
    )?;
    write(
        &claude.join("commands/cc12-project-only.md"),
        "---\ndescription: cc12 project-only command\nargument-hint: <thing>\n---\nnever run $ARGUMENTS\n",
    )?;
    write(
        &claude.join("commands/cc12-collide.md"),
        &md("cc12 PROJECT collide"),
    )?;
    write(
        &claude.join("commands/ns/cc12-nested.md"),
        &md("cc12 namespaced command"),
    )?;
    write(
        &claude.join("skills/cc12-project-skill/SKILL.md"),
        &skill("cc12-project-skill", "cc12 project skill"),
    )?;
    write(
        &claude.join("skills/cc12-skill-collide/SKILL.md"),
        &skill("cc12-skill-collide", "cc12 PROJECT skill collide"),
    )?;
    write(
        &claude.join("commands/cc12-dual.md"),
        &md("cc12 dual COMMAND"),
    )?;
    write(
        &claude.join("skills/cc12-dual/SKILL.md"),
        &skill("cc12-dual", "cc12 dual SKILL"),
    )?;
    write(
        &claude.join("output-styles/project-style.md"),
        "---\nname: project-style\n---\nBe precise.\n",
    )?;
    write(
        &managed.join("managed-settings.json"),
        &json!({"hooks": {"Stop": [{"hooks": [
            {"type": "http", "url": "https://hooks.example.org/stop"}]}]}})
        .to_string(),
    )?;
    // The real 2.1.286 initialize answers for this layout (isolated probe).
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../harness/tests/fixtures");
    std::fs::copy(
        fixtures.join("claude/init-2.1.286-all-sources.json"),
        config.join("fake-initialize.json"),
    )?;
    std::fs::copy(
        fixtures.join("claude/init-2.1.286-restricted-ask.json"),
        config.join("fake-initialize-restricted.json"),
    )?;
    Ok(())
}

fn fake_cli() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/fake-claude-extensions.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    path
}

struct PanelRoot {
    panel: Entity<settings::native_extensions::NativeExtensionsPanel>,
}

impl Render for PanelRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(typography::font_size(cx).pixels()));
        let theme = theme::Theme::of(cx);
        div()
            .size_full()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .p(px(24.0))
            .child(self.panel.clone())
    }
}

fn capture(window: AnyWindowHandle, cx: &mut AsyncApp, path: PathBuf) -> anyhow::Result<()> {
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

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::ClaudeCode,
        provider_account_id: None,
        model: Some("claude-fable-5".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Full,
        effective_permission_mode: PermissionMode::Full,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn hook_parts(core: &EngineCore, rt: &tokio::runtime::Handle) -> anyhow::Result<serde_json::Value> {
    let _guard = rt.enter();
    let doc = core.doc_host.open(CHAT)?;
    let entries = doc.doc().read_entries()?;
    let mut tools = Vec::new();
    let mut text = String::new();
    for entry in &entries {
        for part in &entry.parts {
            match part {
                MessagePart::Tool {
                    id,
                    call,
                    is_error,
                    resolved,
                    output,
                    ..
                } => tools.push(json!({
                    "id": id,
                    "chip": luna_proto::view::tool_chip_content(call),
                    "hook": call.native_hook_name(),
                    "isError": is_error,
                    "resolved": resolved,
                    "preview": output,
                })),
                MessagePart::Text { text: t, .. } => text.push_str(t),
                _ => {}
            }
        }
    }
    Ok(json!({"tools": tools, "text": text}))
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
        anyhow::anyhow!("usage: claude-extensions-fixture ABSOLUTE_EVIDENCE_DIR")
    })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["readback.json", "claude-extensions.pass", "failure.txt"] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let root = tempfile::tempdir()?;
    let ui_data = root.path().join("ui");
    let engine_data = root.path().join("engine");
    let config_dir = root.path().join("claude-config");
    let managed_dir = root.path().join("managed");
    let project = root.path().join("project");
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(&project)?;
    seed(&config_dir, &managed_dir, &project)?;
    let fake_log = config_dir.join("fake-log.txt");
    let runtime = tokio::runtime::Runtime::new()?;
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(
        ClaudeHarness::new()
            .with_executable(fake_cli())
            .with_config_dir(&config_dir)
            .with_managed_settings_dir(&managed_dir),
    ));
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&engine_data, "cc12-fixture", "cc12-fixture"),
            Arc::new(registry),
            HarnessId::ClaudeCode,
            None,
            AgentAccountsConfig {
                data_dir: engine_data.clone(),
                claude_config_dir: config_dir.clone(),
                claude_config_file: root.path().join("claude.json"),
                codex_home: root.path().join("codex"),
            },
        )
    })?;
    let mut auth = AuthConfig::new("", root.path().join("auth"));
    auth.dev_user_id = "cc12-fixture".into();
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
    runtime.block_on(async {
        core.workspace.create_chat(
            CHAT,
            Some(PROJECT),
            Some(&device),
            Some(config()),
            Some(project.to_string_lossy().into_owned()),
        )?;
        core.workspace.rename_chat(CHAT, "Clean the build output")?;
        anyhow::Ok(())
    })?;
    let spaces = core.workspace.read_spaces()?;
    let chats = vec![core.workspace.chat(CHAT)?.expect("seeded chat")];
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
    let client = handle.clone();
    let rt = runtime.handle().clone();
    let core = Arc::new(core);
    let core_ui = core.clone();
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 59997,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::ClaudeCode,
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
                state.chats = chats;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.auto_selected = true;
                state.connection = state::ConnectionStatus::Ready;
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
                state.fixture_attachment_engine(handle);
                state.select_chat(Some(CHAT.into()), cx);
                state
            });
            let shell: WindowHandle<shell::Shell> = cx
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
            let panel = cx.new(|cx| {
                settings::native_extensions::NativeExtensionsPanel::new(state.clone(), cx)
            });
            let panel_for_window = panel.clone();
            let panel_window: WindowHandle<PanelRoot> = cx
                .open_window(
                    WindowOptions {
                        window_background: gpui::WindowBackgroundAppearance::Opaque,
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(20.), px(20.)),
                            size(px(980.), px(1700.)),
                        ))),
                        show: false,
                        focus: false,
                        ..Default::default()
                    },
                    |_, cx| cx.new(|_| PanelRoot { panel: panel_for_window }),
                )
                .unwrap();
            cx.spawn(async move |cx| {
                let result: anyhow::Result<serde_json::Value> = async {
                    let mut record = serde_json::Map::new();
                    settle(cx, 900).await;

                    // 1. Hook lifecycle through the real adapter, engine and doc.
                    client
                        .client()
                        .call(methods::QUEUE_MESSAGE, json!({"chatId": CHAT, "text": "scenario:hooks"}))
                        .await
                        .map_err(|error| anyhow::anyhow!("queue hooks turn: {error}"))?;
                    let mut parts = serde_json::Value::Null;
                    for _ in 0..150 {
                        settle(cx, 100).await;
                        parts = hook_parts(&core_ui, &rt)?;
                        if parts["text"].as_str().is_some_and(|t| t.contains(FINAL_TEXT))
                            && parts["tools"].as_array().is_some_and(|t| t.len() >= 6)
                        {
                            break;
                        }
                    }
                    settle(cx, 700).await;
                    capture(shell.into(), cx, output.join("01-hook-chips.png"))?;
                    let tools = parts["tools"].as_array().cloned().unwrap_or_default();
                    let find = |id: &str| tools.iter().find(|t| t["id"] == id).cloned().unwrap_or_default();
                    let guard = find("hook:hk-guard");
                    anyhow::ensure!(guard["chip"][0] == "Hook" && guard["chip"][1] == "PreToolUse · Bash", "guard chip: {parts}");
                    anyhow::ensure!(guard["isError"] == true && guard["resolved"] == true, "guard state: {parts}");
                    anyhow::ensure!(
                        guard["preview"].as_str().is_some_and(|p| p.starts_with("Hook blocked the tool call: rm -rf is blocked by project policy")),
                        "guard preview must state the outcome: {parts}"
                    );
                    let session = find("hook:280a2fc8-3647-4632-8142-c1d9c11b9dee");
                    anyhow::ensure!(session["chip"][1] == "SessionStart · startup" && session["isError"] == false, "SessionStart chip lost at the run boundary: {parts}");
                    anyhow::ensure!(find("hook:hk-lint")["preview"].as_str().is_some_and(|p| p.starts_with("Hook sent feedback to Claude")), "{parts}");
                    anyhow::ensure!(find("hook:hk-stop")["preview"].as_str().is_some_and(|p| p.starts_with("Hook failed (exit 1)")), "{parts}");
                    anyhow::ensure!(find("hook:hk-end").is_null(), "SessionEnd must not reopen the turn: {parts}");
                    record.insert("hookTranscript".into(), parts);

                    // 2. A typed skill selection queued through the engine.
                    let skill_path = project.join(".claude/commands/cc12-project-only.md");
                    client
                        .client()
                        .call(methods::QUEUE_MESSAGE, json!({
                            "chatId": CHAT,
                            "text": "summarize the parser",
                            "inputItems": [{"type": "skill", "name": "cc12-project-only", "path": skill_path}],
                        }))
                        .await
                        .map_err(|error| anyhow::anyhow!("queue typed skill: {error}"))?;
                    let mut user_line = None;
                    for _ in 0..150 {
                        settle(cx, 100).await;
                        let log = std::fs::read_to_string(&fake_log).unwrap_or_default();
                        user_line = log
                            .lines()
                            .filter_map(|line| line.strip_prefix("STDIN: "))
                            .find(|line| line.contains("cc12-project-only summarize the parser"))
                            .map(str::to_owned);
                        if user_line.is_some() {
                            break;
                        }
                    }
                    let user_line = user_line.ok_or_else(|| anyhow::anyhow!(
                        "typed skill never reached the CLI as a native invocation: {}",
                        std::fs::read_to_string(&fake_log).unwrap_or_default()
                    ))?;
                    let frame: serde_json::Value = serde_json::from_str(&user_line)?;
                    let content = &frame["message"]["content"];
                    let text = content.as_str().map(str::to_owned).or_else(|| {
                        content.as_array()?.last()?["text"].as_str().map(str::to_owned)
                    });
                    anyhow::ensure!(
                        text.as_deref().is_some_and(|t| t.starts_with("/cc12-project-only summarize the parser")),
                        "native invocation must lead the text: {frame}"
                    );
                    record.insert("typedSkillUserFrame".into(), frame);
                    settle(cx, 600).await;

                    // 3. Effective extensions for the task's Full access launch.
                    let mut full = None;
                    for _ in 0..100 {
                        settle(cx, 100).await;
                        full = panel.update(cx, |panel, _| panel.loaded_view().cloned());
                        if full.is_some() {
                            break;
                        }
                    }
                    let full = full.ok_or_else(|| anyhow::anyhow!("extensions view never loaded"))?;
                    settle(cx, 300).await;
                    capture(panel_window.into(), cx, output.join("02-extensions-full.png"))?;
                    anyhow::ensure!(full.native_verified, "Full view must be verified: {full:?}");
                    anyhow::ensure!(full.permission_mode == PermissionMode::Full);
                    anyhow::ensure!(full.output_style.native.as_deref() == Some("project-style"));
                    anyhow::ensure!(
                        full.hooks.iter().any(|h| h.event == "PreToolUse" && h.matcher.as_deref() == Some("Bash") && h.handler == "command · guard.sh" && h.effective),
                        "project PreToolUse hook: {:?}", full.hooks
                    );
                    anyhow::ensure!(full.hooks.iter().any(|h| h.scope == luna_proto::NativeSettingsScope::Managed && h.effective));
                    record.insert("viewFull".into(), serde_json::to_value(&full)?);

                    // 4. The same task previewed as an Ask (restricted) launch.
                    panel.update(cx, |panel, cx| panel.set_mode(Some(PermissionMode::Ask), cx));
                    let mut ask = None;
                    for _ in 0..100 {
                        settle(cx, 100).await;
                        ask = panel.update(cx, |panel, _| {
                            panel.loaded_view().filter(|v| v.permission_mode == PermissionMode::Ask).cloned()
                        });
                        if ask.is_some() {
                            break;
                        }
                    }
                    let ask = ask.ok_or_else(|| anyhow::anyhow!("Ask view never loaded"))?;
                    settle(cx, 300).await;
                    capture(panel_window.into(), cx, output.join("03-extensions-ask.png"))?;
                    anyhow::ensure!(ask.launch_policy.starts_with("Restricted mode"), "{ask:?}");
                    anyhow::ensure!(ask.output_style.native.as_deref() == Some("default"));
                    anyhow::ensure!(ask.commands.iter().all(|c| !c.configured_effective));
                    anyhow::ensure!(
                        ask.hooks.iter().filter(|h| h.scope != luna_proto::NativeSettingsScope::Managed).all(|h| !h.effective),
                        "restricted mode must not run user/project hooks: {:?}", ask.hooks
                    );
                    anyhow::ensure!(
                        ask.hooks.iter().any(|h| h.scope == luna_proto::NativeSettingsScope::Managed && h.effective),
                        "managed hooks still apply under restricted mode"
                    );
                    record.insert("viewAsk".into(), serde_json::to_value(&ask)?);
                    record.insert(
                        "fakeCliLog".into(),
                        std::fs::read_to_string(&fake_log).unwrap_or_default().into(),
                    );
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
                                output.join("claude-extensions.pass"),
                                "PASS: headless production Shell + NativeExtensionsPanel; real ClaudeHarness on the 2.1.286-shaped fake CLI: hook chips with outcomes in the doc/transcript, typed skill as native /invocation, effective Full vs restricted Ask extension views verified by the CLI's initialize answer\n",
                            )
                        });
                        if let Err(error) = written {
                            *failure_for_ui.lock().unwrap() = Some(error.to_string());
                        }
                    }
                    Err(error) => {
                        eprintln!("Claude extensions fixture failed: {error:#}");
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                        *failure_for_ui.lock().unwrap() = Some(error.to_string());
                    }
                }
                let _ = shell.update(cx, |_, window, _| window.remove_window());
                let _ = panel_window.update(cx, |_, window, _| window.remove_window());
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
