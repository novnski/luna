//! Hermetic L14 session-history/search native QA fixture.
//!
//! Every engine, UI, auth, provider, workspace, and temporary path is rooted
//! below the explicit synthetic root. Auth is in-memory, platform Keychain
//! fallback and provider-native metadata discovery are disabled, and no real
//! provider executable or private provider store is consulted.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use luna_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_proto::{
    ChatConfig, HarnessId, HistoryProjectPolicy, PermissionMode, SandboxLevel, ToolCall,
};
use luna_rpc::methods;
use luna_ui::*;

const TASK_ALPHA: &str = "14141414-1414-4141-8141-141414141414";
const TASK_COLLISION: &str = "24242424-2424-4242-8242-242424242424";
const TASK_REMOTE: &str = "34343434-3434-4343-8343-343434343434";
const REMOTE_DEVICE: &str = "l14-offline-fixture-machine";
const FIXTURE_ID: &str = "luna-l14-session-history-v1";
const BUNDLE_ID: &str = "com.novnski.luna.l14-qa";

fn account_config(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    }
}

fn fixture_auth_config(root: &Path) -> AuthConfig {
    let mut config = AuthConfig::new("http://127.0.0.1:1", root.join("synthetic-auth"));
    config.dev_user_id = "l14-fixture-user".into();
    config.use_in_memory_credentials_for_tests();
    config
}

fn task_config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Mock,
        provider_account_id: None,
        model: Some("synthetic-l14-model".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Auto,
        effective_permission_mode: PermissionMode::Auto,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn text_entry(
    id: &str,
    role: MessageRole,
    text: &str,
    device_id: &str,
    created_at: i64,
) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role,
        parts: vec![MessagePart::Text {
            id: "text".into(),
            text: text.into(),
        }],
        created_at,
        device_id: device_id.into(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    }
}

fn tool(id: &str, call: ToolCall, failed: bool) -> MessagePart {
    MessagePart::Tool {
        id: id.into(),
        call,
        is_error: failed,
        resolved: true,
        output: Some("synthetic private output intentionally absent from the index".into()),
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: None,
        subagent_tail: None,
        subagent_activity: None,
    }
}

