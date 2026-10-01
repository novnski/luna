//! Headless GPUI E2E for CC-11 Claude MCP inventory in the production
//! Settings > Providers "Skill library and MCP" card.
//!
//! A real in-process engine (IPC + `EngineHandle`) drives the deterministic
//! fake Claude CLI (`crates/harness/tests/fixtures/fake-claude-mcp.py`, 2.1.286
//! frame shapes). Provider homes, project and data live in a temporary root;
//! no real Claude, LUNA Dev or user configuration is touched. A Claude chat
//! run happens first so the selected chat has real session evidence.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example mcp-inventory-fixture
//! Run:   target/debug/examples/mcp-inventory-fixture ABSOLUTE_EVIDENCE_DIR
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    AppContext, Bounds, Context, Entity, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, size,
};
use luna_doc::SessionCommandPayload;
use luna_proto::{ChatConfig, HarnessId, PermissionMode, RunRequest, SandboxLevel};
use luna_ui::*;
use serde_json::json;

const TASK: &str = "cc11-fixture-task";
const PROJECT: &str = "cc11-fixture-project";

struct Root {
    page: Entity<settings::harnesses::HarnessesPage>,
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(typography::font_size(cx).pixels()));
        let theme = theme::Theme::of(cx);
        div()
            .size_full()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .p(px(24.0))
            .child(self.page.clone())
    }
}

fn state() -> serde_json::Value {
    let widget = "<!doctype html><html><body><h1>LUNA-CC11-WIDGET-MARKER</h1><script>window.parent.postMessage({},'*')</script></body></html>";
    json!({
        "servers": [
            {
                "name": "userdemo", "status": "connected", "scope": "user", "source": "user",
                "config": {"type": "stdio", "command": "/opt/secret/python3", "args": ["--token=fixture-secret"]},
                "serverInfo": {"name": "luna-cc11-demo", "version": "0.0.1"},
                "tools": [
                    {"name": "show_widget", "annotations": {}, "_meta": {"ui": {"resourceUri": "ui://demo/widget.html", "visibility": ["model", "app"]}}},
                    {"name": "echo", "annotations": {}}
                ],
                "resources": {"ui://demo/widget.html": [{
                    "uri": "ui://demo/widget.html", "mimeType": "text/html;profile=mcp-app", "text": widget,
                    "_meta": {"ui": {"csp": {"connectDomains": [], "resourceDomains": ["https://cdn.example.test"]}, "permissions": {"clipboardWrite": {}}, "prefersBorder": true}}
                }]}
            },
            {
                "name": "userauth", "status": "failed", "scope": "user", "source": "user",
                "error": "Dynamic Client Registration rejected (HTTP 401) at https://mcp.example.test/mcp?token=abc",
                "config": {"type": "http", "url": "https://mcp.example.test/mcp?token=abc", "headers": {"Authorization": "Bearer fixture-secret"}}
            },
            {
                "name": "projdemo", "status": "connected", "scope": "project", "source": "project",
                "config": {"type": "stdio", "command": "python3", "args": []},
                "serverInfo": {"name": "luna-cc11-demo", "version": "0.0.1"},
                "tools": [{"name": "echo", "annotations": {}}], "slowPolls": 1
            },
            {
                "name": "dyndemo", "status": "connected", "scope": "dynamic", "source": "dynamic",
                "config": {"type": "stdio", "command": "python3", "args": []},
                "serverInfo": {"name": "luna-bridge-sample", "version": "1.0.0"},
                "tools": []
            }
        ],
        "sessionInit": {
            "type": "system", "subtype": "init", "session_id": "sess-cc11-fixture",
            "model": "claude-haiku-4-5-20251001", "tools": ["Bash"],
            "mcp_servers": [
                {"name": "userdemo", "status": "connected", "source": "user"},
                {"name": "userauth", "status": "needs-auth", "source": "user"}
            ],
            "capabilities": ["mcp_read_resource_v1", "mcp_tool_ui_meta_v1"]
        }
    })
}

