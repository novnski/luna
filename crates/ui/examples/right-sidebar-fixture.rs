//! Headless GPUI E2E for the right sidebar redesign, Option B
//! (docs/design/right-sidebar-redesign-2026-10-11): every place has a fixed
//! spot on a rail at the window's right edge. Clicking a place opens the
//! sidebar there, clicking the active one closes it, and the rail with its
//! badges stays while the sidebar is closed. A workspace place lists its
//! open instances in its own tab row. Nothing navigates back.
//!
//! Drives the production `Shell` in a hidden window with real pointer and
//! keyboard input against synthetic AppState rows, transcript and Activity
//! snapshot (no engine, provider, account or credential). An opt-in probe
//! records what the right sidebar painted and where.
//!
//! Failure cases this run must catch (written before the implementation; the
//! spec lists the same eleven):
//! 1. Opening the sidebar on a chat shows anything other than the Overview
//!    place, or the rail is missing.
//! 2. The rail misses a place or orders them wrongly (Overview, Agents,
//!    Related, Automations | Files, Changes, Terminal, Browser), or is not at
//!    the window's right edge with the sidebar ending where it starts.
//! 3. Clicking a place does not open it; clicking the active place does not
//!    close the sidebar; the rail disappears with the sidebar closed.
//! 4. With the sidebar closed, a running subagent does not badge Agents or a
//!    related chat waiting for approval does not badge Related.
//! 5. A Back state exists: a state visited is neither a place's own page nor
//!    a tab of the place the rail shows.
//! 6. Changes does not open a working-tree diff on its first visit with Git,
//!    History does not open as another Changes tab, returning to Changes
//!    does not show the tab it last showed, or without Git the reason is
//!    missing.
//! 7. A place's tab row lists another place's tabs.
//! 8. Closing a place's last tab leaves anything but that place's empty state
//!    with a way to open one.
//! 9. A former entry point (the related count, Agents, Automations) opens
//!    anything but its place, or creates a tab for it.
//! 10. Clicking a related chat renders that conversation inside the sidebar.
//! 11. An Overview row does not open its place.
//!
//! Build: scripts/cargo.sh build --locked -p luna-ui --features appshots-fixture \
//!   --example right-sidebar-fixture
//! Run:   target/debug/examples/right-sidebar-fixture ABSOLUTE_EVIDENCE_DIR
//! Writes PNG captures per step, readback.json and right-sidebar.pass; a
//! failure writes failure.txt/failure.png and exits nonzero.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use gpui::{
    AppContext, AsyncApp, Bounds, Entity, MouseButton, PlatformInput, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
};
use luna_doc::{MessagePart, MessageRole, SessionMessageEntry, SubagentStatus};
use luna_proto::{
    ATTENTION_CONTRACT_VERSION, AttentionItem, AttentionKind, AttentionLifecycle,
    AttentionSeverity, AttentionSnapshot, AttentionSourcePointer, AttentionTarget, ChatConfig,
    ChildThread, HarnessId, PermissionMode, SandboxLevel, ToolCall,
};
use luna_ui::*;
use serde_json::{Value, json};

const DEVICE: &str = "studio";
const P_GIT: &str = "proj-luna";
const P_PLAIN: &str = "proj-notes";
const PARENT: &str = "rs-parent";
const PLAIN: &str = "rs-plain";
const CHILDREN: usize = 4;
const WIDTH: f32 = 1440.0;
const HEIGHT: f32 = 900.0;

type Window = WindowHandle<shell::Shell>;

fn child_id(ix: usize) -> String {
    format!("rs-child-{ix:02}")
}

