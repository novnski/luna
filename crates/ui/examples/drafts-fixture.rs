//! Interactive L08 composer-draft QA fixture.
//!
//! The data root is an explicit command-line argument. Provider homes are
//! synthetic paths below it, the mock harness is selected, no Auth object is
//! installed, and no upstream provider or macOS Keychain path is consulted.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowOptions, px, size};
use sha2::{Digest, Sha256};
use zeron_engine::{AgentAccountsConfig, EngineCore, EngineProfile};
use zeron_proto::{
    ChatConfig, ComposerDraft, DraftBlob, DraftBlobState, DraftContext, HarnessId, PermissionMode,
    SandboxLevel,
};
use zeron_rpc::methods;
use zeron_ui::*;

const TASK_A: &str = "l08-native-a";
const TASK_B: &str = "l08-native-b";
const PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

fn unused_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn capture_after_load(
    window: gpui::AnyWindowHandle,
    path: PathBuf,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    cx.background_executor()
        .timer(Duration::from_secs(15))
        .await;
    window.update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })?
}

fn account_config(root: &Path) -> AgentAccountsConfig {
    let providers = root.join("synthetic-providers");
    AgentAccountsConfig {
        data_dir: providers.clone(),
        claude_config_dir: providers.join("claude"),
        claude_config_file: providers.join("claude.json"),
        codex_home: providers.join("codex"),
        cursor_sdk_auth_file: providers.join("cursor/auth.json"),
    }
}

fn draft(task_id: &str, revision: u64, text: &str) -> ComposerDraft {
    ComposerDraft {
        version: zeron_proto::COMPOSER_DRAFT_VERSION,
        draft_id: task_id.into(),
        task_id: Some(task_id.into()),
        revision,
        text: text.into(),
        attachments: Vec::new(),
        appshots: Vec::new(),
        context: DraftContext::default(),
        execution: serde_json::Value::Null,
        updated_at_ms: chrono::Utc::now().timestamp_millis(),
    }
}

