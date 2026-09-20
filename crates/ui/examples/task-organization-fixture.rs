//! Interactive L12 task-organization QA fixture.
//!
//! The only accepted argument is an explicit synthetic root. Engine, UI,
//! provider homes, repository folders, credentials, and temporary files stay
//! below that root. Auth uses an in-memory credential broker; Codex native
//! metadata access and every platform-Keychain fallback are disabled. No
//! provider process or live account is used.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use zeron_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use zeron_proto::{ChatConfig, HarnessId, PermissionMode, SandboxLevel};
use zeron_rpc::{RpcClient, methods};
use zeron_ui::*;

const TASK_PINNED_A: &str = "12121212-1212-4121-8121-121212121212";
const TASK_PINNED_B: &str = "23232323-2323-4232-8232-232323232323";
const TASK_PERSONAL: &str = "34343434-3434-4343-8343-343434343434";
const TASK_ARCHIVED: &str = "45454545-4545-4454-8454-454545454545";
const TASK_REMOTE: &str = "56565656-5656-4565-8565-565656565656";
const TASK_NON_GIT: &str = "67676767-6767-4676-8676-676767676767";
const REMOTE_DEVICE: &str = "fixture-offline-machine";

fn account_config(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
        cursor_sdk_auth_file: root.join("synthetic-cursor-auth.json"),
    }
}

fn fixture_auth_config(root: &Path) -> AuthConfig {
    let mut config = AuthConfig::new("http://127.0.0.1:1", root.join("synthetic-auth"));
    config.dev_user_id = "l12-fixture-user".into();
    config.use_in_memory_credentials_for_tests();
    config
}

fn task_config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Mock,
        provider_account_id: None,
        model: Some("fixture-only".into()),
        reasoning: None,
        model_options: Default::default(),
        sandbox: SandboxLevel::WorkspaceWrite,
        permission_mode: PermissionMode::Ask,
        effective_permission_mode: PermissionMode::Ask,
        permission_ceiling_source: None,
        permission_degradation_reason: None,
        permission_audit: Vec::new(),
    }
}

fn init_git_repo(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    let status = std::process::Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg(path)
        .status()?;
    anyhow::ensure!(status.success(), "fixture git init failed");
    Ok(())
}

async fn snapshot(
    client: &RpcClient,
    task_id: &str,
) -> anyhow::Result<zeron_proto::TaskCommandRead> {
    Ok(client
        .call_as(
            methods::GET_TASK_COMMAND_SNAPSHOT,
            serde_json::json!({ "taskId": task_id }),
        )
        .await?)
}

async fn command(
    client: &RpcClient,
    command_id: &str,
    task_id: &str,
    intent: serde_json::Value,
) -> anyhow::Result<()> {
    let read = snapshot(client, task_id).await?;
    let mut envelope = serde_json::json!({
        "commandId": command_id,
        "actorId": "l12-native-fixture",
        "clientId": "l12-native-fixture",
        "clientSessionId": "l12-native-fixture-bootstrap",
        "taskId": task_id,
        "expectedTaskRevision": read.snapshot.task_revision,
        "intent": intent,
    });
    if let Some(generation) = read.snapshot.binding_generation {
        envelope["expectedBindingGeneration"] = generation.into();
    }
    let receipt: zeron_proto::CommandReceipt = client
        .call_as(methods::EXECUTE_TASK_COMMAND, envelope)
        .await?;
    anyhow::ensure!(
        receipt.status == zeron_proto::CommandReceiptStatus::Applied,
        "fixture command {command_id} ended {:?}: {:?}",
        receipt.status,
        receipt.reason
    );
    Ok(())
}

