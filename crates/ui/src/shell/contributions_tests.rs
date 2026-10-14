//! CORE-12 / E12, native shell side. An installed, enabled data package's
//! command, settings section, sidebar page and task panel join the shell's
//! contribution registry after LUNA's own entries, are drawn and activated
//! through the host's own controls (pointer and keyboard), and leave again
//! without moving the selected task, the composer's draft or a protected
//! entry. A saved layout naming a contribution is restored only while it
//! exists, and a late view reply for one task never lands beside another.
//!
//! Everything runs against a real engine over the RPC transport, with real
//! package directories installed and enabled through a separate client
//! (the normal path); the shell learns of each change only from its own
//! `WatchUiContributions`. GPUI's test platform drops accessibility tree
//! updates, so the macOS AX tree is native QA
//! (`docs/pending-tasks/extensible-core/core-12/README.md`). Set
//! `LUNA_CORE12_EVIDENCE_DIR` to keep each step's readback.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use gpui::{AppContext as _, TestAppContext, VisualTestContext, WindowHandle};
use luna_proto::UiPlacement;
use luna_rpc::{RpcError, RpcReply, RpcService};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::contributions::{ViewKey, ViewState};
use super::*;
use crate::contributions::ContributionKey;

const NOTES: &str = "core12.ui/notes";
const COMMAND: &str = "core12.ui/notes.open";
const SETTINGS: &str = "core12.ui/notes.settings";
const PAGE: &str = "core12.ui/notes.page";
const PANEL: &str = "core12.ui/notes.panel";
const SAVED_PAGE: &str = "contribution:core12.ui/notes.page";

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn evidence(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("LUNA_CORE12_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}

/// The data package (nothing runs): a command, a settings section, a
/// sidebar page whose button opens the section, and a task panel.
fn notes_package(root: &Path) -> (PathBuf, String) {
    let dir = root.join("notes-1.0.0");
    std::fs::create_dir_all(&dir).unwrap();
    let ui = serde_json::to_vec_pretty(&json!({
        "declarationVersion": 1,
        "contributions": [
            {"id": COMMAND, "placement": "command", "title": "Open notes appearance",
             "actions": [{"id": "run", "title": "Open notes appearance",
                          "run": {"host": {"openSettings": {"section": "appearance"}}}}]},
            {"id": SETTINGS, "placement": "settingsSection", "title": "Notes", "icon": "notebook",
             "view": {"static": {"nodes": [
                 {"heading": {"text": "Notes settings"}},
                 {"field": {"label": "Storage", "value": "This profile"}}
             ]}}},
            {"id": PAGE, "placement": "sidebarSection", "title": "Notes", "icon": "list",
             "view": {"static": {"nodes": [
                 {"heading": {"text": "Recent notes"}},
                 {"item": {"id": "n1", "title": "First note", "badge": "new", "action": "open"}},
                 "divider",
                 {"button": {"action": "open", "label": "Open notes settings"}}
             ]}},
             "actions": [{"id": "open", "title": "Open notes settings",
                          "run": {"host": {"openContribution": {"contributionId": SETTINGS}}}}]},
            {"id": PANEL, "placement": "panel", "title": "Task notes", "context": "task",
             "view": {"static": {"nodes": [{"text": {"text": "Notes for this task", "tone": "muted"}}]}}}
        ]
    }))
    .unwrap();
    std::fs::write(dir.join("luna-ui.json"), &ui).unwrap();
    let manifest = json!({
        "manifestVersion": 1,
        "id": NOTES,
        "version": "1.0.0",
        "displayName": "CORE-12 notes",
        "publisher": "CORE-12 UI",
        "api": {"major": 1, "minMinor": 3},
        "runtime": {"kind": "data"},
        "contributions": [
            {"id": COMMAND, "kind": "command"},
            {"id": SETTINGS, "kind": "settingsSection"},
            {"id": PAGE, "kind": "sidebarSection"},
            {"id": PANEL, "kind": "panel"}
        ],
        "files": [{"path": "luna-ui.json", "size": ui.len(), "sha256": sha256_hex(&ui)}],
    });
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    std::fs::write(dir.join("luna-plugin.json"), &bytes).unwrap();
    (dir, sha256_hex(&bytes))
}

/// Holds `GetUiContributionView` for one task until released; every other
/// call (and that one, once released) is the real engine's.
struct HeldViews {
    engine: Arc<dyn RpcService>,
    task: String,
    release: tokio::sync::Notify,
    held: AtomicUsize,
}

#[async_trait]
impl RpcService for HeldViews {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        if method == methods::GET_UI_CONTRIBUTION_VIEW
            && params.get("taskId").and_then(Value::as_str) == Some(self.task.as_str())
        {
            self.held.fetch_add(1, Ordering::SeqCst);
            self.release.notified().await;
        }
        self.engine.handle(method, params).await
    }
}

/// Fields drop in order: the services and engine before the data
/// directory, and all of them before the runtime their tasks run on.
struct Fixture {
    shell_service: Arc<dyn RpcService>,
    service: Arc<dyn RpcService>,
    core: luna_engine::EngineCore,
    temp: tempfile::TempDir,
    runtime: tokio::runtime::Runtime,
}

impl Fixture {
    /// A real engine with two local tasks (each with a checkout). The shell
    /// talks to `wrap(engine)`; the test's own calls go to the engine.
    fn new(
        cx: &mut TestAppContext,
        wrap: impl FnOnce(Arc<dyn RpcService>) -> Arc<dyn RpcService>,
    ) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (temp, core, service) = {
            let _entered = runtime.enter();
            let temp = tempfile::tempdir().unwrap();
            let data = temp.path().join("engine");
            std::fs::create_dir_all(&data).unwrap();
            std::fs::write(data.join("device-id"), "local").unwrap();
            let core = luna_engine::EngineCore::assemble(
                &data,
                Arc::new(luna_engine::HarnessRegistry::new()),
                luna_proto::HarnessId::Mock,
                None,
            )
            .unwrap();
            for task in ["task-a", "task-b"] {
                let checkout = temp.path().join(task);
                std::fs::create_dir_all(&checkout).unwrap();
                core.workspace
                    .create_chat(
                        task,
                        None,
                        Some(&core.device_id),
                        None,
                        Some(
                            checkout
                                .canonicalize()
                                .unwrap()
                                .to_string_lossy()
                                .into_owned(),
                        ),
                    )
                    .unwrap();
            }
            let service: Arc<dyn RpcService> = core.rpc_service();
            (temp, core, service)
        };
        let shell_service = wrap(service.clone());
        std::fs::create_dir_all(temp.path().join("ui")).unwrap();
        cx.update(|cx| {
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            gpui_tokio::init_from_handle(cx, runtime.handle().clone());
        });
        Self {
            shell_service,
            service,
            core,
            temp,
            runtime,
        }
    }

    /// (Re)start the UI preferences a new shell reads.
    fn preferences(&self, cx: &mut TestAppContext, saved_page: Option<&str>) {
        let mut preferences = settings::UiSettings::default();
        preferences.last_sidebar_page = saved_page.map(str::to_owned);
        let dir = self.temp.path().join("ui");
        cx.update(|cx| settings::init(preferences, dir, cx));
    }

    /// The shell's state: the engine (through `wrap`), one project and the
    /// two tasks, task A selected. Call with the runtime entered.
    fn state(&self) -> AppState {
        let client = luna_rpc::memory_client(self.shell_service.clone());
        let device = self.core.device_id.clone();
        let root = self.temp.path().to_path_buf();
        let mut state = AppState::new();
        state.set_test_engine(crate::state::EngineHandle::from_test_client(client));
        state.spaces = vec![luna_proto::Space {
            execution_defaults: Default::default(),
            id: "core12-project".into(),
            device_id: device.clone(),
            path: root.to_string_lossy().into_owned(),
            name: Some("CORE-12".into()),
            git_detected: false,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
            removed_at: None,
        }];
        state.chats = ["task-a", "task-b"]
            .into_iter()
            .map(|id| {
                let mut chat: luna_proto::Chat = serde_json::from_value(json!({
                    "id": id,
                    "deviceId": device,
                    "title": format!("Title of {id}"),
                    "archived": false,
                    "cwd": root.join(id).canonicalize().unwrap(),
                    "config": {
                        "harness": "claude-code",
                        "model": "synthetic-core12",
                        "sandbox": "workspace-write",
                        "permissionMode": "full",
                        "effectivePermissionMode": "full"
                    },
                    "createdAt": Utc::now(),
                }))
                .unwrap();
                chat.space_id = Some("core12-project".into());
                chat
            })
            .collect();
        state.selected_chat = Some("task-a".into());
        state
    }

    fn boot(&self) -> EngineBootConfig {
        EngineBootConfig {
            data_dir: self.temp.path().join("ui"),
            ipc_port: 0,
            edge_url: "http://127.0.0.1:1".into(),
            edge_token: None,
            org_id: None,
            workos_client_id: None,
            default_harness: luna_proto::HarnessId::Mock,
        }
    }

    /// The production shell as the window's root view, past its boot gate
    /// and splash: its own render drives the watch, the route
    /// reconciliation, the saved layout and the right sidebar.
    fn shell_window(&self, cx: &mut TestAppContext) -> WindowHandle<Shell> {
        let _entered = self.runtime.enter();
        let boot = self.boot();
        let state = self.state();
        cx.add_window(move |_, cx| {
            let state = cx.new(move |_| state);
            let mut shell = Shell::new(state, boot, cx);
            shell.debug_gate = Some(GatePhase::Ready);
            shell.splash = SplashPhase::Gone;
            shell
        })
    }

    /// The shell inside a host that draws its navigation, its main outlet
    /// and its right place with the production renderers, so their painted
    /// controls can be pressed (the full window keeps its boot splash over
    /// them until a real connection is Ready).
    fn host_window(&self, cx: &mut TestAppContext) -> WindowHandle<Host> {
        let _entered = self.runtime.enter();
        let boot = self.boot();
        let state = self.state();
        cx.add_window(move |_, cx| {
            let state = cx.new(move |_| state);
            let shell = cx.new(|cx| Shell::new(state, boot, cx));
            Host { shell }
        })
    }

    /// A host window whose shell follows the engine and shows task A.
    fn host(&self, cx: &mut TestAppContext) -> (WindowHandle<Host>, Entity<Shell>) {
        let window = self.host_window(cx);
        let shell = host_shell(cx, window);
        with_shell(cx, &shell, |shell, cx| {
            shell.state.update(cx, |_, cx| cx.notify());
            // What the shell's own render does each frame.
            shell.ensure_contribution_watch(cx);
        });
        self.pump(cx, window, &shell, "the first snapshot", |shell, _| {
            shell.contributions.registry.loaded() && shell.active_chat == "task-a"
        });
        (window, shell)
    }

    /// One engine call on a client of its own, never the shell's.
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let _entered = self.runtime.enter();
        let client = luna_rpc::memory_client(self.service.clone());
        self.runtime
            .block_on(client.call(method, params))
            .map_err(|error| error.to_string())
    }

    /// The normal install path: install, then enable with no grants.
    fn install_and_enable(&self, dir: &Path, hash: &str) {
        let installed = self
            .call("InstallExtension", json!({"path": dir}))
            .expect("install");
        assert_eq!(installed["packageHash"], hash);
        assert_eq!(installed["state"], "disabled");
        self.set_enabled(hash, true);
    }

    fn set_enabled(&self, hash: &str, enabled: bool) {
        self.call(
            "SetExtensionEnabled",
            json!({"extensionId": NOTES, "packageHash": hash, "enabled": enabled, "grants": []}),
        )
        .expect("change enablement");
    }

    fn turn(&self, cx: &mut TestAppContext, window: AnyWindowHandle) {
        cx.run_until_parked();
        self.runtime.block_on(tick());
        cx.update_window(window, |_, window, cx| {
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();
    }

    /// Drive both executors and paint until `done` holds.
    fn pump(
        &self,
        cx: &mut TestAppContext,
        window: impl Into<AnyWindowHandle> + Copy,
        shell: &Entity<Shell>,
        what: &str,
        mut done: impl FnMut(&Shell, &App) -> bool,
    ) {
        for _ in 0..1500 {
            self.turn(cx, window.into());
            if cx.update(|cx| done(shell.read(cx), cx)) {
                return;
            }
        }
        panic!("the shell never reached: {what}");
    }

    fn shutdown(self) {
        let Self {
            shell_service,
            service,
            core,
            temp,
            runtime,
        } = self;
        runtime.block_on(core.shutdown());
        let _entered = runtime.enter();
        drop(shell_service);
        drop(service);
        drop(core);
        drop(temp);
    }
}

use gpui::AnyWindowHandle;

async fn tick() {
    tokio::time::sleep(Duration::from_millis(2)).await;
}

struct Host {
    shell: Entity<Shell>,
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shell = self.shell.clone();
        shell.update(cx, |shell, cx| {
            let theme = Theme::of(cx).clone();
            let nav = match shell.route {
                Route::Settings(section) => shell.render_settings_nav(section, &theme, cx),
                Route::Chat | Route::Page(_) => shell.render_sidebar_nav(&theme, cx),
            };
            let outlet = match shell.route {
                Route::Page(page @ SidebarPage::Contributed(_)) => {
                    shell.sidebar_page_outlet(page, cx)
                }
                Route::Settings(section @ SettingsSection::Contributed(_)) => {
                    shell.settings_outlet(section, window, cx)
                }
                _ => gpui::Empty.into_any_element(),
            };
            let right = if shell.right_pane_open(cx) {
                let place = shell.right_place(cx);
                shell.render_place_home(place, cx)
            } else {
                gpui::Empty.into_any_element()
            };
            div()
                .size_full()
                .flex()
                .flex_row()
                .child(div().w(px(280.0)).h_full().child(nav))
                .child(div().flex_1().h_full().child(outlet))
                .child(div().w(px(360.0)).h_full().child(right))
        })
    }
}