fn config() -> ChatConfig {
    ChatConfig {
        harness: HarnessId::ClaudeCode,
        provider_account_id: None,
        model: Some("synthetic-model".into()),
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

fn chat(id: &str, title: &str, project: &str, minutes_ago: i64) -> luna_proto::Chat {
    let at = Utc::now() - TimeDelta::minutes(minutes_ago);
    luna_proto::Chat {
        child_thread: None,
        id: id.into(),
        device_id: DEVICE.into(),
        title: Some(title.into()),
        archived: false,
        settlement_override: None,
        pinned_order: None,
        cwd: None,
        branch: Some("luna/settings-migration".into()),
        checkout_id: None,
        source_context: None,
        config: Some(config()),
        last_message_preview: None,
        last_message_at: Some(at),
        created_at: at - TimeDelta::minutes(3),
        harness_session_id: None,
        harness_session_cwd: None,
        space_id: Some(project.into()),
        last_seen_at: Some(at + TimeDelta::seconds(1)),
        manually_unread: false,
        room_gen: None,
    }
}

fn space(id: &str, name: &str, git: bool) -> luna_proto::Space {
    luna_proto::Space {
        id: id.into(),
        device_id: DEVICE.into(),
        path: format!("/synthetic/{name}"),
        name: Some(name.into()),
        git_detected: git,
        git_checked_at: None,
        checkout_id: None,
        execution_defaults: Default::default(),
        created_at: Utc::now() - TimeDelta::days(30),
        removed_at: None,
    }
}

fn seed_chats() -> Vec<luna_proto::Chat> {
    let mut chats = vec![
        chat(PARENT, "Settings migration", P_GIT, 1),
        chat(PLAIN, "Meeting notes", P_PLAIN, 2),
    ];
    for ix in 0..CHILDREN {
        let mut child = chat(
            &child_id(ix),
            &format!("Migrate settings call site {}", ix + 1),
            P_GIT,
            3 + ix as i64,
        );
        let at_ms = child.created_at.timestamp_millis();
        child.child_thread = Some(ChildThread {
            work_origin: None,
            parent_task_id: PARENT.into(),
            creation_command_id: format!("fixture-create-{ix}"),
            permission_ceiling: Default::default(),
            sandbox_ceiling: None,
            initial_message_id: format!("fixture-prompt-{ix}"),
            initial_prompt: "Move one call site to the v3 store (fixture)".into(),
            report_results: false,
            continue_parent: false,
            outcomes: Vec::new(),
            outcome_floor_ms: Some(at_ms),
            provisioning: None,
        });
        chats.push(child);
    }
    chats
}

/// A related chat waiting for the user's approval.
fn snapshot() -> AttentionSnapshot {
    let at = Utc::now().timestamp_millis();
    let task_id = child_id(0);
    let item = AttentionItem {
        contract_version: ATTENTION_CONTRACT_VERSION,
        id: format!("{task_id}-approval"),
        dedup_key: format!("{task_id}-approval"),
        source_key: "synthetic".into(),
        suppress_notification: false,
        kind: AttentionKind::ApprovalRequired,
        severity: AttentionSeverity::Info,
        lifecycle: AttentionLifecycle::Unresolved,
        environment_id: DEVICE.into(),
        device_id: DEVICE.into(),
        task_id: Some(task_id.clone()),
        turn_id: None,
        request_id: None,
        title: "Synthetic".into(),
        summary: "Synthetic".into(),
        created_at_ms: at,
        updated_at_ms: at,
        seen_at_ms: None,
        acknowledged_at_ms: None,
        resolved_at_ms: None,
        target: AttentionTarget::Task {
            task_id: task_id.clone(),
        },
        source: AttentionSourcePointer {
            task_id: Some(task_id),
            message_id: None,
            part_id: None,
            request_id: None,
            queue_id: None,
            recovery_id: None,
            activity_event_id: None,
        },
    };
    AttentionSnapshot {
        contract_version: ATTENTION_CONTRACT_VERSION,
        revision: 1,
        read_at_ms: at,
        unresolved_count: 1,
        needs_attention: vec![item],
        recent: Vec::new(),
        producers: Vec::new(),
    }
}

fn spawn(id: &str, name: &str, status: SubagentStatus) -> MessagePart {
    MessagePart::Tool {
        id: id.into(),
        call: ToolCall::Unknown {
            name: format!("Agent: {name}"),
            input: None,
        },
        is_error: false,
        resolved: true,
        output: None,
        diff: None,
        output_ref: None,
        output_bytes: None,
        diff_ref: None,
        diff_stats: None,
        subagent_ref: None,
        subagent_status: Some(status),
        subagent_tail: None,
        subagent_activity: None,
    }
}

/// One running subagent and one finished one, on the parent's transcript.
fn transcript() -> Vec<SessionMessageEntry> {
    vec![SessionMessageEntry {
        id: "fixture-assistant".into(),
        role: MessageRole::Assistant,
        parts: vec![
            spawn(
                "toolu_running",
                "Explore call sites",
                SubagentStatus::Running,
            ),
            spawn("toolu_done", "Audit settings keys", SubagentStatus::Done),
        ],
        created_at: Utc::now().timestamp_millis(),
        device_id: DEVICE.into(),
        status: None,
        continuation_of: None,
    }]
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

fn draw(window: Window, cx: &mut AsyncApp) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.draw(cx).clear();
    })?;
    Ok(())
}