async fn seed(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    if !core.workspace.read_chats()?.is_empty() {
        return Ok(());
    }
    let repo = root.join("workspaces/fieldnotes");
    let app = repo.join("apps/desktop");
    let other = root.join("workspaces/unrelated");
    let notes = root.join("workspaces/notes");
    let other_notes = root.join("workspaces/other-notes");
    init_git_repo(&repo)?;
    init_git_repo(&other)?;
    std::fs::create_dir_all(&app)?;
    std::fs::create_dir_all(&notes)?;
    std::fs::create_dir_all(&other_notes)?;
    core.workspace.create_space(
        "fixture-app",
        &core.device_id,
        app.to_str().unwrap(),
        Some("Fieldnotes Desktop".into()),
        true,
    )?;
    core.workspace.create_space(
        "fixture-root",
        &core.device_id,
        repo.to_str().unwrap(),
        Some("Fieldnotes".into()),
        true,
    )?;
    core.workspace.create_space(
        "fixture-other",
        &core.device_id,
        other.to_str().unwrap(),
        Some("Unrelated checkout".into()),
        true,
    )?;
    core.workspace.create_space(
        "fixture-remote",
        REMOTE_DEVICE,
        "/synthetic/remote/archived-client",
        Some("Offline client project".into()),
        true,
    )?;
    core.workspace.create_space(
        "fixture-notes",
        &core.device_id,
        notes.to_str().unwrap(),
        Some("Notes (non-Git)".into()),
        false,
    )?;
    core.workspace.create_space(
        "fixture-other-notes",
        &core.device_id,
        other_notes.to_str().unwrap(),
        Some("Other notes (different root)".into()),
        false,
    )?;

    for (id, space, cwd, title) in [
        (
            TASK_PINNED_A,
            Some("fixture-app"),
            Some(app.clone()),
            "Pinned design review",
        ),
        (
            TASK_PINNED_B,
            Some("fixture-root"),
            Some(repo.clone()),
            "Pinned release checklist",
        ),
        (TASK_PERSONAL, None, None, "Personal scratch task"),
        (
            TASK_ARCHIVED,
            Some("fixture-root"),
            Some(repo.clone()),
            "Archived migration notes",
        ),
    ] {
        core.workspace.create_chat(
            id,
            space,
            Some(&core.device_id),
            Some(task_config()),
            cwd.map(|path| path.to_string_lossy().into_owned()),
        )?;
        core.workspace.rename_chat(id, title)?;
    }
    core.workspace.create_chat(
        TASK_REMOTE,
        Some("fixture-remote"),
        Some(REMOTE_DEVICE),
        Some(task_config()),
        Some("/synthetic/remote/archived-client".into()),
    )?;
    core.workspace
        .rename_chat(TASK_REMOTE, "Disconnected project task")?;
    core.workspace.create_chat(
        TASK_NON_GIT,
        None,
        Some(&core.device_id),
        Some(task_config()),
        Some(notes.to_string_lossy().into_owned()),
    )?;
    core.workspace
        .rename_chat(TASK_NON_GIT, "Non-Git workspace task")?;
    core.workspace.set_chat_harness_session(
        TASK_PINNED_A,
        "fixture-provider-session-a",
        app.to_str().unwrap(),
    );
    let client = zeron_rpc::memory_client(core.rpc_service());
    command(
        &client,
        "fixture-pin-a",
        TASK_PINNED_A,
        serde_json::json!({ "kind": "setPinned", "pinned": true }),
    )
    .await?;
    command(
        &client,
        "fixture-pin-b",
        TASK_PINNED_B,
        serde_json::json!({ "kind": "setPinned", "pinned": true }),
    )
    .await?;
    command(
        &client,
        "fixture-unread",
        TASK_PINNED_B,
        serde_json::json!({ "kind": "setRead", "read": false }),
    )
    .await?;
    command(
        &client,
        "fixture-archive",
        TASK_ARCHIVED,
        serde_json::json!({ "kind": "setArchived", "archived": true }),
    )
    .await?;
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
            EngineProfile::development(&engine_data, "l12-fixture-org", "l12-fixture-user"),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            accounts,
        )
    })?;
    // Attach hermetic auth before seed creates an in-memory RPC client and
    // before IPC/bootstrap can lazily request EngineCore::auth().
    core.set_auth(Auth::new(fixture_auth_config(&root)));
    runtime.block_on(seed(&core, &root))?;
    std::fs::write(
        root.join("SAFETY.txt"),
        "L12 synthetic fixture only. Auth credential broker is in-memory; native provider metadata and platform Keychain fallbacks are disabled. No live provider/account/store access.\n",
    )?;

    let ipc_port = std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port();
    let _ipc = runtime.block_on(zeron_engine::serve_ipc(ipc_port, core.rpc_service()))?;
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
                        "name": "Synthetic L12 Mac",
                        "platform": "macos",
                        "lastSeenAt": null
                    }))
                    .unwrap(),
                    serde_json::from_value(serde_json::json!({
                        "id": REMOTE_DEVICE,
                        "name": "Offline QA Mac",
                        "platform": "macos",
                        "lastSeenAt": "2024-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                ];
                state.chats = chats;
                state.spaces = spaces;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.select_chat(Some(TASK_PINNED_A.into()), cx);
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
    fn fixture_auth_broker_is_hermetic_and_has_no_platform_fallback() {
        let root = tempfile::tempdir().unwrap();
        let config = fixture_auth_config(root.path());
        assert_eq!(config.data_dir, root.path().join("synthetic-auth"));
        assert_eq!(config.dev_user_id, "l12-fixture-user");

        let broker = config.test_credential_broker();
        assert!(
            broker.independent_in_memory_handle_for_tests().is_some(),
            "only the in-memory broker exposes an independent test handle"
        );
        assert!(
            broker.set_in_memory_store_locked_for_tests(true),
            "production/platform brokers reject this in-memory-only hook"
        );
        assert!(broker.set_in_memory_store_locked_for_tests(false));
    }
}