fn host_shell(cx: &mut TestAppContext, window: WindowHandle<Host>) -> Entity<Shell> {
    window.read_with(cx, |host, _| host.shell.clone()).unwrap()
}

fn with_shell<R>(
    cx: &mut TestAppContext,
    shell: &Entity<Shell>,
    f: impl FnOnce(&mut Shell, &mut Context<Shell>) -> R,
) -> R {
    cx.update(|cx| shell.update(cx, f))
}

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn press(cx: &mut TestAppContext, window: AnyWindowHandle, selector: &'static str) {
    let mut visual = VisualTestContext::from_window(window, cx);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not painted"));
    visual.update(|window, app| {
        let position = bounds.center();
        window.dispatch_event(
            gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                position,
                button: gpui::MouseButton::Left,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            }),
            app,
        );
        window.dispatch_event(
            gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                position,
                button: gpui::MouseButton::Left,
                modifiers: Default::default(),
                click_count: 1,
            }),
            app,
        );
    });
}

fn painted(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    selector: &'static str,
) -> Option<gpui::Bounds<Pixels>> {
    VisualTestContext::from_window(window, cx).debug_bounds(selector)
}

fn ids(shell: &Shell, placement: UiPlacement) -> Vec<String> {
    shell
        .contributions
        .registry
        .placed(placement)
        .map(|contribution| contribution.id.to_string())
        .collect()
}

