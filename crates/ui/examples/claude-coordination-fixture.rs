//! Headless GPUI fixture for CC-13/CC-16 surfaces: native workflow/teammate
//! runs (Agents pane section), the Claude Code advanced-feature ledger and the
//! read-only list of Claude sessions outside LUNA.
//!
//! Renders the production `luna_ui::claude_advanced` functions against
//! synthetic data shaped like the harness E2E output and the real
//! `claude agents --json` probe rows. It never starts an engine, contacts a
//! provider, touches LUNA Dev or opens a visible window.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example claude-coordination-fixture
//! Run:   target/debug/examples/claude-coordination-fixture ABSOLUTE_EVIDENCE_DIR
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    AnyWindowHandle, AppContext, AsyncApp, Bounds, Context, Render, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use luna_proto::{
    ClaudeExternalSession, ClaudeExternalSessionActivity, ClaudeExternalSessionKind,
    ClaudeExternalSessionState, ClaudeExternalSessionsView, NativeTaskEndReason, NativeTaskKind,
    NativeTaskLedger, NativeTaskSnapshot, NativeTaskStatus, NativeWorkflowAgent,
    NativeWorkflowAgentState, NativeWorkflowPhase,
};
use luna_ui::claude_advanced::{self, ExternalSessionsState};
use luna_ui::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shot {
    Tasks,
    Settings,
}

struct Root {
    shot: Shot,
    ledger: NativeTaskLedger,
    sessions: ExternalSessionsState,
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(typography::font_size(cx).pixels()));
        let theme = theme::Theme::of(cx).clone();
        let page = div()
            .id("fixture-page")
            .size_full()
            .overflow_y_scroll()
            .bg(theme.bg)
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .text_size(typography::ui_rems(12.0));
        match self.shot {
            Shot::Tasks => page.p(px(16.0)).child(
                div()
                    .w(px(440.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px(px(12.0))
                            .py(px(8.0))
                            .border_b_1()
                            .border_color(theme.border)
                            .text_color(theme.text_muted)
                            .child("Agents · this conversation"),
                    )
                    .child(
                        div()
                            .p(px(12.0))
                            .children(claude_advanced::native_task_section(&theme, &self.ledger)),
                    ),
            ),
            Shot::Settings => {
                let accent = theme.accent;
                let refresh = gpui_base::Button::new("refresh-claude-sessions")
                    .role(gpui::Role::Button)
                    .accessibility_id("refresh-claude-sessions")
                    .accessibility_label("Refresh Claude sessions outside LUNA")
                    .tab_index(0)
                    .tab_stop(true)
                    .flex_none()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(8.0))
                    .text_size(typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .focus_visible(move |style| style.bg(accent.opacity(0.18)))
                    .child("Refresh")
                    .into_any_element();
                page.px(px(24.0)).pb(px(24.0)).child(
                    div()
                        .w(px(660.0))
                        .child(claude_advanced::ledger_card(&theme))
                        .child(claude_advanced::external_sessions_card(
                            &theme,
                            &self.sessions,
                            refresh,
                        )),
                )
            }
        }
    }
}

fn agent(
    index: u32,
    label: &str,
    phase: u32,
    state: NativeWorkflowAgentState,
    tokens: Option<u64>,
) -> NativeWorkflowAgent {
    NativeWorkflowAgent {
        index,
        label: label.into(),
        phase_index: Some(phase),
        state,
        model: Some("claude-haiku-4-5".into()),
        tokens,
        tool_calls: tokens.map(|t| t / 500),
        cached: false,
        blocked: false,
        error: None,
    }
}