fn unused_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn main() -> anyhow::Result<()> {
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: mcp-inventory-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["mcp-inventory.json", "mcp-inventory.pass", "failure.txt"] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/fake-claude-mcp.py")
        .canonicalize()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
    }
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("ui");
    let engine_data = temp.path().join("engine");
    let providers = temp.path().join("providers");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&project)?;
    let state_path = temp.path().join("fake-claude-state.json");
    std::fs::write(&state_path, serde_json::to_vec_pretty(&state())?)?;
    std::fs::write(
        project.join(".fake-claude-mcp.json"),
        json!({"state": state_path, "log": temp.path().join("fake-claude-log.jsonl")}).to_string(),
    )?;
    // Synthetic provider-native account; no credential exists anywhere here.
    std::fs::create_dir_all(&providers)?;
    std::fs::write(
        providers.join("claude.json"),
        json!({"oauthAccount": {"accountUuid": "cc11-fixture", "emailAddress": "cc11@example.test"}}).to_string(),
    )?;
    let runtime = tokio::runtime::Runtime::new()?;
    let registry = luna_engine::HarnessRegistry::new();
    registry.register(Arc::new(
        luna_harness::ClaudeHarness::new().with_executable(&fake),
    ));
    let core = runtime.block_on(async {
        luna_engine::EngineCore::assemble_with_profile_and_agent_accounts_config(
            luna_engine::EngineProfile::development(&engine_data, "fixture-org", "fixture-user"),
            Arc::new(registry),
            HarnessId::ClaudeCode,
            None,
            luna_engine::AgentAccountsConfig {
                data_dir: engine_data.clone(),
                claude_config_dir: providers.join("claude"),
                claude_config_file: providers.join("claude.json"),
                codex_home: providers.join("codex"),
            },
        )
    })?;
    let mut auth_config = luna_engine::AuthConfig::new("", &engine_data);
    auth_config.use_in_memory_credentials_for_tests();
    auth_config.dev_user_id = "fixture-user".into();
    core.set_auth(luna_engine::Auth::new(auth_config));
    core.workspace.create_space(
        PROJECT,
        &core.device_id,
        project.to_str().unwrap(),
        Some("MCP fixture project".into()),
        false,
    )?;
    core.workspace.create_chat(
        TASK,
        Some(PROJECT),
        Some(&core.device_id),
        Some(ChatConfig {
            harness: HarnessId::ClaudeCode,
            provider_account_id: None,
            model: None,
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            permission_mode: PermissionMode::Ask,
            effective_permission_mode: PermissionMode::Ask,
            permission_ceiling_source: None,
            permission_degradation_reason: None,
            permission_audit: Vec::new(),
        }),
        Some(project.to_string_lossy().into_owned()),
    )?;
    // A real Claude run on the production path: its own init frame is the
    // selected chat's session evidence.
    runtime.block_on(async {
        let memory = luna_rpc::memory_client(core.rpc_service());
        core.doc_host.open(TASK)?;
        core.doc_host.queue_command_with_id_and_transfers(
            TASK,
            "cc11-fixture-run",
            SessionCommandPayload::Run {
                request: RunRequest {
                    goal: None,
                    review: None,
                    input_items: Vec::new(),
                    prompt: "hello".into(),
                    harness: Some(HarnessId::ClaudeCode),
                    provider_account_id: None,
                    model: None,
                    reasoning: None,
                    model_options: Default::default(),
                    cwd: project.to_string_lossy().into_owned(),
                    sandbox: SandboxLevel::WorkspaceWrite,
                    auto_approve: false,
                    permission_mode: PermissionMode::Ask,
                    parent_permission_ceiling: None,
                    environment_permission_ceiling: None,
                    permission_policy: None,
                    attachments: Vec::new(),
                    worktree: None,
                    resume: None,
                },
                message_id: "cc11-fixture-message".into(),
            },
            Vec::new(),
        )?;
        for _ in 0..300 {
            let view: luna_proto::IntegrationInventoryView = memory
                .call_as(
                    luna_rpc::methods::GET_INTEGRATION_INVENTORY,
                    json!({"provider": "claude-code", "projectId": PROJECT, "taskId": TASK}),
                )
                .await?;
            if view.items.iter().any(|item| item.attached == Some(true)) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("the Claude run never produced session evidence")
    })?;
    let ipc_port = unused_port();
    let _ipc = runtime.block_on(luna_engine::serve_ipc(
        ipc_port,
        core.rpc_service(),
        &engine_data,
    ))?;
    let boot = EngineBootConfig {
        data_dir: engine_data.clone(),
        ipc_port,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::ClaudeCode,
    };
    let handle = runtime.block_on(state::EngineHandle::bootstrap(boot))?;
    let chats = core.workspace.read_chats()?;
    let spaces = core.workspace.read_spaces()?;
    let device = core.device_id.clone();
    let failure = Arc::new(Mutex::new(None::<String>));
    let failure_for_ui = failure.clone();
    let capture_output = output.clone();

    gpui_platform::headless()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let settings = settings::UiSettings::default();
            settings::init(settings.clone(), data.clone(), cx);
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
            theme_library::init(data.clone(), cx);
            appearance::init(
                appearance::AppearanceMode::Dark,
                settings.theme_selection,
                settings.accent,
                settings.surface,
                cx,
            );
            composer::init(cx, settings.composer_send_behavior);
            let state = cx.new(|_| {
                let mut state = state::AppState::new();
                state.fixture_attachment_engine(handle);
                state.connection = luna_proto::view::ConnectionStatus::Ready;
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
                state.local_device_id = Some(device.clone());
                state.chats = chats;
                state.spaces = spaces;
                state.selected_space = Some(PROJECT.into());
                state.selected_chat = Some(TASK.into());
                state.chats_synced = true;
                state.spaces_synced = true;
                state
            });
            let page = cx.new(|cx| settings::harnesses::HarnessesPage::new(state.clone(), cx));
            let view = page.clone();
            let window: gpui::AnyWindowHandle = match cx.open_window(
                WindowOptions {
                    window_background: gpui::WindowBackgroundAppearance::Opaque,
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        gpui::point(px(20.0), px(20.0)),
                        size(px(1000.0), px(2600.0)),
                    ))),
                    show: false,
                    focus: false,
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Root { page: view }),
            ) {
                Ok(window) => window.into(),
                Err(error) => {
                    let _ = std::fs::write(
                        capture_output.join("failure.txt"),
                        format!("open window: {error:#}\n"),
                    );
                    std::process::exit(1);
                }
            };
            cx.spawn(async move |cx| {
                let result: anyhow::Result<()> = async {
                    let evidence = settings::harnesses::mcp_fixture::run_mcp_inventory_e2e(
                        page,
                        window,
                        &capture_output,
                        cx,
                    )
                    .await?;
                    let text = serde_json::to_string_pretty(&evidence)?;
                    for secret in ["fixture-secret", "token=abc", "/opt/secret"] {
                        anyhow::ensure!(!text.contains(secret), "{secret} reached the UI evidence");
                    }
                    std::fs::write(capture_output.join("mcp-inventory.json"), text)?;
                    std::fs::write(
                        capture_output.join("mcp-inventory.pass"),
                        "PASS: headless GPUI Settings MCP card; in-process engine + fake Claude CLI (2.1.286 shapes); real chat run session evidence\n",
                    )?;
                    Ok(())
                }
                .await;
                let _ = window.update(cx, |_, window, _| window.remove_window());
                if let Err(error) = result {
                    eprintln!("MCP inventory fixture failed: {error:#}");
                    let _ = std::fs::write(capture_output.join("failure.txt"), format!("{error:#}\n"));
                    *failure_for_ui.lock().unwrap() = Some(error.to_string());
                    // AppKit termination from inside `run` exits 0; fail explicitly.
                    std::process::exit(1);
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    drop(core);
    drop(temp);
    if let Some(error) = failure.lock().unwrap().take() {
        anyhow::bail!(error);
    }
    Ok(())
}