fn extension_count(shell: &Shell) -> usize {
    UiPlacement::ALL
        .into_iter()
        .flat_map(|placement| shell.contributions.registry.placed(placement))
        .filter(|contribution| contribution.entry().is_some())
        .count()
}

/// (draft owner key, draft text, selected task, shown task).
fn identity(shell: &Shell, cx: &App) -> (String, String, Option<String>, String) {
    let composer = shell.composer.read(cx);
    (
        composer.current_key.clone(),
        composer.input.read(cx).text().to_string(),
        shell.state.read(cx).selected_chat.clone(),
        shell.active_chat.clone(),
    )
}

fn view_state<'a>(shell: &'a Shell, key: &ViewKey) -> Option<&'a ViewState> {
    shell.contributions.views.get(key)
}

fn ready(shell: &Shell, key: &ViewKey) -> bool {
    matches!(view_state(shell, key), Some(ViewState::Ready { .. }))
}

/// An enabled package's four entries join after the host's own, are drawn
/// and activated through host controls (pointer and keyboard), and leave on
/// disable for the known-good defaults, with the selected task, the shown
/// task and the composer's draft untouched throughout. Regressions caught:
/// an extension entry precedes, displaces or hides a built-in one (Settings
/// → Extensions above all); a contributed row is not painted, not reachable
/// by Tab or does nothing on Space; its view is not the declared one; its
/// host action does not navigate; removing the package leaves a dead page,
/// section or panel, or moves the selection or the draft.
#[gpui::test]
fn extension_entries_join_after_the_host_and_leave_without_moving_task_or_draft(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new(cx, |engine| engine);
    fixture.preferences(cx, None);
    let (package, hash) = notes_package(&fixture.temp.path().join("sources"));
    let (window, shell) = fixture.host(cx);
    let any: AnyWindowHandle = window.into();
    let host_commands = cx.update(|cx| ids(shell.read(cx), UiPlacement::Command));
    let host_settings = cx.update(|cx| ids(shell.read(cx), UiPlacement::SettingsSection));
    let host_sidebar = cx.update(|cx| ids(shell.read(cx), UiPlacement::SidebarSection));
    assert_eq!(
        host_commands,
        [
            "luna.shell/command.new-task",
            "luna.shell/command.new-project",
            "luna.shell/command.open-board",
            "luna.shell/command.open-settings",
            "luna.shell/command.toggle-activity",
            "luna.shell/command.toggle-theme",
        ]
    );
    assert_eq!(
        host_sidebar,
        [
            "luna.shell/sidebar.new-thread",
            "luna.shell/sidebar.automations",
            "luna.shell/sidebar.board",
        ]
    );
    assert!(host_settings.contains(&"luna.shell/settings.extensions".to_owned()));
    assert!(cx.update(|cx| {
        shell
            .read(cx)
            .contributions
            .registry
            .find_id("luna.shell/settings.extensions")
            .is_some_and(|extensions| extensions.protected)
    }));

    // The user's draft for task A, settled before anything changes.
    with_shell(cx, &shell, |shell, cx| {
        let input = shell.composer.read(cx).input.clone();
        input.update(cx, |input, cx| input.set_text("draft for task A", cx));
    });
    for _ in 0..20 {
        fixture.turn(cx, any);
    }
    let before = cx.update(|cx| identity(shell.read(cx), cx));
    assert_eq!(before.1, "draft for task A");
    assert_eq!(before.2.as_deref(), Some("task-a"));

    fixture.install_and_enable(&package, &hash);
    fixture.pump(cx, any, &shell, "the package's four entries", |shell, _| {
        extension_count(shell) == 4
    });
    let (commands, settings_ids, sidebar) = cx.update(|cx| {
        let shell = shell.read(cx);
        (
            ids(shell, UiPlacement::Command),
            ids(shell, UiPlacement::SettingsSection),
            ids(shell, UiPlacement::SidebarSection),
        )
    });
    // The host's entries first, unchanged and in order; the extension's after.
    assert_eq!(commands[..host_commands.len()], host_commands[..]);
    assert_eq!(&commands[host_commands.len()..], [COMMAND]);
    assert_eq!(settings_ids[..host_settings.len()], host_settings[..]);
    assert_eq!(&settings_ids[host_settings.len()..], [SETTINGS]);
    assert_eq!(sidebar[..host_sidebar.len()], host_sidebar[..]);
    assert_eq!(&sidebar[host_sidebar.len()..], [PAGE]);
    let labels = cx.update(|cx| {
        let registry = &shell.read(cx).contributions.registry;
        [COMMAND, SETTINGS, PAGE, PANEL].map(|id| {
            registry
                .find_id(id)
                .map(|contribution| contribution.accessible_label().to_string())
        })
    });
    for label in &labels {
        assert!(
            label
                .as_deref()
                .is_some_and(|label| label.ends_with(", from CORE-12 notes")),
            "an extension entry's name does not name its extension: {labels:?}"
        );
    }
    assert_eq!(cx.update(|cx| identity(shell.read(cx), cx)), before);

    // The palette lists the extension's command after every host action; it
    // runs its declared host action.
    let command_key = ContributionKey::of(COMMAND);
    assert_eq!(
        cx.update(|cx| shell.read(cx).contributed_commands("")),
        [command_key]
    );
    assert_eq!(
        cx.update(|cx| shell.read(cx).contributed_commands("appearance notes")),
        [command_key]
    );
    with_shell(cx, &shell, |shell, cx| {
        shell.run_contribution_command(command_key, cx)
    });
    fixture.pump(cx, any, &shell, "the command's host action", |shell, _| {
        shell.route == Route::Settings(SettingsSection::Appearance)
    });

    // The sidebar page: painted after the built-in rows, opened by pointer.
    let page_key = ContributionKey::of(PAGE);
    with_shell(cx, &shell, |shell, cx| shell.open_chat("task-a".into(), cx));
    fixture.pump(cx, any, &shell, "back on the task", |shell, _| {
        shell.route == Route::Chat
    });
    let page_row = leak(format!("sidebar-nav-ext-{}", page_key.slug()));
    let board_row = painted(cx, any, "sidebar-nav-board").expect("the Board row is painted");
    let notes_row = painted(cx, any, page_row).expect("the extension's row is painted");
    assert!(
        notes_row.origin.y > board_row.origin.y,
        "the extension's row is not after the host's rows"
    );
    press(cx, any, page_row);
    let page_view = ViewKey {
        contribution: page_key,
        package_hash: hash.clone(),
        task_id: None,
    };
    fixture.pump(cx, any, &shell, "the page's view", |shell, _| {
        shell.route == Route::Page(SidebarPage::Contributed(page_key)) && ready(shell, &page_view)
    });
    let declared: Value =
        serde_json::from_slice(&std::fs::read(package.join("luna-ui.json")).unwrap()).unwrap();
    let shown = cx.update(|cx| match view_state(shell.read(cx), &page_view) {
        Some(ViewState::Ready { view }) => serde_json::to_value(view).unwrap(),
        other => panic!("the page view is {other:?}"),
    });
    assert_eq!(shown, declared["contributions"][2]["view"]["static"]);
    assert!(painted(cx, any, "contribution-item-n1").is_some());
    // Its button runs the declared host action: open the package's section.
    press(cx, any, "contribution-button-open");
    let settings_key = ContributionKey::of(SETTINGS);
    fixture.pump(
        cx,
        any,
        &shell,
        "the contributed settings section",
        |shell, _| shell.route == Route::Settings(SettingsSection::Contributed(settings_key)),
    );
    fixture.turn(cx, any);
    let extensions_row = painted(cx, any, "settings-nav-Extensions")
        .expect("Settings → Extensions stays in the navigation");
    let notes_section = painted(
        cx,
        any,
        leak(format!("settings-nav-ext-{}", settings_key.slug())),
    )
    .expect("the extension's section is in the navigation");
    assert!(
        notes_section.origin.y > extensions_row.origin.y,
        "the extension's section is not after the host's sections"
    );
    assert!(painted(cx, any, "contribution-page").is_some());

    // Keyboard: the extension's sidebar row is the fourth tab stop (after
    // New thread, Automations and Board) and Space opens it.
    with_shell(cx, &shell, |shell, cx| shell.open_chat("task-a".into(), cx));
    fixture.pump(cx, any, &shell, "back on the task again", |shell, _| {
        shell.route == Route::Chat
    });
    let mut visual = VisualTestContext::from_window(any, cx);
    visual.update(|window, app| {
        window.blur();
        for _ in 0..4 {
            window.focus_next(app);
        }
        let space = gpui::Keystroke::parse("space").unwrap();
        window.dispatch_event(
            gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                keystroke: space.clone(),
                is_held: false,
                prefer_character_input: false,
            }),
            app,
        );
        window.dispatch_event(
            gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke: space }),
            app,
        );
    });
    fixture.pump(
        cx,
        any,
        &shell,
        "the page opened from the keyboard",
        |shell, _| shell.route == Route::Page(SidebarPage::Contributed(page_key)),
    );

    // The task panel, for exactly the shown task.
    let panel_key = ContributionKey::of(PANEL);
    with_shell(cx, &shell, |shell, cx| {
        shell.show_right_place(RightPlace::Contributed(panel_key), cx)
    });
    let panel_view = ViewKey {
        contribution: panel_key,
        package_hash: hash.clone(),
        task_id: Some("task-a".into()),
    };
    fixture.pump(
        cx,
        any,
        &shell,
        "the panel's view for task A",
        |shell, cx| {
            shell.resolved_right_active(cx)
                == RightSurface::Home(RightPlace::Contributed(panel_key))
                && ready(shell, &panel_view)
        },
    );
    assert!(painted(cx, any, "contribution-panel").is_some());
    assert_eq!(cx.update(|cx| identity(shell.read(cx), cx)), before);
    let while_enabled = cx.update(|cx| {
        let shell = shell.read(cx);
        json!({
            "commands": ids(shell, UiPlacement::Command),
            "settings": ids(shell, UiPlacement::SettingsSection),
            "sidebar": ids(shell, UiPlacement::SidebarSection),
            "panels": ids(shell, UiPlacement::Panel),
            "labels": labels,
            "route": format!("{:?}", shell.route),
        })
    });

    // Disable through the normal path: every entry leaves; the open page
    // and panel fall back; nothing about the task or the draft moves.
    fixture.set_enabled(&hash, false);
    fixture.pump(cx, any, &shell, "the entries gone", |shell, _| {
        extension_count(shell) == 0
    });
    cx.update(|cx| {
        let shell_ref = shell.read(cx);
        assert_eq!(shell_ref.route, Route::Chat, "the dead page stayed open");
        assert_eq!(
            shell_ref.resolved_right_active(cx),
            RightSurface::Home(RightPlace::Overview),
            "the dead panel stayed open"
        );
        assert!(
            shell_ref.contributions.views.is_empty(),
            "a dead view stayed cached"
        );
        assert_eq!(ids(shell_ref, UiPlacement::Command), host_commands);
        assert_eq!(ids(shell_ref, UiPlacement::SettingsSection), host_settings);
        assert_eq!(ids(shell_ref, UiPlacement::SidebarSection), host_sidebar);
    });
    assert_eq!(cx.update(|cx| identity(shell.read(cx), cx)), before);
    assert!(
        painted(cx, any, page_row).is_none(),
        "the dead row is still painted"
    );
    evidence(
        "ui-join-and-leave",
        &json!({
            "packageHash": hash,
            "identity": [before.0, before.1, before.2, before.3],
            "whileEnabled": while_enabled,
            "afterDisable": cx.update(|cx| format!("{:?}", shell.read(cx).route)),
        }),
    );
    drop(visual);
    fixture.shutdown();
}

