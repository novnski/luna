//! Headless GPUI E2E for "From GitHub issue" (A7): the production `Shell`
//! in a hidden window over the Board page, a real in-process `EngineCore`
//! (Mock harness) and the deterministic fake `gh`
//! (`scripts/fixtures/board-github/gh`) first on PATH. Real pointer and
//! keyboard input through GPUI hit testing; painted control bounds come from
//! layout probes (`perf_counters`).
//!
//! Failure cases this run must catch (written before the implementation):
//! 1. The picker does not open over the Board page for the project, or it
//!    assumes a repository when the checkout has fork + upstream remotes (a
//!    source selector must ask first; nothing is listed before a choice).
//! 2. Typing a query and pressing Enter does not search, or a superseded
//!    search's late result overwrites the newer one.
//! 3. A row lacks number/title/labels/assignees/state/updated, a pull
//!    request is listed, or freshness/partial notices are missing.
//! 4. The preview lacks the body, canonical link, selectable comments,
//!    truncation notice or the editable initial prompt.
//! 5. Add to board does not create exactly one card in the Board page's
//!    model, or a second click can import again.
//! 6. After adding, the row and the preview do not show "On board" / Open
//!    card, or Add to board stays enabled.
//! 7. Any GitHub write happens during the flow (the fake records none).
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example board-github-fixture
//! Run:   LUNA_PERF_COUNTERS=1 target/debug/examples/board-github-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes PNG captures, readback.json and board-github.pass; the manifest is
//! mirrored to target/qa/board/github/fixture-manifest.json. A failure writes
//! failure.txt/failure.png and exits nonzero.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    AppContext, AsyncApp, Bounds, Keystroke, MouseButton, PlatformInput, WindowBounds,
    WindowHandle, WindowOptions, point, px, size,
};
use luna_engine::{
    AgentAccountsConfig, Auth, AuthConfig, EngineCore, EngineProfile, HarnessRegistry,
};
use luna_harness::mock::MockHarness;
use luna_proto::{AgentEvent, DoneStatus, HarnessId};
use luna_ui::*;
use sha2::{Digest, Sha256};

const PROJECT: &str = "proj-widgets";
const ACME: &str = "github:github.com:acme/widgets";
const PROBES: &str = "board-gh:";
const WIDTH: f32 = 1440.0;
const HEIGHT: f32 = 900.0;

type Win = WindowHandle<shell::Shell>;

fn git(root: &Path, args: &[&str]) -> anyhow::Result<()> {
    let out = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

fn capture(window: Win, cx: &mut AsyncApp, path: PathBuf) -> anyhow::Result<String> {
    let window = gpui::AnyWindowHandle::from(window);
    window.update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(&path)?;
        Ok(())
    })??;
    Ok(format!("{:x}", Sha256::digest(std::fs::read(&path)?)))
}

fn draw(window: Win, cx: &mut AsyncApp) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
    })?;
    Ok(())
}

fn input(window: Win, cx: &mut AsyncApp, event: PlatformInput) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(event, cx);
    })?;
    Ok(())
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// Real pointer input through GPUI hit testing.
async fn click_at(window: Win, cx: &mut AsyncApp, (x, y): (f32, f32)) -> anyhow::Result<()> {
    let position = point(px(x), px(y));
    input(
        window,
        cx,
        PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Default::default(),
        }),
    )?;
    draw(window, cx)?;
    settle(cx, 60).await;
    input(
        window,
        cx,
        PlatformInput::MouseDown(gpui::MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        }),
    )?;
    input(
        window,
        cx,
        PlatformInput::MouseUp(gpui::MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Default::default(),
            click_count: 1,
        }),
    )?;
    settle(cx, 200).await;
    Ok(())
}

async fn keys(window: Win, cx: &mut AsyncApp, combos: &[&str]) -> anyhow::Result<()> {
    for combo in combos {
        let keystroke = Keystroke::parse(combo)?;
        gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
            window.dispatch_keystroke(keystroke, cx);
        })?;
        draw(window, cx)?;
        settle(cx, 30).await;
    }
    settle(cx, 150).await;
    Ok(())
}

async fn type_text(window: Win, cx: &mut AsyncApp, text: &str) -> anyhow::Result<()> {
    let combos: Vec<String> = text
        .chars()
        .map(|c| {
            if c == ' ' {
                "space".into()
            } else {
                c.to_string()
            }
        })
        .collect();
    let combos: Vec<&str> = combos.iter().map(String::as_str).collect();
    keys(window, cx, &combos).await
}

