//! Headless GPUI E2E for Claude Code review (CC-15).
//!
//! Drives the production `Shell` review dialog on a Claude chat in a hidden
//! window against an in-process `EngineCore` with the REAL `ClaudeHarness`
//! pointed at the deterministic Claude Code 2.1.286 peer
//! `luna-harness/tests/fixtures/fake-claude-goals.py` (shapes from a real
//! no-model probe and the installed bundle). The workspace is a throwaway git
//! repository with an uncommitted change. Nothing contacts a provider,
//! account or model, touches LUNA Dev or its data directory, or opens a
//! visible window.
//!
//! Proves: the Claude dialog wording and the separate cloud-ultrareview lane
//! (shown unavailable, no control); Start review runs Claude's own
//! `/code-review <base>...HEAD`, never Codex `review/start`; Claude's
//! `ReportFindings` lands on the shared findings UI with provenance (the
//! repository HEAD and its uncommitted state).
//!
//! Failure cases: `$B/evidence/cc14-goals-review/failure-cases.md` (R1, R2, R5, R7).
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example claude-review-fixture
//! Run:   target/debug/examples/claude-review-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes 01..03 PNG captures, readback.json and claude-review.pass.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{AppContext, AsyncApp, Bounds, WindowBounds, WindowHandle, WindowOptions, px, size};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::ClaudeHarness;
use luna_proto::{ChatConfig, HarnessId, PermissionMode, SandboxLevel};
use luna_ui::*;

const PROJECT: &str = "claude-review-project";
const TASK: &str = "c14c14c1-0000-4000-8000-000000000015";
const CLOUD_LANE: &str = "Cloud ultrareview is a separate claude.ai service job. LUNA can't start or track one yet, so this review always runs locally. Run /ultrareview in a terminal if your account offers it.";

fn fake_claude() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../harness/tests/fixtures/fake-claude-goals.py");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    path
}

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::ClaudeCode,
        provider_account_id: None,
        model: Some("claude-haiku-4-5-20251001".into()),
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