async fn seed_recovery(core: &EngineCore) -> anyhow::Result<()> {
    let client = zeron_rpc::memory_client(core.rpc_service());
    let png = base64::engine::general_purpose::STANDARD.decode(PNG_B64)?;
    client
        .call(
            methods::UPLOAD_CHUNK,
            serde_json::json!({
                "uploadId":"l08-native-recovery",
                "data":PNG_B64,
                "seq":0
            }),
        )
        .await?;
    let committed = client
        .call(
            methods::UPLOAD_COMMIT,
            serde_json::json!({
                "uploadId":"l08-native-recovery",
                "fileName":"recovered.png"
            }),
        )
        .await?;
    let path = committed["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("fixture upload returned no path"))?
        .to_string();
    let mut older = draft(TASK_A, 10, "L08 recoverable older message");
    older.attachments.push(DraftBlob {
        attachment_id: "l08-recover-image".into(),
        name: "recovered.png".into(),
        path: Some(path),
        sha256: Some(format!("{:x}", Sha256::digest(&png))),
        size: Some(png.len() as u64),
        state: DraftBlobState::Saved,
        error: None,
    });
    client
        .call(
            methods::BEGIN_DRAFT_SUBMISSION,
            serde_json::json!({
                "commandId":"l08-native-recover-command",
                "taskId":TASK_A,
                "snapshot":older
            }),
        )
        .await?;
    client
        .call(
            methods::SETTLE_DRAFT_SUBMISSION,
            serde_json::json!({
                "commandId":"l08-native-recover-command",
                "outcome":"recoverable"
            }),
        )
        .await?;
    client
        .call(
            methods::SAVE_COMPOSER_DRAFT,
            serde_json::json!({
                "draft":draft(TASK_A, 20, "L08 newer draft stays first")
            }),
        )
        .await?;
    Ok(())
}

async fn seed_queue(core: &EngineCore) -> anyhow::Result<()> {
    let client = zeron_rpc::memory_client(core.rpc_service());
    core.doc_host.open(TASK_A)?;
    core.doc_host.pause_all_queues();
    client
        .call(
            methods::UPLOAD_CHUNK,
            serde_json::json!({
                "uploadId":"l09-native-attachment",
                "data":PNG_B64,
                "seq":0
            }),
        )
        .await?;
    let committed = client
        .call(
            methods::UPLOAD_COMMIT,
            serde_json::json!({
                "uploadId":"l09-native-attachment",
                "fileName":"l09-queue.png"
            }),
        )
        .await?;
    let attachment = committed["path"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("fixture upload returned no path"))?;
    for (command_id, message_id, text, attachments) in [
        (
            "l09-native-enqueue-edit",
            "l09-native-edit",
            "L09 edit me — attachment must remain owned",
            vec![attachment],
        ),
        (
            "l09-native-enqueue-remove",
            "l09-native-remove",
            "L09 remove me safely",
            Vec::new(),
        ),
        (
            "l09-native-enqueue-send",
            "l09-native-send",
            "L09 send now exactly once",
            Vec::new(),
        ),
    ] {
        let receipt: zeron_proto::CommandReceipt = client
            .call_as(
                methods::EXECUTE_TASK_COMMAND,
                serde_json::json!({
                    "commandId": command_id,
                    "actorId": "l09-native-fixture",
                    "clientId": "l09-native-fixture",
                    "clientSessionId": "isolated",
                    "taskId": TASK_A,
                    "intent": {
                        "kind": "queueMessage",
                        "messageId": message_id,
                        "text": text,
                        "attachments": attachments,
                        "holdForTurnEnd": true,
                        "execution": {
                            "harness": "mock",
                            "providerAccountId": null,
                            "model": "mock-1",
                            "reasoning": null,
                            "modelOptions": {},
                            "sandbox": "workspace-write",
                            "permissionMode": "ask",
                            "effectivePermissionMode": "ask"
                        }
                    }
                }),
            )
            .await?;
        anyhow::ensure!(
            receipt.status == zeron_proto::CommandReceiptStatus::Applied,
            "fixture queue seed was not applied"
        );
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let root = PathBuf::from(std::env::args().nth(1).expect("isolated fixture data root"));
    let mode = std::env::args().nth(2).unwrap_or_else(|| "resume".into());
    let capture_path = std::env::args().nth(3).map(PathBuf::from);
    let engine_data = root.join("engine");
    let ui_data = root.join("ui");
    std::fs::create_dir_all(&engine_data)?;
    std::fs::create_dir_all(&ui_data)?;

    let runtime = tokio::runtime::Runtime::new()?;
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(&engine_data, "l08-native-org", "l08-native-user"),
            Arc::new(zeron_engine::default_registry()),
            HarnessId::Mock,
            None,
            account_config(&root),
        )
    })?;
    let existing = core.workspace.read_chats()?;
    for (id, title) in [(TASK_A, "L08 Draft A"), (TASK_B, "L08 Draft B")] {
        if !existing.iter().any(|chat| chat.id == id) {
            core.workspace
                .create_chat(id, None, Some(&core.device_id), None, None)?;
            core.workspace.rename_chat(id, title)?;
        }
    }
    core.workspace.set_chat_config(
        TASK_A,
        &ChatConfig {
            harness: HarnessId::Mock,
            provider_account_id: None,
            model: Some("mock-1".into()),
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            permission_mode: PermissionMode::Ask,
            effective_permission_mode: PermissionMode::Ask,
            permission_ceiling_source: None,
            permission_degradation_reason: None,
            permission_audit: Vec::new(),
        },
    )?;
    if mode == "recovery" {
        runtime.block_on(seed_recovery(&core))?;
    } else if mode == "queue" {
        core.workspace.rename_chat(TASK_A, "L09 Durable Queue")?;
        runtime.block_on(seed_queue(&core))?;
    }

    let ipc_port = unused_port();
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
    let device = core.device_id.clone();
    let seed = matches!(mode.as_str(), "seed" | "queue");

    gpui_platform::application()
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
            app_menus::init(cx);
            let state = cx.new(|_| {
                let mut state = state::AppState::new();
                state.fixture_attachment_engine(handle);
                state.connection = zeron_proto::view::ConnectionStatus::Ready;
                state.workspace_scope = Some(zeron_proto::WorkspaceScope::Development);
                state.local_device_id = Some(device.clone());
                state.devices = vec![
                    serde_json::from_value(serde_json::json!({
                        "id":device,
                        "name":"Synthetic L08 Mac",
                        "platform":"macos",
                        "lastSeenAt":null
                    }))
                    .unwrap(),
                ];
                state.chats = chats;
                state.selected_chat = Some(TASK_A.into());
                state.auto_selected = true;
                state.chats_synced = true;
                state.spaces_synced = true;
                state
            });
            let window = cx
                .open_window(
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
            if seed {
                let composer = window
                    .update(cx, |shell, _, _| shell.fixture_appshots_composer())
                    .unwrap();
                composer.update(cx, |composer, cx| composer.fixture_stage_draft_evidence(cx));
            }
            state.update(cx, |_, cx| cx.notify());
            cx.activate(true);
            if let Some(path) = capture_path {
                cx.spawn(async move |cx| {
                    if let Err(error) = capture_after_load(window.into(), path, cx).await {
                        eprintln!("draft fixture capture failed: {error:#}");
                    }
                })
                .detach();
            }
        });
    runtime.block_on(core.shutdown());
    Ok(())
}