fn readback(window: Win, cx: &mut AsyncApp) -> anyhow::Result<serde_json::Value> {
    window.update(cx, |shell, _, cx| shell.fixture_board_github_readback(cx))
}

/// The painted bounds of every probe, freshly re-rendered.
fn probes(window: Win, cx: &mut AsyncApp) -> anyhow::Result<Vec<(String, [f32; 4])>> {
    perf_counters::clear_bounds(PROBES);
    window.update(cx, |shell, _, cx| shell.fixture_board_github_repaint(cx))?;
    draw(window, cx)?;
    Ok(perf_counters::probes(PROBES))
}

fn center_of(probes: &[(String, [f32; 4])], name: &str) -> anyhow::Result<(f32, f32)> {
    let full = format!("{PROBES}{name}");
    probes
        .iter()
        .find(|(probe, _)| *probe == full)
        .map(|(_, [x, y, w, h])| (x + w / 2.0, y + h / 2.0))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "probe {full} not painted; painted: {:?}",
                probes.iter().map(|(name, _)| name).collect::<Vec<_>>()
            )
        })
}

async fn click_probe(window: Win, cx: &mut AsyncApp, name: &str) -> anyhow::Result<()> {
    let painted = probes(window, cx)?;
    let center = center_of(&painted, name)?;
    click_at(window, cx, center).await
}

/// Scroll the preview with the wheel until `name` is inside its visible
/// area, then click it (a person scrolls before clicking).
async fn click_in_preview(window: Win, cx: &mut AsyncApp, name: &str) -> anyhow::Result<()> {
    // Wheel direction conventions differ; flip when a step moved it away.
    let mut step = -120.0_f32;
    let mut last_distance = f32::MAX;
    for _ in 0..24 {
        let painted = probes(window, cx)?;
        let area = painted
            .iter()
            .find(|(probe, _)| probe == &format!("{PROBES}preview-scroll"))
            .map(|(_, bounds)| *bounds)
            .ok_or_else(|| anyhow::anyhow!("preview area not painted"))?;
        let (x, y) = center_of(&painted, name)?;
        let (top, bottom) = (area[1] + 8.0, area[1] + area[3] - 8.0);
        if y > top && y < bottom {
            return click_at(window, cx, (x, y)).await;
        }
        let distance = if y >= bottom { y - bottom } else { top - y };
        if distance > last_distance {
            step = -step;
        }
        last_distance = distance;
        input(
            window,
            cx,
            PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                position: point(px(area[0] + area[2] / 2.0), px(area[1] + area[3] / 2.0)),
                delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(step))),
                modifiers: Default::default(),
                touch_phase: gpui::TouchPhase::Moved,
            }),
        )?;
        draw(window, cx)?;
        settle(cx, 80).await;
    }
    anyhow::bail!("{name} never scrolled into the preview")
}

/// Wait until the readback satisfies `done` (engine round trips are async).
async fn until(
    window: Win,
    cx: &mut AsyncApp,
    what: &str,
    done: impl Fn(&serde_json::Value) -> bool,
) -> anyhow::Result<serde_json::Value> {
    let mut last = serde_json::Value::Null;
    for _ in 0..150 {
        last = readback(window, cx)?;
        if done(&last) {
            return Ok(last);
        }
        settle(cx, 100).await;
    }
    anyhow::bail!("{what} never happened: {last}")
}

fn row_numbers(readback: &serde_json::Value) -> Vec<u64> {
    readback["picker"]["rows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["number"].as_u64())
                .collect()
        })
        .unwrap_or_default()
}

fn lines(path: &Path) -> usize {
    std::fs::read_to_string(path).map_or(0, |text| text.lines().count())
}

