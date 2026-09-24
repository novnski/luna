//! Hermetic L22 child-activity native QA fixture. Build here; the parent task
//! alone operates the desktop after source and bundle review.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use luna_doc::{
    MessagePart, MessageRole, MessageStatus, SessionMessageEntry, SubagentActivity, SubagentStatus,
};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_proto::{
    ChatConfig, HarnessId, PermissionMode, SandboxLevel, Session, SessionStatus, ToolCall,
};
use luna_ui::*;

const TASK: &str = "22222222-2222-4222-8222-222222222222";
const PROJECT: &str = "l22-synthetic-project";
const FIXTURE_ID: &str = "luna-l22-child-activity-v1";
const BUNDLE_ID: &str = "com.novnski.luna.l22-qa";
const SAFETY: &str = "L22 synthetic fixture only. In-memory auth; Mock harness; root-scoped provider homes; native Codex metadata and Claude Keychain fallback disabled. No live provider/account/private store access.\n";

#[derive(Clone, Copy)]
struct ChildSpec {
    id: &'static str,
    task: &'static str,
    role: &'static str,
    status: SubagentStatus,
    usage: (u64, u64),
    result: Option<&'static str>,
    error: Option<&'static str>,
    transcript: &'static str,
    has_doc: bool,
    parent_summary_error: bool,
}

const CHILDREN: [ChildSpec; 7] = [
    ChildSpec {
        id: "running",
        task: "Trace the cache",
        role: "explorer",
        status: SubagentStatus::Running,
        usage: (12, 3),
        result: None,
        error: None,
        transcript: "Checking the synthetic cache path.",
        has_doc: true,
        parent_summary_error: false,
    },
    ChildSpec {
        id: "waiting",
        task: "Await a decision",
        role: "reviewer",
        status: SubagentStatus::Waiting,
        usage: (8, 0),
        result: None,
        error: None,
        transcript: "Waiting for a synthetic provider input request.",
        has_doc: true,
        parent_summary_error: false,
    },
    ChildSpec {
        id: "idle",
        task: "Hold for follow-up",
        role: "general",
        status: SubagentStatus::Idle,
        usage: (6, 2),
        result: None,
        error: None,
        transcript: "Last synthetic assignment finished; provider reported idle.",
        has_doc: true,
        parent_summary_error: false,
    },
    ChildSpec {
        id: "done",
        task: "Verify two checks",
        role: "tester",
        status: SubagentStatus::Done,
        usage: (34, 5),
        result: Some("Two synthetic checks passed"),
        error: None,
        transcript: "Two synthetic checks passed. This is the child transcript.",
        has_doc: true,
        parent_summary_error: true,
    },
    ChildSpec {
        id: "failed",
        task: "Reproduce the failure",
        role: "tester",
        status: SubagentStatus::Failed,
        usage: (11, 1),
        result: None,
        error: Some("Synthetic child failure"),
        transcript: "Synthetic reproduction failed at the provider child boundary.",
        has_doc: true,
        parent_summary_error: false,
    },
    ChildSpec {
        id: "unobserved",
        task: "Inspect stream loss",
        role: "explorer",
        status: SubagentStatus::Unobserved,
        usage: (4, 0),
        result: None,
        error: None,
        transcript: "The parent stream ended without an observed child completion.",
        has_doc: true,
        parent_summary_error: false,
    },
    ChildSpec {
        id: "metadata-only",
        task: "Report metadata only",
        role: "general",
        status: SubagentStatus::Idle,
        usage: (7, 0),
        result: None,
        error: None,
        transcript: "",
        has_doc: false,
        parent_summary_error: false,
    },
];

fn account_config(root: &Path) -> AgentAccountsConfig {
    AgentAccountsConfig {
        data_dir: root.join("provider-state"),
        claude_config_dir: root.join("synthetic-claude"),
        claude_config_file: root.join("synthetic-claude.json"),
        codex_home: root.join("synthetic-codex"),
    }
}

fn auth_config(root: &Path) -> AuthConfig {
    let mut config = AuthConfig::new("http://127.0.0.1:1", root.join("synthetic-auth"));
    config.dev_user_id = "l22-fixture-user".into();
    config.use_in_memory_credentials_for_tests();
    config
}

fn task_config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::Mock,
        provider_account_id: None,
        model: Some("synthetic-l22-model".into()),
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

fn entry(
    id: &str,
    role: MessageRole,
    text: &str,
    device: &str,
    status: MessageStatus,
    at: i64,
) -> SessionMessageEntry {
    SessionMessageEntry {
        id: id.into(),
        role,
        parts: vec![MessagePart::Text {
            id: "text".into(),
            text: text.into(),
        }],
        created_at: at,
        device_id: device.into(),
        status: Some(status),
        continuation_of: None,
    }
}