async fn frames(window: Window, cx: &mut AsyncApp, ms: u64) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        draw(window, cx)?;
        settle(cx, 16).await;
    }
    draw(window, cx)
}

fn capture(window: Window, cx: &mut AsyncApp, path: PathBuf) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| -> anyhow::Result<()> {
        window.draw(cx).clear();
        window.render_to_image()?.save(path)?;
        Ok(())
    })??;
    Ok(())
}

fn input(window: Window, cx: &mut AsyncApp, event: PlatformInput) -> anyhow::Result<()> {
    gpui::AnyWindowHandle::from(window).update(cx, |_, window, cx| {
        window.dispatch_event(event, cx);
    })?;
    Ok(())
}

fn hover_at(window: Window, cx: &mut AsyncApp, x: f32, y: f32) -> anyhow::Result<()> {
    input(
        window,
        cx,
        PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position: point(px(x), px(y)),
            pressed_button: None,
            modifiers: Default::default(),
        }),
    )
}

async fn press_at(
    window: Window,
    cx: &mut AsyncApp,
    (x, y): (f32, f32),
    button: MouseButton,
) -> anyhow::Result<()> {
    hover_at(window, cx, x, y)?;
    frames(window, cx, 60).await?;
    let position = point(px(x), px(y));
    input(
        window,
        cx,
        PlatformInput::MouseDown(gpui::MouseDownEvent {
            button,
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
            button,
            position,
            modifiers: Default::default(),
            click_count: 1,
        }),
    )?;
    frames(window, cx, 300).await
}

async fn click(window: Window, cx: &mut AsyncApp, at: (f32, f32)) -> anyhow::Result<()> {
    press_at(window, cx, at, MouseButton::Left).await
}

async fn press(window: Window, cx: &mut AsyncApp, key: &str) -> anyhow::Result<()> {
    let keystroke = gpui::Keystroke::parse(key)?;
    input(
        window,
        cx,
        PlatformInput::KeyDown(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        }),
    )?;
    input(
        window,
        cx,
        PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }),
    )?;
    frames(window, cx, 400).await
}

/// The right sidebar as last painted, read after a fresh probe and frame so
/// labels that stopped painting are gone.
async fn readback(window: Window, cx: &mut AsyncApp) -> anyhow::Result<Value> {
    window.update(cx, |shell, _, cx| shell.fixture_clear_right_probe(cx))?;
    frames(window, cx, 120).await?;
    Ok(window.update(cx, |shell, _, cx| shell.fixture_right_sidebar_readback(cx))?)
}

fn rect(rb: &Value, label: &str) -> Option<[f32; 4]> {
    let v = rb["bounds"][label].as_array()?;
    let n = |i: usize| v.get(i).and_then(Value::as_f64).map(|f| f as f32);
    Some([n(0)?, n(1)?, n(2)?, n(3)?])
}