/// Matches the harness E2E graph (`scenario:workflow`) plus finished runs.
fn synthetic_ledger() -> NativeTaskLedger {
    let mut blocked = agent(3, "audit b.ts", 2, NativeWorkflowAgentState::Failed, None);
    blocked.blocked = true;
    blocked.model = None;
    blocked.error =
        Some("[audit b.ts] blocked by safety classifier: writes outside the project".into());
    let running = NativeTaskSnapshot {
        task_id: "wf1".into(),
        kind: NativeTaskKind::Workflow,
        provider_session_id: Some("sess-wf".into()),
        tool_use_id: Some("toolu_wf".into()),
        name: Some("audit-routes".into()),
        description: "Audit every route handler for missing auth checks".into(),
        status: NativeTaskStatus::Running,
        phases: vec![
            NativeWorkflowPhase {
                index: 1,
                title: "Discover".into(),
            },
            NativeWorkflowPhase {
                index: 2,
                title: "Audit".into(),
            },
        ],
        agents: vec![
            agent(
                1,
                "list routes",
                1,
                NativeWorkflowAgentState::Done,
                Some(1_400),
            ),
            agent(
                2,
                "audit a.ts",
                2,
                NativeWorkflowAgentState::Running,
                Some(300),
            ),
            blocked,
        ],
        graph_observed: true,
        total_tokens: Some(3_100),
        tool_uses: Some(5),
        duration_ms: Some(9_000),
        last_activity: Some("Audit: audit a.ts".into()),
        revision: 6,
        ..Default::default()
    };
    let completed = NativeTaskSnapshot {
        task_id: "wf0".into(),
        name: Some("deep-research".into()),
        description: "How did the permission model change between v20 and v22?".into(),
        status: NativeTaskStatus::Completed,
        phases: vec![NativeWorkflowPhase {
            index: 1,
            title: "phase 1".into(),
        }],
        agents: (1..=4)
            .map(|i| {
                agent(
                    i,
                    &format!("source {i}"),
                    1,
                    NativeWorkflowAgentState::Done,
                    Some(2_000 * i as u64),
                )
            })
            .collect(),
        graph_observed: true,
        total_tokens: Some(248_000),
        tool_uses: Some(61),
        duration_ms: Some(312_000),
        summary: Some("Dynamic workflow \"deep-research\" completed".into()),
        output_file: Some("/tmp/claude-501/-project/sess/tasks/wf0.output".into()),
        revision: 3,
        ..Default::default()
    };
    let teammate = NativeTaskSnapshot {
        task_id: "tm1".into(),
        kind: NativeTaskKind::Teammate,
        description: "reviewer teammate".into(),
        status: NativeTaskStatus::Stopped,
        end_reason: Some(NativeTaskEndReason::WorkerRestart),
        summary: Some("reviewer teammate".into()),
        revision: 8,
        ..Default::default()
    };
    let abandoned = NativeTaskSnapshot {
        task_id: "wf2".into(),
        name: Some("migrate".into()),
        description: "Long migration".into(),
        status: NativeTaskStatus::Stopped,
        end_reason: Some(NativeTaskEndReason::SessionEnded),
        total_tokens: Some(10),
        revision: 2,
        ..Default::default()
    };
    let mut ledger = NativeTaskLedger::default();
    for task in [abandoned, completed, teammate, running] {
        assert!(ledger.upsert(task));
    }
    ledger
}

/// Row shapes from the real 2.1.286 `claude agents --json` probe.
fn synthetic_sessions() -> ClaudeExternalSessionsView {
    ClaudeExternalSessionsView {
        checked_at_ms: 1_790_864_760_000,
        sessions: vec![
            ClaudeExternalSession {
                kind: ClaudeExternalSessionKind::Interactive,
                session_id: Some("11111111-2222-4333-8444-555555555555".into()),
                name: Some("project-4f".into()),
                cwd: "~/code/project".into(),
                started_at_ms: Some(1_790_864_718_981),
                activity: Some(ClaudeExternalSessionActivity::Idle),
                process_alive: true,
                owned_by_luna: true,
                ..Default::default()
            },
            ClaudeExternalSession {
                kind: ClaudeExternalSessionKind::Background,
                short_id: Some("7c5dcf5d".into()),
                session_id: Some("1f0e2c9a-6d0b-4c11-9f39-2a77c1d4e8b5".into()),
                name: Some("nightly-audit".into()),
                cwd: "~/code/project/.claude/worktrees/nightly-audit".into(),
                started_at_ms: Some(1_790_860_000_000),
                state: Some(ClaudeExternalSessionState::Blocked),
                activity: Some(ClaudeExternalSessionActivity::Waiting),
                waiting_for: Some("permission prompt".into()),
                process_alive: true,
                owned_by_luna: false,
            },
            ClaudeExternalSession {
                kind: ClaudeExternalSessionKind::Interactive,
                session_id: Some("2a9f7c1e-0000-4000-8000-000000000002".into()),
                name: Some("docs".into()),
                cwd: "~/code/docs".into(),
                activity: Some(ClaudeExternalSessionActivity::Busy),
                process_alive: true,
                ..Default::default()
            },
            ClaudeExternalSession {
                kind: ClaudeExternalSessionKind::Background,
                short_id: Some("9bc63a21".into()),
                session_id: Some("9bc63a21-6fa6-420e-a00f-38b62bfbd78b".into()),
                name: Some("cc13-probe".into()),
                cwd: "~/probes/bg/project".into(),
                state: Some(ClaudeExternalSessionState::Stopped),
                ..Default::default()
            },
        ],
        unavailable_reason: None,
        truncated: false,
    }
}