fn child_doc_id(id: &str) -> String {
    format!("{TASK}--sub--{id}")
}

fn spawn_part(spec: &ChildSpec) -> MessagePart {
    MessagePart::Tool {
        id: spec.id.into(),
        call: ToolCall::Unknown {
            name: format!("Agent: {}", spec.task),
            input: Some(serde_json::json!({
                "description": spec.task, "subagent_type": spec.role, "model": "synthetic-l22-model"
            })),
        },
        // The completed child deliberately has an erroneous PARENT tool
        // summary. Its child status remains Done, never inferred as Failed.
        is_error: spec.parent_summary_error,
        resolved: true,
        output: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: spec.has_doc.then(|| child_doc_id(spec.id)),
        subagent_status: Some(spec.status),
        subagent_tail: None,
        subagent_activity: Some(SubagentActivity {
            result: spec.result.map(str::to_owned),
            error: spec.error.map(str::to_owned),
            input_tokens: Some(spec.usage.0),
            output_tokens: Some(spec.usage.1),
        }),
    }
}

fn reject_symlinks(root: &Path) -> anyhow::Result<()> {
    for item in std::fs::read_dir(root)? {
        let path = item?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "fixture root contains a symlink: {}",
            path.display()
        );
        if metadata.is_dir() {
            reject_symlinks(&path)?;
        }
    }
    Ok(())
}

fn guarded_root(requested: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(requested.is_absolute(), "fixture root must be absolute");
    let metadata = std::fs::symlink_metadata(requested)?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "fixture root must be a real directory"
    );
    anyhow::ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "fixture root must be owned by the current user"
    );
    let root = requested.canonicalize()?;
    anyhow::ensure!(
        root.parent() == Some(Path::new("/private/tmp"))
            && root
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("luna-l22-")),
        "canonical fixture root must be /private/tmp/luna-l22-*"
    );
    reject_symlinks(&root)?;
    let marker = root.join("FIXTURE.json");
    if marker.exists() {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
        anyhow::ensure!(
            value["fixtureId"] == FIXTURE_ID
                && value["bundleIdentifier"] == BUNDLE_ID
                && value["root"] == root.to_string_lossy().as_ref()
                && value["task"] == TASK,
            "existing root is not the exact L22 fixture"
        );
        anyhow::ensure!(
            std::fs::read_to_string(root.join("SAFETY.txt"))? == SAFETY,
            "existing fixture safety marker differs"
        );
    } else {
        anyhow::ensure!(
            std::fs::read_dir(&root)?.next().is_none(),
            "first run requires an empty root"
        );
    }
    Ok(root)
}

fn guarded_bundle() -> anyhow::Result<()> {
    let executable = std::env::current_exe()?.canonicalize()?;
    anyhow::ensure!(
        executable
            .file_name()
            .is_some_and(|name| name == "child-activity-fixture"),
        "wrong fixture executable"
    );
    let app = executable
        .ancestors()
        .nth(3)
        .ok_or_else(|| anyhow::anyhow!("missing app bundle"))?;
    anyhow::ensure!(
        app.file_name()
            .is_some_and(|name| name == "LUNA L22 QA.app")
            && app
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "luna-l22-fixture"),
        "fixture must run from its exact L22 QA bundle"
    );
    let plist = app.join("Contents/Info.plist");
    for (key, expected) in [
        ("CFBundleIdentifier", BUNDLE_ID),
        ("CFBundleExecutable", "child-activity-fixture"),
    ] {
        let output = std::process::Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", &format!("Print :{key}")])
            .arg(&plist)
            .output()?;
        anyhow::ensure!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == expected,
            "fixture bundle {key} differs"
        );
    }
    Ok(())
}