/// Let only the app's own invalidations paint (the test platform draws a
/// dirty window when effects flush; this forces no refresh) until the cached
/// sidebar has repainted with the Board row and with the extension's row
/// present (`shown`) or absent. A frame that reuses the sidebar's cached
/// paint records none of its debug bounds, so the Board row marks a frame
/// in which the sidebar really drew.
fn await_sidebar_row(
    fixture: &Fixture,
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    row: &'static str,
    shown: bool,
    what: &str,
) {
    for _ in 0..1500 {
        cx.run_until_parked();
        let board = painted(cx, window, "sidebar-nav-board").is_some();
        if board && painted(cx, window, row).is_some() == shown {
            return;
        }
        fixture.runtime.block_on(tick());
    }
    panic!("the sidebar never repainted: {what}");
}

/// In the production shell, where the sidebar is a cached view, a watch
/// snapshot alone (no navigation, no other UI change) adds the extension's
/// sidebar row and removes it again, for enable, safe mode on, safe mode off
/// and disable. Regressions caught: the registry updates but the cached
/// sidebar keeps its old rows until something else re-renders it (a row
/// missing after enable, or a dead row left after safe mode or disable).
#[gpui::test]
fn a_watch_snapshot_alone_adds_and_removes_the_sidebar_row(cx: &mut TestAppContext) {
    let fixture = Fixture::new(cx, |engine| engine);
    fixture.preferences(cx, None);
    let (package, hash) = notes_package(&fixture.temp.path().join("sources"));
    let window = fixture.shell_window(cx);
    let any: AnyWindowHandle = window.into();
    let shell = window.entity(cx).unwrap();
    with_shell(cx, &shell, |shell, cx| {
        shell.state.update(cx, |_, cx| cx.notify())
    });
    fixture.pump(cx, window, &shell, "the first snapshot", |shell, _| {
        shell.contributions.registry.loaded() && shell.active_chat == "task-a"
    });
    let row = leak(format!(
        "sidebar-nav-ext-{}",
        ContributionKey::of(PAGE).slug()
    ));
    assert!(
        painted(cx, any, "sidebar-nav-board").is_some(),
        "no sidebar"
    );
    assert!(painted(cx, any, row).is_none());
    let route = cx.update(|cx| shell.read(cx).route);

    fixture.install_and_enable(&package, &hash);
    await_sidebar_row(&fixture, cx, any, row, true, "the row after enable");
    fixture
        .call("SetExtensionSafeMode", json!({"enabled": true}))
        .expect("safe mode on");
    await_sidebar_row(&fixture, cx, any, row, false, "no row in safe mode");
    fixture
        .call("SetExtensionSafeMode", json!({"enabled": false}))
        .expect("safe mode off");
    await_sidebar_row(&fixture, cx, any, row, true, "the row after safe mode");
    fixture.set_enabled(&hash, false);
    await_sidebar_row(&fixture, cx, any, row, false, "no row after disable");
    // Nothing navigated: only the snapshots changed the sidebar.
    assert_eq!(cx.update(|cx| shell.read(cx).route), route);
    evidence(
        "ui-sidebar-follows-snapshots",
        &json!({"packageHash": hash, "route": format!("{route:?}")}),
    );
    fixture.shutdown();
}