fn act(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut Window, &mut gpui::App),
) -> anyhow::Result<()> {
    window.update(cx, |_, window, cx| f(window, cx))
}

async fn shot(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    output: &Path,
    name: &str,
) -> anyhow::Result<()> {
    act(window, cx, |window, cx| {
        window.draw(cx).clear();
    })?;
    cx.background_executor()
        .timer(Duration::from_millis(400))
        .await;
    let path = output.join(name);
    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(&path)?;
        Ok(())
    })??;
    Ok(())
}

#[cfg(target_os = "macos")]
fn activate_accessibility(window: &Window) -> anyhow::Result<()> {
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
fn activate_accessibility(_: &Window) -> anyhow::Result<()> {
    anyhow::bail!("AccessKit activation readback is implemented for macOS only")
}

fn a11y_text(window: AnyWindowHandle, cx: &mut AsyncApp) -> anyhow::Result<String> {
    for _ in 0..2 {
        act(window, cx, |window, cx| {
            window.draw(cx).clear();
        })?;
    }
    window
        .update(cx, |_, window, _| window.debug_a11y_tree_json())?
        .ok_or_else(|| anyhow::anyhow!("AccessKit tree was not built (activation failed)"))
}

fn open(
    cx: &mut gpui::App,
    root: Root,
    width: f32,
    height: f32,
) -> anyhow::Result<(AnyWindowHandle, gpui::Entity<Root>)> {
    let entity = cx.new(|_| root);
    let view = entity.clone();
    let window = cx.open_window(
        WindowOptions {
            window_background: gpui::WindowBackgroundAppearance::Opaque,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                gpui::point(px(20.0), px(20.0)),
                size(px(width), px(height)),
            ))),
            show: false,
            focus: false,
            ..Default::default()
        },
        move |_, _| view,
    )?;
    Ok((window.into(), entity))
}