async fn seed(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    let existing_chats = core.workspace.read_chats()?;
    if !existing_chats.is_empty() {
        for task_id in [TASK_ALPHA, TASK_COLLISION, TASK_REMOTE] {
            anyhow::ensure!(
                existing_chats.iter().any(|chat| chat.id == task_id),
                "existing fixture root is missing required task {task_id}"
            );
        }
        return Ok(());
    }
    let alpha_root = root.join("workspaces/duplicate-alpha");
    let beta_root = root.join("workspaces/duplicate-beta");
    std::fs::create_dir_all(alpha_root.join("private"))?;
    std::fs::create_dir_all(&beta_root)?;
    core.workspace.create_space(
        "fixture-project-alpha",
        &core.device_id,
        alpha_root.to_str().unwrap(),
        Some("Duplicate project".into()),
        false,
    )?;
    core.workspace.create_space(
        "fixture-project-beta",
        &core.device_id,
        beta_root.to_str().unwrap(),
        Some("Duplicate project".into()),
        false,
    )?;
    core.workspace.create_space(
        "fixture-project-remote",
        REMOTE_DEVICE,
        "/synthetic/other-device/duplicate-project",
        Some("Duplicate project".into()),
        false,
    )?;
    for (task_id, project_id, title) in [
        (
            TASK_ALPHA,
            "fixture-project-alpha",
            "Searchable release investigation",
        ),
        (
            TASK_COLLISION,
            "fixture-project-beta",
            "Searchable release investigation",
        ),
        (
            TASK_REMOTE,
            "fixture-project-remote",
            "Searchable release investigation",
        ),
    ] {
        core.workspace
            .create_chat(task_id, Some(project_id), None, Some(task_config()), None)?;
        core.workspace.rename_chat(task_id, title)?;
    }

    let alpha = core.doc_host.open(TASK_ALPHA)?;
    alpha.doc().push_message(&text_entry(
        "alpha-prompt",
        MessageRole::User,
        "Investigate the lunar cache regression and preserve the exact task link",
        &core.device_id,
        1_780_000_000_000,
    ))?;
    alpha.doc().push_message(&SessionMessageEntry {
        id: "alpha-answer".into(),
        role: MessageRole::Assistant,
        parts: vec![
            MessagePart::Text {
                id: "answer".into(),
                text: "The bounded cache rebuild completed successfully".into(),
            },
            tool(
                "command",
                ToolCall::Exec {
                    command: "cargo test session_history".into(),
                },
                false,
            ),
            tool(
                "file",
                ToolCall::ReadFile {
                    path: alpha_root
                        .join("src/cache.rs")
                        .to_string_lossy()
                        .into_owned(),
                },
                false,
            ),
            tool(
                "excluded-file",
                ToolCall::ReadFile {
                    path: alpha_root.join(".env").to_string_lossy().into_owned(),
                },
                false,
            ),
            tool(
                "app",
                ToolCall::Mcp {
                    server: "synthetic-issues".into(),
                    tool: "read_ticket".into(),
                    input: Some(serde_json::json!({"private": "never indexed"})),
                },
                false,
            ),
            tool(
                "excluded-command-term",
                ToolCall::Exec {
                    command: "echo private-fixture-term".into(),
                },
                false,
            ),
            tool(
                "excluded-file-term",
                ToolCall::ReadFile {
                    path: alpha_root
                        .join("private-fixture-term.txt")
                        .to_string_lossy()
                        .into_owned(),
                },
                false,
            ),
            tool(
                "excluded-app-tool-term",
                ToolCall::Mcp {
                    server: "private-fixture-term-app".into(),
                    tool: "private-fixture-term-tool".into(),
                    input: None,
                },
                false,
            ),
            MessagePart::Image {
                id: "excluded-image-term".into(),
                path: alpha_root
                    .join("synthetic-image.png")
                    .to_string_lossy()
                    .into_owned(),
                name: "private-fixture-term.png".into(),
                mime_type: "image/png".into(),
            },
            MessagePart::Error {
                id: "failure".into(),
                message: "Synthetic retry failed once before recovery".into(),
            },
        ],
        created_at: 1_780_000_001_000,
        device_id: core.device_id.clone(),
        status: Some(MessageStatus::Complete),
        continuation_of: None,
    })?;
    alpha.doc().push_message(&text_entry(
        "excluded-prompt",
        MessageRole::User,
        "private-fixture-term must be withheld",
        &core.device_id,
        1_780_000_002_000,
    ))?;
    for index in 0..55 {
        alpha.doc().push_message(&text_entry(
            &format!("paging-{index:02}"),
            MessageRole::User,
            &format!("paging-fixture result {index:02}"),
            &core.device_id,
            1_780_000_010_000 + index,
        ))?;
    }

    for (task_id, message_id, device_id, text, timestamp) in [
        (
            TASK_COLLISION,
            "collision-prompt",
            core.device_id.as_str(),
            "Investigate the lunar cache regression in the second project",
            1_780_000_003_000,
        ),
        (
            TASK_REMOTE,
            "remote-prompt",
            REMOTE_DEVICE,
            "Investigate the lunar cache regression on the offline machine",
            1_780_000_004_000,
        ),
    ] {
        core.doc_host
            .open(task_id)?
            .doc()
            .push_message(&text_entry(
                message_id,
                MessageRole::User,
                text,
                device_id,
                timestamp,
            ))?;
    }

    core.store
        .save_activity_project_policy(&HistoryProjectPolicy {
            project_id: "fixture-project-alpha".into(),
            excluded_path_prefixes: vec![alpha_root.join("private").to_string_lossy().into_owned()],
            excluded_terms: vec!["private-fixture-term".into()],
            index_terminal_output: false,
            index_browser_fields: false,
            index_accessibility_text: false,
            index_screenshot_text: false,
        })?;
    let client = luna_rpc::memory_client(core.rpc_service());
    let rebuilt: luna_proto::HistoryRebuildResult = client
        .call_as(
            methods::REBUILD_SESSION_HISTORY,
            serde_json::json!({"taskIds": []}),
        )
        .await?;
    anyhow::ensure!(rebuilt.indexed_tasks == 3, "fixture rebuild missed tasks");
    std::fs::write(
        root.join("FIXTURE.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "fixtureId": FIXTURE_ID,
            "bundleIdentifier": BUNDLE_ID,
            "root": root,
            "tasks": [TASK_ALPHA, TASK_COLLISION, TASK_REMOTE],
            "defaultProject": "fixture-project-alpha",
            "queries": ["lunar cache", "tool:exec cargo", "app:synthetic-issues", "status:failed retry", "project:fixture-project-beta lunar", "paging-fixture"],
            "withheldQueries": ["private-fixture-term", ".env", "never indexed"],
            "pagingMatches": 55,
            "remoteTask": TASK_REMOTE,
            "rebuiltEvents": rebuilt.indexed_events,
        }))?,
    )?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated fixture root"));
    anyhow::ensure!(root.is_absolute(), "fixture root must be absolute");
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    let engine_data = root.join("engine");
    let ui_data = root.join("ui");
    let temp_data = root.join("tmp");
    std::fs::create_dir_all(&engine_data)?;
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(&temp_data)?;
    unsafe {
        std::env::set_var("TMPDIR", &temp_data);
        std::env::set_var("TMP", &temp_data);
        std::env::set_var("TEMP", &temp_data);
    }

    let runtime = tokio::runtime::Runtime::new()?;
    let accounts = account_config(&root);
    accounts.disable_claude_keychain_fallback()?;
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
            EngineProfile::development(&engine_data, "l14-fixture-org", "l14-fixture-user"),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            accounts,
        )
    })?;
    core.set_auth(Auth::new(fixture_auth_config(&root)));
    runtime.block_on(seed(&core, &root))?;
    std::fs::write(
        root.join("SAFETY.txt"),
        "L14 synthetic fixture only. In-memory auth; native provider metadata and platform Keychain fallbacks disabled. No live provider/account/private store access.\n",
    )?;

    let ipc_port = std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port();
    let _ipc = runtime.block_on(luna_engine::serve_ipc(
        ipc_port,
        core.rpc_service(),
        &engine_data,
    ))?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let handle = runtime.block_on(state::EngineHandle::bootstrap(boot.clone()))?;
    let chats = core.workspace.read_chats()?;
    let spaces = core.workspace.read_spaces()?;
    let device = core.device_id.clone();

    gpui_platform::application()
        .with_assets(icons::Assets)
        .run(move |cx| {
            gpui_tokio::init(cx);
            gpui_base::init(cx);
            let settings = settings::UiSettings::load(&ui_data);
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
            app_menus::init(cx);
            let state = cx.new(|cx| {
                let mut state = state::AppState::new();
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": device,
                        "name": "Synthetic L14 Mac",
                        "platform": "macos",
                        "lastSeenAt": null
                    }))
                    .unwrap(),
                    serde_json::from_value(serde_json::json!({
                        "id": REMOTE_DEVICE,
                        "name": "Offline L14 Mac",
                        "platform": "macos",
                        "lastSeenAt": "2024-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                ];
                state.chats = chats;
                state.spaces = spaces;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.select_chat(Some(TASK_ALPHA.into()), cx);
                state.fixture_attach_engine(handle, cx);
                state
            });
            cx.open_window(
                WindowOptions {
                    window_background: theme::Theme::of(cx).window_background_appearance(),
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        gpui::point(px(80.), px(80.)),
                        size(px(1100.), px(850.)),
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| shell::Shell::new(state.clone(), boot, cx)),
            )
            .unwrap();
            state.update(cx, |_, cx| cx.notify());
            cx.activate(true);
        });
    runtime.block_on(core.shutdown());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_auth_is_in_memory_and_root_scoped() {
        let root = tempfile::tempdir().unwrap();
        let config = fixture_auth_config(root.path());
        assert_eq!(config.data_dir, root.path().join("synthetic-auth"));
        assert!(
            config
                .test_credential_broker()
                .independent_in_memory_handle_for_tests()
                .is_some()
        );
        let accounts = account_config(root.path());
        assert!(accounts.codex_home.starts_with(root.path()));
        assert!(accounts.claude_config_dir.starts_with(root.path()));
    }
}