fn main() -> anyhow::Result<()> {
    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("usage: board-github-fixture ABSOLUTE_EVIDENCE_DIR"))?,
    );
    anyhow::ensure!(output.is_absolute(), "evidence directory must be absolute");
    std::fs::create_dir_all(&output)?;
    for stale in [
        "readback.json",
        "board-github.pass",
        "failure.txt",
        "failure.png",
    ] {
        let _ = std::fs::remove_file(output.join(stale));
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/fixtures/board-github")
        .canonicalize()?;
    let old_path = std::env::var("PATH").unwrap_or_default();
    // SAFETY: set once before any runtime, window or subprocess exists.
    unsafe {
        std::env::set_var("PATH", format!("{}:{old_path}", fixture.display()));
        std::env::set_var("LUNA_NO_LOGIN_SHELL", "1");
        std::env::set_var("LUNA_PERF_COUNTERS", "1");
        std::env::set_var("LUNA_OPEN_ROUTE", "board");
    }
    perf_counters::set_probes_enabled(true);

    let root = tempfile::tempdir()?;
    let ui_data = root.path().join("ui");
    let engine_data = root.path().join("engine");
    let state_dir = root.path().join("fake-gh-state");
    let project = root.path().join("projects/widgets");
    for dir in [&ui_data, &engine_data, &state_dir, &project] {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::copy(fixture.join("recorded.json"), state_dir.join("state.json"))?;
    std::fs::write(state_dir.join("account"), "octo-a")?;
    std::fs::write(state_dir.join("mode"), "ok")?;
    git(&project, &["init", "-q", "-b", "main"])?;
    git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:octo-a/widgets.git",
        ],
    )?;
    git(
        &project,
        &[
            "remote",
            "add",
            "upstream",
            "https://github.com/acme/widgets.git",
        ],
    )?;

    let runtime = tokio::runtime::Runtime::new()?;
    let registry = HarnessRegistry::new();
    registry.register(Arc::new(MockHarness {
        script: vec![AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: None,
        }],
    }));
    let core = runtime.block_on(async {
        EngineCore::assemble_with_profile_and_agent_accounts_config(
            EngineProfile::development(
                &engine_data,
                "board-github-fixture",
                "board-github-fixture",
            ),
            Arc::new(registry),
            HarnessId::Mock,
            None,
            AgentAccountsConfig {
                data_dir: engine_data.clone(),
                claude_config_dir: root.path().join("providers/claude"),
                claude_config_file: root.path().join("providers/claude.json"),
                codex_home: root.path().join("providers/codex"),
            },
        )
    })?;
    let mut auth = AuthConfig::new("", root.path().join("auth"));
    auth.dev_user_id = "board-github-fixture".into();
    auth.use_in_memory_credentials_for_tests();
    core.set_auth(Auth::new(auth));
    let device = core.device_id.clone();
    core.workspace.create_space(
        PROJECT,
        &device,
        project.to_str().unwrap(),
        Some("Widgets".into()),
        true,
    )?;
    let spaces = core.workspace.read_spaces()?;
    let handle = runtime.block_on(state::EngineHandle::fixture_from_core(&core))?;
    let boot = EngineBootConfig {
        data_dir: ui_data.clone(),
        ipc_port: 59994,
        edge_url: String::new(),
        edge_token: None,
        org_id: None,
        workos_client_id: None,
        default_harness: HarnessId::Mock,
    };
    let failure = Arc::new(Mutex::new(None::<String>));
    let failure_for_ui = failure.clone();
    let writes_log = state_dir.join("writes.jsonl");
    let calls_log = state_dir.join("calls.jsonl");
    gpui_platform::headless().with_assets(icons::Assets).run(move |cx| {
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
        appearance::init(appearance::AppearanceMode::Dark, settings.theme_selection, settings.accent, settings.surface, cx);
        history::init(
            settings.git_history_columns,
            settings.git_history_column_widths,
            settings.git_history_column_order,
            settings.git_history_author_display,
            cx,
        );
        composer::init(cx, settings.composer_send_behavior);
        terminal::panel::init(cx);
        let state = cx.new(|_| {
            let mut state = state::AppState::new();
            state.local_device_id = Some(device.clone());
            state.devices = vec![
                serde_json::from_value(serde_json::json!({
                    "id": device, "name": "Studio Mac", "platform": "macos", "lastSeenAt": null
                }))
                .unwrap(),
            ];
            state.spaces = spaces;
            state.chats_synced = true;
            state.spaces_synced = true;
            state.auto_selected = true;
            state.connection = state::ConnectionStatus::Ready;
            state.workspace_scope = Some(luna_proto::WorkspaceScope::Development);
            state.fixture_attachment_engine(handle);
            state
        });
        let window: Win = cx
            .open_window(
                WindowOptions {
                    window_background: theme::Theme::of(cx).window_background_appearance(),
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(20.), px(20.)),
                        size(px(WIDTH), px(HEIGHT)),
                    ))),
                    show: false,
                    focus: false,
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| shell::Shell::new(state, boot, cx)),
            )
            .unwrap();
        cx.spawn(async move |cx| {
            let result: anyhow::Result<serde_json::Value> = async {
                let mut record = serde_json::Map::new();
                let mut captures = serde_json::Map::new();
                settle(cx, 900).await;
                let booted = readback(window, cx)?;
                anyhow::ensure!(booted["route"].as_str().unwrap_or("").contains("Board"), "the Board page is open: {booted}");

                // 1. Open over the Board page; two remotes → explicit choice.
                window.update(cx, |shell, _, cx| shell.open_board_github_picker(PROJECT.into(), cx))?;
                let opened = until(window, cx, "sources resolve", |r| r["picker"]["sources"]["state"] == "ready").await?;
                anyhow::ensure!(opened["picker"]["sources"]["ids"].as_array().map(Vec::len) == Some(2), "fork + upstream listed: {opened}");
                anyhow::ensure!(opened["picker"]["sources"]["defaults"] == 0 && opened["picker"]["sourceId"].is_null(), "no repository assumed: {opened}");
                anyhow::ensure!(row_numbers(&opened).is_empty(), "nothing listed before a choice: {opened}");
                captures.insert("01-choose-source".into(), capture(window, cx, output.join("01-choose-source.png"))?.into());
                record.insert("opened".into(), opened);

                click_probe(window, cx, &format!("source:{ACME}")).await?;
                let listed = until(window, cx, "issues list", |r| !row_numbers(r).is_empty()).await?;
                // 3. Rows carry the fields; PRs never listed and disclosed.
                anyhow::ensure!(row_numbers(&listed) == vec![12, 11, 10, 9, 8, 7, 4, 3, 1], "open issues by update, PRs excluded: {listed}");
                anyhow::ensure!(listed["picker"]["pullRequestsExcluded"] == 2, "PRs counted: {listed}");
                let one = listed["picker"]["rows"].as_array().unwrap().iter().find(|row| row["number"] == 1).cloned().unwrap();
                anyhow::ensure!(one["labels"] == serde_json::json!(["bug", "search"]) && one["assignees"] == serde_json::json!(["alice"]) && one["state"] == "open" && one["updatedAt"] == "2026-10-01T10:00:00Z", "row fields: {one}");
                anyhow::ensure!(listed["picker"]["page"]["freshness"]["account"] == "octo-a", "freshness names the account: {listed}");
                record.insert("listed".into(), listed);

                // 2. Search: the latest query wins.
                click_probe(window, cx, "search").await?;
                type_text(window, cx, "flaky").await?;
                keys(window, cx, &["enter"]).await?;
                keys(window, cx, &["backspace", "backspace", "backspace", "backspace", "backspace"]).await?;
                type_text(window, cx, "archived").await?;
                keys(window, cx, &["enter"]).await?;
                let searched = until(window, cx, "search results", |r| r["picker"]["query"] == "archived" && r["picker"]["searching"] == false && row_numbers(r) == vec![1]).await?;
                settle(cx, 800).await;
                let settled = readback(window, cx)?;
                anyhow::ensure!(row_numbers(&settled) == vec![1], "a superseded search must not overwrite: {settled}");
                record.insert("searched".into(), searched);

                // 4. Preview with link, body, comments, truncation, prompt.
                click_probe(window, cx, "row:1").await?;
                let previewed = until(window, cx, "preview", |r| r["picker"]["detail"]["number"] == 1).await?;
                anyhow::ensure!(previewed["picker"]["detail"]["url"] == "https://github.com/acme/widgets/issues/1", "canonical link: {previewed}");
                anyhow::ensure!(previewed["picker"]["detail"]["omitted"].as_str().unwrap_or("").contains("image/attachment"), "attachment notice: {previewed}");
                click_in_preview(window, cx, "load-comments").await?;
                let commented = until(window, cx, "comments", |r| r["picker"]["comments"].as_array().map(Vec::len) == Some(3)).await?;
                record.insert("comments".into(), commented);
                click_in_preview(window, cx, "comment:7001").await?;
                click_probe(window, cx, "prompt").await?;
                type_text(window, cx, "Respect the Include archived toggle.").await?;
                let ready = until(window, cx, "selection and prompt", |r| r["picker"]["selectedComments"] == serde_json::json!(["7001"]) && r["picker"]["prompt"] == "Respect the Include archived toggle.").await?;
                anyhow::ensure!(ready["picker"]["addEnabled"] == true && ready["picker"]["openCard"].is_null(), "Add to board offered: {ready}");
                captures.insert("02-preview".into(), capture(window, cx, output.join("02-preview.png"))?.into());
                record.insert("preview".into(), ready);

                // 5. Add to board: exactly one card on the Board page.
                click_probe(window, cx, "add").await?;
                let added = until(window, cx, "import", |r| r["picker"]["lastImport"]["outcome"] == "created").await?;
                let card = added["picker"]["lastImport"]["workItemId"].clone();
                let on_board = until(window, cx, "card on the board", |r| r["cards"].as_array().map(Vec::len) == Some(1)).await?;
                anyhow::ensure!(on_board["cards"][0]["id"] == card && on_board["cards"][0]["origin"]["kind"] == "issue", "the imported card: {on_board}");
                // 6. On board: marker in the row and Open card in the preview.
                let marked = until(window, cx, "on-board marker", |r| {
                    r["picker"]["rows"].as_array().is_some_and(|rows| rows.iter().any(|row| row["number"] == 1 && row["onBoard"] == card))
                        && r["picker"]["openCard"] == card
                }).await?;
                anyhow::ensure!(marked["picker"]["addEnabled"] == false, "Add to board is gone: {marked}");
                let painted = probes(window, cx)?;
                anyhow::ensure!(center_of(&painted, "add").is_err() && center_of(&painted, "open-card").is_ok(), "Open card replaces Add to board: {painted:?}");
                captures.insert("03-on-board".into(), capture(window, cx, output.join("03-on-board.png"))?.into());
                settle(cx, 500).await;
                let after = readback(window, cx)?;
                anyhow::ensure!(after["cards"].as_array().map(Vec::len) == Some(1), "still one card: {after}");
                record.insert("onBoard".into(), marked);

                // 7. No GitHub write during the whole flow.
                anyhow::ensure!(lines(&writes_log) == 0, "GitHub writes recorded: {}", std::fs::read_to_string(&writes_log).unwrap_or_default());
                record.insert("fakeGh".into(), serde_json::json!({"calls": lines(&calls_log), "writes": 0}));
                record.insert("captures".into(), serde_json::Value::Object(captures));
                Ok(serde_json::Value::Object(record))
            }
            .await;
            match result {
                Ok(record) => {
                    let body = serde_json::to_vec_pretty(&record).unwrap_or_default();
                    let manifest = serde_json::json!({
                        "suite": "board-github-fixture",
                        "contract": "docs/design/project-board/pb0-contract.md",
                        "recordedAtMs": chrono::Utc::now().timestamp_millis(),
                        "evidenceDir": output,
                        "readbackSha256": format!("{:x}", Sha256::digest(&body)),
                        "captures": record["captures"],
                        "passed": true,
                    });
                    let qa = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/qa/board/github");
                    let written = std::fs::write(output.join("readback.json"), &body)
                        .and_then(|_| std::fs::create_dir_all(&qa))
                        .and_then(|_| std::fs::write(qa.join("fixture-manifest.json"), serde_json::to_vec_pretty(&manifest).unwrap_or_default()))
                        .and_then(|_| std::fs::write(
                            output.join("board-github.pass"),
                            "PASS: production Shell over the Board page; fork + upstream remotes require an explicit repository; issues listed with fields and PRs excluded; the latest search wins; preview shows the canonical link, body, attachment notice, selectable comments and the editable prompt; Add to board creates exactly one card; the row and preview then show On board / Open card; no GitHub writes\n",
                        ));
                    if let Err(error) = written {
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                        std::process::exit(1);
                    }
                }
                Err(error) => {
                    eprintln!("Board GitHub fixture failed: {error:#}");
                    let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                    let _ = capture(window, cx, output.join("failure.png"));
                    *failure_for_ui.lock().unwrap() = Some(error.to_string());
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
        eprintln!("{error}");
        std::process::exit(1);
    }
    Ok(())
}