async fn seed(core: &EngineCore, root: &Path) -> anyhow::Result<()> {
    if root.join("FIXTURE.json").exists() {
        anyhow::ensure!(
            core.workspace
                .read_chats()?
                .iter()
                .any(|chat| chat.id == TASK),
            "saved task missing"
        );
        let parts = core.doc_host.open(TASK)?.doc().read_entries()?;
        anyhow::ensure!(
            parts
                .iter()
                .flat_map(|entry| &entry.parts)
                .filter(|part| matches!(part,
                    MessagePart::Tool { call, .. } if call.is_subagent_spawn()
                ))
                .count()
                == CHILDREN.len(),
            "saved child chips missing"
        );
        return Ok(());
    }
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace)?;
    core.workspace.create_space(
        PROJECT,
        &core.device_id,
        workspace.to_str().unwrap(),
        Some("L22 synthetic child activity".into()),
        false,
    )?;
    core.workspace
        .create_chat(TASK, Some(PROJECT), None, Some(task_config()), None)?;
    core.workspace.rename_chat(TASK, "Inspect child activity")?;
    let now = chrono::Utc::now().timestamp_millis();
    let parent = core.doc_host.open(TASK)?;
    parent.doc().push_message(&entry(
        "parent-prompt",
        MessageRole::User,
        "Inspect provider child status without inferring parent completion",
        &core.device_id,
        MessageStatus::Complete,
        now,
    ))?;
    let mut parts = vec![MessagePart::Text { id: "parent-progress".into(),
        text: "Synthetic parent stream remains open. No root Done event or completed-turn marker was seeded.".into() }];
    for (index, spec) in CHILDREN.iter().enumerate() {
        parts.push(spawn_part(spec));
        if spec.has_doc {
            let child = core.doc_host.open(&child_doc_id(spec.id))?;
            child.doc().push_message(&entry(
                "assignment",
                MessageRole::User,
                spec.task,
                &core.device_id,
                MessageStatus::Complete,
                now + index as i64 + 1,
            ))?;
            let child_status = match spec.status {
                SubagentStatus::Done | SubagentStatus::Failed | SubagentStatus::Idle => {
                    MessageStatus::Complete
                }
                SubagentStatus::Unobserved => MessageStatus::Aborted,
                _ => MessageStatus::Streaming,
            };
            child.doc().push_message(&entry(
                "child-output",
                MessageRole::Assistant,
                spec.transcript,
                &core.device_id,
                child_status,
                now + index as i64 + 2,
            ))?;
        }
    }
    parent.doc().push_message(&SessionMessageEntry {
        id: "parent-progress".into(),
        role: MessageRole::Assistant,
        parts,
        created_at: now + 20,
        device_id: core.device_id.clone(),
        status: Some(MessageStatus::Streaming),
        continuation_of: None,
    })?;
    core.workspace.record_session(&Session {
        chat_id: TASK.into(),
        device_id: core.device_id.clone(),
        status: SessionStatus::Idle,
        started_at: None,
        updated_at: chrono::Utc::now(),
        last_completed_turn: None,
        recovery: None,
    });
    std::fs::write(root.join("SAFETY.txt"), SAFETY)?;
    std::fs::write(
        root.join("FIXTURE.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1, "fixtureId": FIXTURE_ID, "bundleIdentifier": BUNDLE_ID,
            "root": root, "task": TASK, "project": PROJECT, "lastCompletedTurn": null,
            "children": CHILDREN.iter().map(|child| serde_json::json!({
                "id": child.id, "status": child.status, "docRef": child.has_doc.then(|| child_doc_id(child.id)),
                "inputTokens": child.usage.0, "outputTokens": child.usage.1,
            })).collect::<Vec<_>>(),
        }))?,
    )?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    guarded_bundle()?;
    let requested = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("isolated fixture root required"))?,
    );
    let root = guarded_root(&requested)?;
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
            EngineProfile::development(&engine_data, "l22-fixture-org", "l22-fixture-user"),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
            accounts,
        )
    })?;
    core.set_auth(Auth::new(auth_config(&root)));
    runtime.block_on(seed(&core, &root))?;
    let ipc_port = std::net::TcpListener::bind(("127.0.0.1", 0))?
        .local_addr()?
        .port();
    let _ipc = runtime.block_on(luna_engine::serve_ipc(ipc_port, core.rpc_service()))?;
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
                state.devices = vec![serde_json::from_value(serde_json::json!({
                "id": device, "name": "Synthetic L22 Mac", "platform": "macos", "lastSeenAt": null,
            })).unwrap()];
                state.chats = chats;
                state.spaces = spaces;
                state.chats_synced = true;
                state.spaces_synced = true;
                state.select_chat(Some(TASK.into()), cx);
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
    fn fixture_has_no_real_provider_or_completed_parent() {
        assert_eq!(task_config().harness, HarnessId::Mock);
        assert_eq!(CHILDREN.len(), 7);
        assert!(CHILDREN.iter().any(|child| !child.has_doc));
        assert!(
            CHILDREN
                .iter()
                .any(|child| child.parent_summary_error && child.status == SubagentStatus::Done)
        );
        let root = tempfile::tempdir().unwrap();
        assert!(
            auth_config(root.path())
                .test_credential_broker()
                .independent_in_memory_handle_for_tests()
                .is_some()
        );
        assert!(
            account_config(root.path())
                .codex_home
                .starts_with(root.path())
        );
        assert!(
            guarded_root(root.path()).is_err(),
            "an arbitrary temp root must be rejected"
        );
    }
}