fn git(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "-c",
            "user.name=LUNA fixture",
            "-c",
            "user.email=fixture@luna.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()?;
    anyhow::ensure!(output.status.success(), "git {args:?} failed");
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
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

#[cfg(target_os = "macos")]
fn activate_accessibility(window: &gpui::Window) -> anyhow::Result<()> {
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
fn activate_accessibility(_: &gpui::Window) -> anyhow::Result<()> {
    anyhow::bail!("AccessKit activation readback is implemented for macOS only")
}

fn a11y_labels(
    window: WindowHandle<shell::Shell>,
    cx: &mut AsyncApp,
) -> anyhow::Result<Vec<(String, String)>> {
    let window = gpui::AnyWindowHandle::from(window);
    let json = window.update(cx, |_, window, cx| {
        window.draw(cx).clear();
        window.draw(cx).clear();
        window.debug_a11y_tree_json()
    })?;
    let tree: serde_json::Value = serde_json::from_str(
        &json.ok_or_else(|| anyhow::anyhow!("AccessKit tree was not built"))?,
    )?;
    Ok(tree["nodes"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, node)| {
            Some((
                node["aria"]["label"].as_str()?.to_owned(),
                node["aria"]["role"].as_str().unwrap_or_default().to_owned(),
            ))
        })
        .collect())
}

fn transcript_texts(core: &EngineCore) -> anyhow::Result<Vec<(String, String)>> {
    Ok(core
        .doc_host
        .open(TASK)?
        .doc()
        .read_entries()
        .unwrap_or_default()
        .into_iter()
        .map(|entry| {
            let text: String = entry
                .parts
                .iter()
                .filter_map(|part| match part {
                    luna_doc::MessagePart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            (format!("{:?}", entry.role), text)
        })
        .collect())
}

fn main() -> anyhow::Result<()> {
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: claude-review-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in ["readback.json", "claude-review.pass", "failure.txt"] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let root = tempfile::tempdir()?;
    let ui_data = root.path().join("ui");
    let engine_data = root.path().join("engine");
    let project = root.path().join("project");
    let claude_dir = root.path().join("providers/claude");
    std::fs::create_dir_all(&ui_data)?;
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&claude_dir)?;
    // Account identity only (no credential): LUNA resolves Claude's native
    // account from this file before it launches the CLI.
    std::fs::write(
        root.path().join("providers/claude.json"),
        r#"{"oauthAccount":{"accountUuid":"claude-review-fixture"}}"#,
    )?;
    git(&project, &["init", "-q"])?;
    std::fs::write(project.join("auth.rs"), "fn is_valid() -> bool { true }\n")?;
    git(&project, &["add", "auth.rs"])?;
    git(&project, &["commit", "-q", "-m", "fixture"])?;
    std::fs::write(project.join("auth.rs"), "fn is_valid() -> bool { false }\n")?;
    let head = git(&project, &["rev-parse", "HEAD"])?;

    let runtime = tokio::runtime::Runtime::new()?;
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(
        ClaudeHarness::new()
            .with_executable(fake_claude())
            .with_config_dir(&claude_dir),
    ));
    let accounts = AgentAccountsConfig {
        data_dir: engine_data.clone(),
        claude_config_dir: claude_dir.clone(),
        claude_config_file: root.path().join("providers/claude.json"),
        codex_home: root.path().join("providers/codex"),
    };
    accounts.disable_claude_keychain_fallback()?;
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(
                &engine_data,
                "claude-review-fixture",
                "claude-review-fixture",
            ),
            Arc::new(registry),
            HarnessId::ClaudeCode,
            None,
            accounts,
        )
    })?;
    let mut auth = AuthConfig::new("", root.path().join("auth"));
    auth.dev_user_id = "claude-review-fixture".into();
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
    // A managed Claude task, created the way the app creates one.
    let client = runtime.block_on(async { luna_rpc::memory_client(core.rpc_service()) });
    let receipt: luna_proto::CommandReceipt = runtime.block_on(client.call_as(
        luna_rpc::methods::EXECUTE_TASK_COMMAND,
        serde_json::json!({
            "commandId": "create-claude-review-task",
            "actorId": "fixture", "clientId": "fixture", "clientSessionId": "fixture",
            "taskId": TASK,
            "intent": {"kind": "createChat", "spaceId": PROJECT,
                       "cwd": project.to_str().unwrap(), "config": config()},
        }),
    ))?;
    anyhow::ensure!(
        receipt.status == luna_proto::CommandReceiptStatus::Applied,
        "task creation failed: {receipt:?}"
    );
    let mut chat = core.workspace.chat(TASK)?.expect("created chat");
    chat.title = Some("Claude review fixture".into());
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
    let failure = Arc::new(Mutex::new(None::<String>));
    let failure_for_ui = failure.clone();
    let core = Arc::new(core);
    let core_for_ui = core.clone();
    let stdin_log = claude_dir.join("stdin.jsonl");
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
                    settle(cx, 900).await;
                    gpui::AnyWindowHandle::from(window)
                        .update(cx, |_, window, _| activate_accessibility(window))??;

                    // 1. The review dialog on a Claude chat: Claude wording and
                    // the cloud lane stated unavailable (no control).
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_open_native_review(TASK.into(), cx)
                    })?;
                    let mut dialog = serde_json::Value::Null;
                    for _ in 0..100 {
                        settle(cx, 50).await;
                        dialog = window.update(cx, |shell, _, _| shell.fixture_native_review_state())?;
                        if dialog["snapshotReady"] == true {
                            break;
                        }
                    }
                    anyhow::ensure!(
                        dialog["open"] == true && dialog["claude"] == true && dialog["snapshotReady"] == true,
                        "Claude review dialog did not open: {dialog}"
                    );
                    settle(cx, 400).await;
                    capture(window, cx, output.join("01-claude-review-dialog.png"))?;
                    let labels = a11y_labels(window, cx)?;
                    let lane = labels.iter().find(|(label, _)| label == CLOUD_LANE).cloned();
                    anyhow::ensure!(
                        lane.as_ref().is_some_and(|(_, role)| role == "Status"),
                        "cloud lane must be an unavailable status, not a control: {lane:?}"
                    );
                    record.insert("dialog".into(), dialog);
                    record.insert("cloudLane".into(), serde_json::json!(lane));

                    // 2. Base branch target.
                    window.update(cx, |shell, _, cx| {
                        shell.fixture_native_review_target(1, "main", cx)
                    })?;
                    settle(cx, 400).await;
                    capture(window, cx, output.join("02-claude-review-target.png"))?;

                    // 3. Start review: Claude's own `/code-review main...HEAD`;
                    // ReportFindings arrive on the shared findings UI.
                    window.update(cx, |shell, _, cx| shell.fixture_submit_native_review(cx))?;
                    let mut texts = Vec::new();
                    for _ in 0..200 {
                        settle(cx, 100).await;
                        texts = transcript_texts(&core_for_ui)?;
                        if texts.iter().any(|(_, text)| text.contains("call `toolu_rf2`")) {
                            break;
                        }
                    }
                    settle(cx, 900).await;
                    capture(window, cx, output.join("03-claude-review-findings.png"))?;
                    let stdin = std::fs::read_to_string(&stdin_log).unwrap_or_default();
                    let sent: Vec<String> = stdin
                        .lines()
                        .filter_map(|line| {
                            serde_json::from_str::<serde_json::Value>(line).ok()?["message"]["content"]
                                .as_str()
                                .map(str::to_owned)
                        })
                        .collect();
                    anyhow::ensure!(
                        sent == ["/code-review main...HEAD"],
                        "Claude must receive its own /code-review line only: {sent:?}"
                    );
                    let findings = texts
                        .iter()
                        .find(|(_, text)| text.contains("### Review findings"))
                        .map(|(_, text)| text.clone())
                        .unwrap_or_default();
                    let source = format!("source `{}` + uncommitted changes", &head[..12]);
                    anyhow::ensure!(
                        findings.contains("**`src/auth.rs:42`**")
                            && findings.contains(&source)
                            && findings.contains("call `toolu_rf1`")
                            && findings.contains("call `toolu_rf2`"),
                        "findings or provenance missing: {texts:?}"
                    );
                    let after = window.update(cx, |shell, _, _| shell.fixture_native_review_state())?;
                    anyhow::ensure!(after["open"] == false, "dialog stayed open: {after}");
                    record.insert("claudeStdin".into(), serde_json::json!(sent));
                    record.insert("transcript".into(), serde_json::json!(texts));
                    record.insert("reviewedHead".into(), serde_json::json!(head));
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
                                output.join("claude-review.pass"),
                                "PASS: production Shell review dialog on a Claude chat -> engine -> real ClaudeHarness -> 2.1.286 peer; /code-review main...HEAD; ReportFindings on the shared findings UI with source provenance; cloud ultrareview lane stated unavailable\n",
                            )
                        });
                        if let Err(error) = written {
                            *failure_for_ui.lock().unwrap() = Some(error.to_string());
                        }
                    }
                    Err(error) => {
                        eprintln!("Claude review fixture failed: {error:#}");
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                        *failure_for_ui.lock().unwrap() = Some(error.to_string());
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
        eprintln!("{error}");
        std::process::exit(1);
    }
    Ok(())
}