fn need(rb: &Value, label: &str) -> anyhow::Result<[f32; 4]> {
    rect(rb, label).ok_or_else(|| anyhow::anyhow!("{label} was not painted: {}", rb["bounds"]))
}

fn center(r: [f32; 4]) -> (f32, f32) {
    (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
}

fn marks(rb: &Value, key: &str) -> Vec<String> {
    rb["marks"][key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn select(cx: &mut AsyncApp, state: &Entity<state::AppState>, id: &str) {
    let id = id.to_string();
    cx.update(|cx| state.update(cx, |state, cx| state.select_chat(Some(id), cx)));
}

fn toggle_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd-r"
    } else {
        "ctrl-r"
    }
}

/// The rail's places, top to bottom, as painted.
fn rail_order(rb: &Value) -> Vec<(String, [f32; 4])> {
    let mut places: Vec<(String, [f32; 4])> = rb["bounds"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(label, _)| {
            let key = label.strip_prefix("rail:")?;
            Some((key.to_string(), rect(rb, label)?))
        })
        .collect();
    places.sort_by(|a, b| a.1[1].total_cmp(&b.1[1]));
    places
}

async fn run(
    window: Window,
    cx: &mut AsyncApp,
    state: Entity<state::AppState>,
    output: &Path,
) -> anyhow::Result<Value> {
    let mut record = serde_json::Map::new();
    let mut visited: Vec<Value> = Vec::new();
    cx.update(|cx| motion::set_reduced_motion(cx, true));
    window.update(cx, |shell, _, cx| {
        shell.fixture_enable_right_probe(cx);
        shell.fixture_set_attention_snapshot(snapshot(), cx);
    })?;
    select(cx, &state, PARENT);
    frames(window, cx, 600).await?;
    cx.update(|cx| {
        state.update(cx, |state, cx| {
            state.transcript = transcript();
            cx.notify();
        })
    });
    frames(window, cx, 400).await?;

    // 2, 3, 4. With the sidebar closed the rail is there, in order, at the
    // window's right edge, with its badges.
    let closed = readback(window, cx).await?;
    capture(window, cx, output.join("01-rail-sidebar-closed.png"))?;
    anyhow::ensure!(
        closed["railShown"] == true && closed["rightPaneOpen"] == false,
        "case 3: the rail must show with the sidebar closed: {closed}"
    );
    let order = rail_order(&closed);
    let keys: Vec<&str> = order.iter().map(|(key, _)| key.as_str()).collect();
    anyhow::ensure!(
        keys == [
            "overview",
            "agents",
            "related",
            "automations",
            "files",
            "changes",
            "terminal",
            "browser"
        ],
        "case 2: rail order: {keys:?}"
    );
    let gap = |a: usize| order[a + 1].1[1] - (order[a].1[1] + order[a].1[3]);
    anyhow::ensure!(
        gap(3) > gap(2) + 6.0,
        "case 2: a line must part the chat's places from the workspace tools: {order:?}"
    );
    let rail_x = order[0].1[0];
    anyhow::ensure!(
        order.iter().all(
            |(_, r)| (r[0] + r[2] / 2.0 - (WIDTH - shell::RIGHT_RAIL_WIDTH / 2.0)).abs() < 1.0
        ),
        "case 2: the rail must sit at the window's right edge: {order:?}"
    );
    let badges = marks(&closed, "rail-badges");
    anyhow::ensure!(
        badges.contains(&"agents:1:running".to_string())
            && badges.contains(&"related:1:needs-you".to_string()),
        "case 4: badges with the sidebar closed: {badges:?}"
    );
    record.insert("closed".into(), closed.clone());
    visited.push(closed.clone());

    // 1, 2. The toggle opens the Overview place; the sidebar ends where the
    // rail starts.
    press(window, cx, toggle_key()).await?;
    frames(window, cx, 500).await?;
    let open = readback(window, cx).await?;
    capture(window, cx, output.join("02-overview.png"))?;
    anyhow::ensure!(
        open["rightPaneOpen"] == true && open["rightActive"] == "Home(Overview)",
        "case 1: the sidebar must open on the Overview: {open}"
    );
    let page = need(&open, "home-viewport")?;
    let rail_left = WIDTH - shell::RIGHT_RAIL_WIDTH;
    anyhow::ensure!(
        (page[0] + page[2] - rail_left).abs() < 1.5 && rail_x > rail_left,
        "case 2: the sidebar must end where the rail starts: page {page:?}, rail {rail_left}"
    );
    record.insert("overview".into(), open.clone());
    visited.push(open.clone());

    // 11. An Overview row opens its place; the rail follows.
    click(window, cx, center(need(&open, "overview:related")?)).await?;
    let related = readback(window, cx).await?;
    capture(window, cx, output.join("03-related.png"))?;
    anyhow::ensure!(
        related["rightActive"] == "Home(Related)" && related["place"] == "related",
        "case 11: the Overview's Related row must open Related: {related}"
    );
    visited.push(related.clone());

    // 3. Clicking the active place closes the sidebar; the rail and its
    // badges stay; another place opens it again.
    click(window, cx, center(need(&related, "rail:related")?)).await?;
    let hidden = readback(window, cx).await?;
    anyhow::ensure!(
        hidden["rightPaneOpen"] == false
            && rect(&hidden, "rail:related").is_some()
            && rect(&hidden, "home-viewport").is_none()
            && marks(&hidden, "rail-badges").contains(&"related:1:needs-you".to_string()),
        "case 3: the active place closes the sidebar, the rail stays: {hidden}"
    );
    click(window, cx, center(need(&hidden, "rail:agents")?)).await?;
    let agents = readback(window, cx).await?;
    capture(window, cx, output.join("04-agents.png"))?;
    anyhow::ensure!(
        agents["rightPaneOpen"] == true && agents["rightActive"] == "Home(Agents)",
        "case 3: a rail click opens its place: {agents}"
    );
    visited.push(agents.clone());

    // 6, 7. Changes opens a working-tree diff; History joins it as a
    // second Changes tab, in Changes' own row.
    click(window, cx, center(need(&agents, "rail:changes")?)).await?;
    let changes = readback(window, cx).await?;
    capture(window, cx, output.join("05-changes.png"))?;
    let tabs_of = |rb: &Value, place: &str| -> Vec<String> {
        rb["tabs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|tab| tab[1] == place)
            .filter_map(|tab| tab[0].as_str().map(str::to_string))
            .collect()
    };
    let diffs = tabs_of(&changes, "changes");
    anyhow::ensure!(
        diffs.len() == 1
            && changes["rightActive"] == diffs[0].as_str()
            && changes["place"] == "changes",
        "case 6: Changes must open a working-tree diff: {changes}"
    );
    click(window, cx, center(need(&changes, "header:history")?)).await?;
    let history = readback(window, cx).await?;
    capture(window, cx, output.join("06-changes-history.png"))?;
    let diffs = tabs_of(&history, "changes");
    anyhow::ensure!(
        diffs.len() == 2 && history["rightActive"] == diffs[1].as_str(),
        "case 6: History must open as a second Changes tab: {history}"
    );
    need(&history, "strip:tab:1")?;
    need(&history, "tabs:plus")?;
    visited.push(history.clone());

    // 7, 6. Terminal's row never lists Changes' tabs; back on Changes the
    // tab it last showed returns.
    click(window, cx, center(need(&history, "rail:terminal")?)).await?;
    let terminal = readback(window, cx).await?;
    capture(window, cx, output.join("07-terminal.png"))?;
    anyhow::ensure!(
        terminal["place"] == "terminal" && rect(&terminal, "strip:tab:0").is_none(),
        "case 7: the Terminal place must not list Changes tabs: {terminal}"
    );
    visited.push(terminal.clone());
    click(window, cx, center(need(&terminal, "rail:changes")?)).await?;
    let back = readback(window, cx).await?;
    anyhow::ensure!(
        back["rightActive"] == history["rightActive"],
        "case 6: returning to Changes shows its last tab: {back}"
    );
    visited.push(back.clone());

    // 8. A tab's menu closes the others; its ✕ then leaves Changes' own
    // empty state, not another place.
    press_at(
        window,
        cx,
        center(need(&back, "strip:tab:1")?),
        MouseButton::Right,
    )
    .await?;
    let menu = readback(window, cx).await?;
    capture(window, cx, output.join("08-tab-menu.png"))?;
    click(
        window,
        cx,
        center(need(&menu, "right-tab-menu-close-others")?),
    )
    .await?;
    let one = readback(window, cx).await?;
    anyhow::ensure!(
        tabs_of(&one, "changes").len() == 1,
        "Close other tabs: {one}"
    );
    let last = need(&one, "strip:tab:0")?;
    click(window, cx, (last[0] + 5.0 + 9.0, last[1] + last[3] / 2.0)).await?;
    let empty = readback(window, cx).await?;
    capture(window, cx, output.join("09-changes-empty.png"))?;
    anyhow::ensure!(
        empty["rightActive"] == "Home(Changes)"
            && marks(&empty, "empty") == ["changes|No diff open."]
            && rect(&empty, "empty-open:changes").is_some(),
        "case 8: the last tab must leave Changes' empty state: {empty}"
    );
    visited.push(empty.clone());

    // 9. Former entry points open their places and create no tabs.
    for (hook, expected) in [
        ("related", "Home(Related)"),
        ("agents", "Home(Agents)"),
        ("automations", "Home(Automations)"),
    ] {
        window.update(cx, |shell, _, cx| match hook {
            "related" => shell.fixture_open_child_chats(cx),
            "agents" => shell.fixture_open_agents(cx),
            _ => shell.fixture_open_automations(cx),
        })?;
        let rb = readback(window, cx).await?;
        anyhow::ensure!(
            rb["rightActive"] == expected && tabs_of(&rb, hook).is_empty(),
            "case 9: the {hook} entry point must open its place: {rb}"
        );
        visited.push(rb);
    }

    // 10. A related row opens its conversation as the main view (this
    // engine-less run has no workbench), never inside the sidebar.
    window.update(cx, |shell, _, cx| shell.fixture_open_child_chats(cx))?;
    let shown = readback(window, cx).await?;
    let body = need(&shown, "home-body:related")?;
    click(window, cx, (body[0] + 80.0, body[1] + 18.0)).await?;
    let opened = readback(window, cx).await?;
    capture(window, cx, output.join("10-related-opened-as-main.png"))?;
    anyhow::ensure!(
        opened["activeChat"]
            .as_str()
            .is_some_and(|id| id.starts_with("rs-child-")),
        "case 10: a related row must open its chat: {opened}"
    );
    visited.push(opened.clone());

    // 6. Without Git, Changes keeps its place and says why.
    select(cx, &state, PLAIN);
    frames(window, cx, 600).await?;
    let plain = readback(window, cx).await?;
    click(window, cx, center(need(&plain, "rail:changes")?)).await?;
    let no_git = readback(window, cx).await?;
    capture(window, cx, output.join("11-changes-without-git.png"))?;
    anyhow::ensure!(
        no_git["rightActive"] == "Home(Changes)"
            && marks(&no_git, "empty") == ["changes|No Git repository in this folder."]
            && tabs_of(&no_git, "changes").is_empty(),
        "case 6: without Git, Changes must say why: {no_git}"
    );
    visited.push(no_git.clone());
    record.insert("noGit".into(), no_git);

    // 5. Every state visited was a place's own page or a tab of the place
    // the rail showed.
    for rb in &visited {
        let active = rb["rightActive"].as_str().unwrap_or_default();
        let place = rb["place"].as_str().unwrap_or_default();
        let ok = if let Some(home) = active.strip_prefix("Home(") {
            home.trim_end_matches(')').eq_ignore_ascii_case(place)
        } else {
            rb["tabs"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|tab| tab[0] == active && tab[1] == place)
        };
        anyhow::ensure!(ok, "case 5: {active} is not in the {place} place: {rb}");
    }
    record.insert(
        "states".into(),
        json!(
            visited
                .iter()
                .map(|rb| rb["rightActive"].clone())
                .collect::<Vec<_>>()
        ),
    );
    Ok(Value::Object(record))
}

fn main() -> anyhow::Result<()> {
    let output =
        PathBuf::from(std::env::args().nth(1).ok_or_else(|| {
            anyhow::anyhow!("usage: right-sidebar-fixture ABSOLUTE_EVIDENCE_DIR")
        })?);
    anyhow::ensure!(
        output.is_absolute(),
        "the evidence directory must be absolute"
    );
    std::fs::create_dir_all(&output)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    let temp = tempfile::tempdir()?;
    let data = temp.path().to_path_buf();
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
                state.connection = luna_proto::view::ConnectionStatus::Ready;
                state.workspace_scope = Some(luna_proto::WorkspaceScope::Local);
                state.local_device_id = Some(DEVICE.into());
                state.devices = serde_json::from_value(serde_json::json!([
                    {"id": DEVICE, "name": "Studio Mac", "platform": "macos", "lastSeenAt": null}
                ]))
                .unwrap();
                state.spaces = vec![space(P_GIT, "luna", true), space(P_PLAIN, "notes", false)];
                state.chats = seed_chats();
                state.auto_selected = true;
                state.chats_synced = true;
                state.spaces_synced = true;
                state
            });
            let boot = EngineBootConfig {
                data_dir: data,
                ipc_port: 0,
                edge_url: String::new(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: HarnessId::ClaudeCode,
            };
            let window = cx
                .open_window(
                    WindowOptions {
                        window_background: theme::Theme::of(cx).window_background_appearance(),
                        show: false,
                        focus: false,
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(20.0), px(20.0)),
                            size(px(WIDTH), px(HEIGHT)),
                        ))),
                        ..Default::default()
                    },
                    |window, cx| {
                        window.set_rem_size(px(typography::font_size(cx).pixels()));
                        cx.new(|cx| shell::Shell::new(state.clone(), boot, cx))
                    },
                )
                .unwrap();
            cx.spawn(async move |cx| {
                let result = run(window, cx, state, &output).await;
                match result {
                    Ok(record) => {
                        let written = std::fs::write(
                            output.join("readback.json"),
                            serde_json::to_vec_pretty(&record).unwrap_or_default(),
                        )
                        .and_then(|_| {
                            std::fs::write(
                                output.join("right-sidebar.pass"),
                                "PASS: headless production Shell with real pointer and keyboard input (dark): the rail shows at the window's right edge with the sidebar closed, its eight places in order with a line before the workspace tools, and badges for a running subagent and a related chat waiting for approval; the toggle opens the Overview place and the sidebar ends where the rail starts; an Overview row opens Related; clicking the active place closes the sidebar while the rail and badges stay, and another place opens it; Changes opens a working-tree diff and History as a second Changes tab, Terminal's row lists none of them, and returning to Changes shows its last tab; a tab menu closes the others and the last ✕ leaves Changes' empty state; the related, Agents and Automations entry points open their places without tabs; a related row opens its chat as the main view; without Git, Changes says why; every state visited was a place page or a tab of the place shown\n",
                            )
                        });
                        if let Err(error) = written {
                            let _ = std::fs::write(output.join("failure.txt"), format!("{error}\n"));
                            std::process::exit(1);
                        }
                    }
                    Err(error) => {
                        eprintln!("right-sidebar fixture failed: {error:#}");
                        let _ = std::fs::write(output.join("failure.txt"), format!("{error:#}\n"));
                        let _ = capture(window, cx, output.join("failure.png"));
                        std::process::exit(1);
                    }
                }
                let _ = gpui::AnyWindowHandle::from(window)
                    .update(cx, |_, window, _| window.remove_window());
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    Ok(())
}