async fn run(
    tasks: (AnyWindowHandle, gpui::Entity<Root>),
    settings: (AnyWindowHandle, gpui::Entity<Root>),
    output: &Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<serde_json::Value> {
    let (tasks_window, _) = tasks;
    let (settings_window, settings_root) = settings;
    act(tasks_window, cx, |window, _| {
        activate_accessibility(window).expect("activate accessibility");
    })?;
    shot(tasks_window, cx, output, "01-native-tasks-dark.png").await?;
    let tasks_tree = a11y_text(tasks_window, cx)?;
    for expected in [
        "Native workflows and teammates: 4 total, 1 active",
        "Workflow · audit-routes, Running",
        "Workflow · deep-research, Completed",
        "Teammate (experimental), Stopped · worker restarted",
        "Workflow · migrate, Stopped · Claude process ended",
    ] {
        anyhow::ensure!(
            tasks_tree.contains(expected),
            "tasks a11y tree lacks {expected:?}"
        );
    }

    act(settings_window, cx, |window, _| {
        activate_accessibility(window).expect("activate accessibility");
    })?;
    shot(
        settings_window,
        cx,
        output,
        "02-ledger-and-sessions-dark.png",
    )
    .await?;
    let settings_tree = a11y_text(settings_window, cx)?;
    for expected in [
        "Claude Code advanced features in LUNA",
        "Dynamic workflow runs: Supported · live QA pending",
        "Stop a workflow: Unsupported in LUNA",
        "Agent teams: Not enabled by LUNA",
        "Claude in Chrome: Native Claude Code only",
        "Computer use: Externally gated",
        "Refresh Claude sessions outside LUNA",
        "nightly-audit · 7c5dcf5d. Background session · blocked · waiting for permission prompt",
        "project-4f. LUNA chat process · idle",
        "cc13-probe · 9bc63a21. Background session · stopped · not running",
    ] {
        anyhow::ensure!(
            settings_tree.contains(expected),
            "settings a11y tree lacks {expected:?}"
        );
    }

    // Light theme, explicit unavailability (never shown as "no sessions").
    act(settings_window, cx, |_, cx| {
        appearance::set_mode(appearance::AppearanceMode::Light, cx);
        settings_root.update(cx, |root, cx| {
            root.sessions = ExternalSessionsState::Ready(ClaudeExternalSessionsView {
                checked_at_ms: 1,
                unavailable_reason: Some(
                    "This Claude Code version did not list background sessions (`claude agents --json` failed)."
                        .into(),
                ),
                ..Default::default()
            });
            cx.notify();
        });
    })?;
    shot(
        settings_window,
        cx,
        output,
        "03-sessions-unavailable-light.png",
    )
    .await?;
    let unavailable_tree = a11y_text(settings_window, cx)?;
    anyhow::ensure!(!unavailable_tree.contains("nightly-audit"));
    act(tasks_window, cx, |window, cx| {
        window.refresh();
        let _ = cx;
    })?;
    shot(tasks_window, cx, output, "04-native-tasks-light.png").await?;
    std::fs::write(output.join("a11y-tasks.json"), &tasks_tree)?;
    std::fs::write(output.join("a11y-settings.json"), &settings_tree)?;
    Ok(serde_json::json!({
        "shots": [
            "01-native-tasks-dark.png",
            "02-ledger-and-sessions-dark.png",
            "03-sessions-unavailable-light.png",
            "04-native-tasks-light.png"
        ],
        "ledgerRows": claude_advanced::CLAUDE_ADVANCED_LEDGER.len(),
        "a11y": ["a11y-tasks.json", "a11y-settings.json"],
        "scope": "headless GPUI render of production claude_advanced functions with synthetic data; no engine, provider or app"
    }))
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
        anyhow::anyhow!("usage: claude-coordination-fixture ABSOLUTE_EVIDENCE_DIR")
    })?);
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in [
        "claude-coordination.json",
        "claude-coordination.pass",
        "failure.txt",
    ] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let profile = tempfile::tempdir()?;
    let data = profile.path().join("ui");
    std::fs::create_dir_all(&data)?;
    let failure = Arc::new(Mutex::new(None::<String>));
    let failure_for_ui = failure.clone();
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
            theme_library::init(data, cx);
            appearance::init(
                appearance::AppearanceMode::Dark,
                settings.theme_selection,
                settings.accent,
                settings.surface,
                cx,
            );
            let windows = open(
                cx,
                Root {
                    shot: Shot::Tasks,
                    ledger: synthetic_ledger(),
                    sessions: ExternalSessionsState::Idle,
                },
                480.0,
                1_120.0,
            )
            .and_then(|tasks| {
                let settings = open(
                    cx,
                    Root {
                        shot: Shot::Settings,
                        ledger: NativeTaskLedger::default(),
                        sessions: ExternalSessionsState::Ready(synthetic_sessions()),
                    },
                    720.0,
                    1_560.0,
                )?;
                Ok((tasks, settings))
            });
            let (tasks, settings) = match windows {
                Ok(windows) => windows,
                Err(error) => {
                    eprintln!("Claude coordination fixture failed: open window: {error:#}");
                    let _ = std::fs::write(output.join("failure.txt"), format!("open window: {error:#}\n"));
                    drop(profile);
                    std::process::exit(1);
                }
            };
            cx.spawn(async move |cx| {
                let result: anyhow::Result<()> = async {
                    let evidence = run(tasks, settings, &output, cx).await?;
                    std::fs::write(
                        output.join("claude-coordination.json"),
                        serde_json::to_vec_pretty(&evidence)?,
                    )?;
                    std::fs::write(
                        output.join("claude-coordination.pass"),
                        "PASS: headless GPUI render + AccessKit labels for native tasks, ledger and external sessions; synthetic data, no provider proof\n",
                    )?;
                    Ok(())
                }
                .await;
                for window in [tasks.0, settings.0] {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                }
                if let Err(error) = result {
                    eprintln!("Claude coordination fixture failed: {error:#}");
                    let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                    *failure_for_ui.lock().unwrap() = Some(error.to_string());
                    drop(profile);
                    std::process::exit(1);
                }
                drop(profile);
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    if let Some(error) = failure.lock().unwrap().take() {
        anyhow::bail!(error);
    }
    Ok(())
}