/// A saved layout naming a contribution is restored only once the engine
/// says it exists, falls back while it does not, and a late view reply for
/// one task lands only in that task's slot, never in the panel shown for
/// the task the user moved to. Regressions caught: an unknown saved id
/// breaks startup or keeps a blank page; a known one is not restored after
/// a restart; the saved layout is overwritten before the first snapshot; a
/// reply for task A replaces the panel shown for task B (a stale callback
/// targeting the new tab); removing the package keeps a dead panel.
#[gpui::test]
fn saved_layouts_and_late_replies_fall_back_to_known_good(cx: &mut TestAppContext) {
    let held: Arc<std::sync::Mutex<Option<Arc<HeldViews>>>> = Default::default();
    let slot = held.clone();
    let fixture = Fixture::new(cx, move |engine| {
        let wrapped = Arc::new(HeldViews {
            engine,
            task: "task-a".into(),
            release: tokio::sync::Notify::new(),
            held: AtomicUsize::new(0),
        });
        *slot.lock().unwrap() = Some(wrapped.clone());
        wrapped as Arc<dyn RpcService>
    });
    let held = held.lock().unwrap().clone().unwrap();
    let (package, hash) = notes_package(&fixture.temp.path().join("sources"));
    let page_key = ContributionKey::of(PAGE);
    let panel_key = ContributionKey::of(PANEL);

    // A saved page whose package is not installed: the shell stays on its
    // default and the saved layout is replaced only after the snapshot.
    fixture.preferences(cx, Some(SAVED_PAGE));
    let first = fixture.shell_window(cx);
    let first_shell = first.entity(cx).unwrap();
    fixture.pump(cx, first, &first_shell, "the first snapshot", |shell, _| {
        shell.contributions.registry.loaded()
    });
    fixture.pump(
        cx,
        first,
        &first_shell,
        "the unknown page dropped",
        |shell, _| {
            !shell.contributions.restore_pending() && shell.settings.last_sidebar_page.is_none()
        },
    );
    assert_eq!(cx.update(|cx| first_shell.read(cx).route), Route::Chat);
    cx.update_window(first.into(), |_, window, _| window.remove_window())
        .unwrap();
    drop(first_shell);

    // The same saved page after the package is enabled, in a new shell (a
    // restart): restored once the first snapshot says it exists.
    fixture.install_and_enable(&package, &hash);
    fixture.preferences(cx, Some(SAVED_PAGE));
    let second = fixture.shell_window(cx);
    let shell = second.entity(cx).unwrap();
    assert!(cx.update(|cx| shell.read(cx).contributions.restore_pending()));
    with_shell(cx, &shell, |shell, cx| {
        shell.state.update(cx, |_, cx| cx.notify())
    });
    fixture.pump(cx, second, &shell, "the saved page restored", |shell, _| {
        shell.route == Route::Page(SidebarPage::Contributed(page_key))
            && shell.active_chat == "task-a"
    });
    fixture.pump(cx, second, &shell, "the page saved by id", |shell, _| {
        shell.settings.last_sidebar_page.as_deref() == Some(SAVED_PAGE)
    });
    let restored = cx.update(|cx| format!("{:?}", shell.read(cx).route));

    // Back on task A, its panel request is held; its checkout then
    // disappears, so the engine's eventual answer for A is a refusal.
    with_shell(cx, &shell, |shell, cx| shell.open_chat("task-a".into(), cx));
    fixture.pump(cx, second, &shell, "task A shown", |shell, _| {
        shell.route == Route::Chat && shell.active_chat == "task-a"
    });
    with_shell(cx, &shell, |shell, cx| {
        shell.show_right_place(RightPlace::Contributed(panel_key), cx)
    });
    let view_a = ViewKey {
        contribution: panel_key,
        package_hash: hash.clone(),
        task_id: Some("task-a".into()),
    };
    let view_b = ViewKey {
        task_id: Some("task-b".into()),
        ..view_a.clone()
    };
    fixture.pump(cx, second, &shell, "task A's request held", |shell, _| {
        matches!(view_state(shell, &view_a), Some(ViewState::Loading { .. }))
            && held.held.load(Ordering::SeqCst) == 1
    });
    std::fs::remove_dir_all(fixture.temp.path().join("task-a")).unwrap();

    // The user moves to task B and opens the same panel there.
    with_shell(cx, &shell, |shell, cx| shell.open_chat("task-b".into(), cx));
    fixture.pump(cx, second, &shell, "task B shown", |shell, _| {
        shell.active_chat == "task-b"
    });
    with_shell(cx, &shell, |shell, cx| {
        shell.show_right_place(RightPlace::Contributed(panel_key), cx)
    });
    fixture.pump(cx, second, &shell, "task B's panel", |shell, _| {
        ready(shell, &view_b)
    });

    // A's late answer arrives: it lands in A's slot only.
    held.release.notify_one();
    fixture.pump(cx, second, &shell, "task A's late answer", |shell, _| {
        matches!(view_state(shell, &view_a), Some(ViewState::Failed { .. }))
    });
    cx.update(|cx| {
        let shell_ref = shell.read(cx);
        assert!(
            ready(shell_ref, &view_b),
            "task A's late answer replaced the panel shown for task B: {:?}",
            view_state(shell_ref, &view_b)
        );
        assert_eq!(shell_ref.active_chat, "task-b");
        assert_eq!(
            shell_ref.resolved_right_active(cx),
            RightSurface::Home(RightPlace::Contributed(panel_key))
        );
    });
    let late = cx.update(|cx| {
        let shell = shell.read(cx);
        json!({
            "taskA": format!("{:?}", view_state(shell, &view_a)),
            "taskB": format!("{:?}", view_state(shell, &view_b)),
        })
    });

    // Disabled: the panel falls back to the Overview for task B.
    fixture.set_enabled(&hash, false);
    fixture.pump(cx, second, &shell, "the panel gone", |shell, cx| {
        extension_count(shell) == 0
            && shell.resolved_right_active(cx) == RightSurface::Home(RightPlace::Overview)
    });
    evidence(
        "ui-saved-layouts-and-late-replies",
        &json!({
            "packageHash": hash,
            "restoredRoute": restored,
            "lateReply": late,
            "held": held.held.load(Ordering::SeqCst),
        }),
    );
    drop(held);
    fixture.shutdown();
}
