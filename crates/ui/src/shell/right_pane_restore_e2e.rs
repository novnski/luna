//! Issue #23 end to end: the production Shell as the window's root view,
//! over a real engine. A chat's right sidebar — shown, on Files, its browser
//! promoted to `src/ThemeSwitch.tsx` by clicks in its tree, beside a second
//! file tab — survives a graceful restart: the quit gate passes, settings
//! flush as on app quit, the window goes, and a new shell boots from the
//! saved file. The restored editor reads the file from the chat's checkout
//! again. A file deleted between runs lands on the editor's recovery state,
//! whose Retry reads it once it is back. Files renamed on disk while another
//! chat shows reopen under their new paths.
//!
//! Owners: what the tree, tabs and editors do once open belongs to the Files
//! suites (`files::tool_file_open_e2e` and others); this file owns what a
//! restart keeps.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{AnyWindowHandle, AppContext as _, TestAppContext, VisualTestContext, WindowHandle};
use luna_rpc::{RpcError, RpcReply, RpcService};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::*;

const CHAT: &str = "issue23-chat";
const OTHER_CHAT: &str = "issue23-other-chat";
const SPACE: &str = "issue23-space";
const THEME: &str = "src/ThemeSwitch.tsx";
const THEME_BYTES: &str = "export function ThemeSwitch() {}\n";
const OTHER: &str = "src/palette.ts";
const OTHER_BYTES: &str = "export const palette = [];\n";
const THEME_RENAMED: &str = "src/ThemeToggle.tsx";
const OTHER_RENAMED: &str = "src/colors.ts";

fn sha256(bytes: &str) -> String {
    Sha256::digest(bytes.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The real engine, recording each workspace file read the shell makes:
/// (path, content hash or error).
struct Recorder {
    engine: Arc<dyn RpcService>,
    reads: Mutex<Vec<(String, Result<String, String>)>>,
}

#[async_trait::async_trait]
impl RpcService for Recorder {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        let reply = self.engine.handle(method, params.clone()).await;
        if method == methods::READ_WORKSPACE_FILE {
            let outcome = match &reply {
                Ok(RpcReply::Value(value)) => {
                    Ok(value["contentHash"].as_str().unwrap_or_default().to_owned())
                }
                Ok(RpcReply::Stream(_)) => Err("a streamed file read".to_owned()),
                Err(error) => Err(error.to_string()),
            };
            let path = params["path"].as_str().unwrap_or_default().to_owned();
            self.reads.lock().unwrap().push((path, outcome));
        }
        reply
    }
}

/// Fields drop in order: the engine before its directory, and both before
/// the runtime their tasks run on.
struct Fixture {
    recorder: Arc<Recorder>,
    core: luna_engine::EngineCore,
    checkout: PathBuf,
    ui: PathBuf,
    _temp: tempfile::TempDir,
    runtime: tokio::runtime::Runtime,
}

impl Fixture {
    /// One local chat whose checkout holds the two files.
    fn new(cx: &mut TestAppContext) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let checkout = base.join("checkout");
        for (path, bytes) in [(THEME, THEME_BYTES), (OTHER, OTHER_BYTES)] {
            let path = checkout.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let ui = base.join("ui");
        std::fs::create_dir_all(&ui).unwrap();
        let core = {
            let _entered = runtime.enter();
            let data = base.join("engine");
            std::fs::create_dir_all(&data).unwrap();
            std::fs::write(data.join("device-id"), "local").unwrap();
            let core = luna_engine::EngineCore::assemble(
                &data,
                Arc::new(luna_engine::HarnessRegistry::new()),
                luna_proto::HarnessId::Mock,
                None,
            )
            .unwrap();
            core.workspace
                .create_space(
                    SPACE,
                    &core.device_id,
                    &checkout.to_string_lossy(),
                    None,
                    false,
                )
                .unwrap();
            for chat in [CHAT, OTHER_CHAT] {
                core.workspace
                    .create_chat(chat, Some(SPACE), None, None, None)
                    .unwrap();
            }
            core
        };
        let recorder = Arc::new(Recorder {
            engine: core.rpc_service(),
            reads: Mutex::new(Vec::new()),
        });
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
            crate::motion::set_reduced_motion(cx, true);
        });
        Self {
            recorder,
            core,
            checkout,
            ui,
            _temp: temp,
            runtime,
        }
    }

    /// Start LUNA: settings from disk, then the shell showing the chat.
    fn launch(&self, cx: &mut TestAppContext) -> (WindowHandle<Shell>, Entity<Shell>) {
        let ui = self.ui.clone();
        cx.update(|cx| settings::init(settings::UiSettings::load(&ui), ui.clone(), cx));
        let window = {
            let _entered = self.runtime.enter();
            let client = luna_rpc::memory_client(self.recorder.clone() as Arc<dyn RpcService>);
            let device = self.core.device_id.clone();
            let rows = [CHAT, OTHER_CHAT].map(|id| {
                json!({
                    "id": id, "deviceId": device, "spaceId": SPACE,
                    "cwd": self.checkout.to_string_lossy(),
                    "title": id, "archived": false, "createdAt": "2026-10-15T00:00:00Z",
                })
            });
            let boot = EngineBootConfig {
                data_dir: self.ui.clone(),
                ipc_port: 0,
                edge_url: "http://127.0.0.1:1".into(),
                edge_token: None,
                org_id: None,
                workos_client_id: None,
                default_harness: luna_proto::HarnessId::Mock,
            };
            cx.add_window(move |_, cx| {
                let state = cx.new(move |_| {
                    let mut state = AppState::new();
                    state.set_test_engine(crate::state::EngineHandle::from_test_client(client));
                    state.local_device_id = Some(device);
                    state.chats = rows
                        .into_iter()
                        .map(|row| serde_json::from_value(row).unwrap())
                        .collect();
                    state.selected_chat = Some(CHAT.into());
                    state
                });
                let mut shell = Shell::new(state, boot, cx);
                shell.debug_gate = Some(GatePhase::Ready);
                shell.splash = SplashPhase::Gone;
                shell
            })
        };
        let shell = window.entity(cx).unwrap();
        cx.update(|cx| shell.read(cx).state.clone().update(cx, |_, cx| cx.notify()));
        self.pump(cx, window.into(), "the chat shown", |cx| {
            cx.update(|cx| shell.read(cx).active_chat == CHAT)
        });
        (window, shell)
    }

    /// Quit gracefully: nothing unsaved holds the quit, settings flush as
    /// the app's quit hook does, and the window and its shell go away.
    fn quit(&self, cx: &mut TestAppContext, window: WindowHandle<Shell>, shell: Entity<Shell>) {
        assert!(
            cx.update(|cx| shell.update(cx, |shell, cx| shell.prepare_quit(cx))),
            "the quit gate held"
        );
        cx.update(settings::flush);
        cx.update_window(window.into(), |_, window, _| window.remove_window())
            .unwrap();
        drop(shell);
        cx.run_until_parked();
    }

    /// Drive both executors and paint until `done` holds.
    fn pump(
        &self,
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        what: &str,
        mut done: impl FnMut(&mut TestAppContext) -> bool,
    ) {
        for _ in 0..1500 {
            cx.run_until_parked();
            self.runtime
                .block_on(async { tokio::time::sleep(Duration::from_millis(2)).await });
            cx.update_window(window, |_, window, cx| {
                window.refresh();
                window.draw(cx).clear();
            })
            .unwrap();
            if done(cx) {
                return;
            }
        }
        panic!("the shell never reached: {what}");
    }

    /// Every read of `path` so far.
    fn reads(&self, path: &str) -> Vec<Result<String, String>> {
        self.recorder
            .reads
            .lock()
            .unwrap()
            .iter()
            .filter(|(read, _)| read == path)
            .map(|(_, outcome)| outcome.clone())
            .collect()
    }

    fn shutdown(self) {
        let Self {
            recorder,
            core,
            _temp,
            runtime,
            ..
        } = self;
        runtime.block_on(core.shutdown());
        let _entered = runtime.enter();
        drop(recorder);
        drop(core);
        drop(_temp);
    }
}

fn painted(cx: &mut TestAppContext, window: AnyWindowHandle, selector: &'static str) -> bool {
    VisualTestContext::from_window(window, cx)
        .debug_bounds(selector)
        .is_some()
}

fn press(cx: &mut TestAppContext, window: AnyWindowHandle, selector: &'static str) {
    let mut visual = VisualTestContext::from_window(window, cx);
    let at = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not painted"))
        .center();
    visual.simulate_click(at, gpui::Modifiers::default());
}

/// The Files browser's open file, and the chat's file tabs' paths in strip
/// order (`None` for the browser).
fn files_place(
    shell: &Entity<Shell>,
    cx: &mut TestAppContext,
) -> (Option<String>, Vec<Option<String>>) {
    cx.update(|cx| {
        let shell = shell.read(cx);
        let browser = shell
            .files
            .get(CHAT)
            .and_then(|files| files.read(cx).attachment_path().map(str::to_owned));
        let tabs = shell
            .right_tabs
            .get(CHAT)
            .into_iter()
            .flatten()
            .filter_map(|surface| match surface {
                RightSurface::Files => Some(None),
                RightSurface::File(id) => Some(shell.file_surface_paths.get(id).cloned()),
                _ => None,
            })
            .collect();
        (browser, tabs)
    })
}

/// The pane shows, on the Files browser, which shows `browser`, beside the
/// `tab` tab.
fn restored(shell: &Entity<Shell>, cx: &mut TestAppContext, browser: &str, tab: &str) -> bool {
    let shown = cx.update(|cx| {
        let shell = shell.read(cx);
        shell.right_pane_open(cx) && shell.resolved_right_active(cx) == RightSurface::Files
    });
    shown && files_place(shell, cx) == (Some(browser.to_owned()), vec![None, Some(tab.to_owned())])
}

#[gpui::test]
fn graceful_restart_reopens_the_files_pane_and_its_document(cx: &mut TestAppContext) {
    let fixture = Fixture::new(cx);

    // First run: the rail's Files, then src → ThemeSwitch.tsx in its tree.
    let (window, shell) = fixture.launch(cx);
    let any: AnyWindowHandle = window.into();
    window
        .update(cx, |shell, window, cx| {
            shell.toggle_right_place(RightPlace::Files, window, cx)
        })
        .unwrap();
    fixture.pump(cx, any, "the tree lists src", |cx| {
        painted(cx, any, "files-tree-entry:src")
    });
    press(cx, any, "files-tree-entry:src");
    fixture.pump(cx, any, "src expanded", |cx| {
        painted(cx, any, "files-tree-entry:src/ThemeSwitch.tsx")
    });
    press(cx, any, "files-tree-entry:src/ThemeSwitch.tsx");
    fixture.pump(cx, any, "ThemeSwitch.tsx open in the browser", |cx| {
        files_place(&shell, cx).0.as_deref() == Some(THEME)
            && fixture.reads(THEME).contains(&Ok(sha256(THEME_BYTES)))
    });
    // A second file in its own tab, then the browser's tab chosen again.
    window
        .update(cx, |shell, window, cx| {
            shell.add_file_surface(OTHER.into(), window, cx)
        })
        .unwrap();
    fixture.pump(cx, any, "palette.ts open in its tab", |_| {
        fixture.reads(OTHER).contains(&Ok(sha256(OTHER_BYTES)))
    });
    press(cx, any, "right-surface-tab-0");
    fixture.pump(cx, any, "the browser's tab shown", |cx| {
        restored(&shell, cx, THEME, OTHER)
    });
    fixture.quit(cx, window, shell);

    // Second run: the same pane, place, tabs and document, read again from
    // this checkout.
    let before = fixture.reads(THEME).len();
    let (window, shell) = fixture.launch(cx);
    let any: AnyWindowHandle = window.into();
    fixture.pump(cx, any, "the pane restored", |cx| {
        restored(&shell, cx, THEME, OTHER)
    });
    fixture.pump(cx, any, "ThemeSwitch.tsx read again", |_| {
        fixture.reads(THEME)[before..].contains(&Ok(sha256(THEME_BYTES)))
    });
    assert!(!painted(cx, any, "files-document-error"));
    fixture.quit(cx, window, shell);

    // Third run, the file deleted meanwhile: the pane and tabs come back and
    // the editor says what failed; Retry reads it once it is back.
    std::fs::remove_file(fixture.checkout.join(THEME)).unwrap();
    let before = fixture.reads(THEME).len();
    let (window, shell) = fixture.launch(cx);
    let any: AnyWindowHandle = window.into();
    fixture.pump(cx, any, "the recovery state", |cx| {
        restored(&shell, cx, THEME, OTHER) && painted(cx, any, "files-document-error")
    });
    let failed = fixture.reads(THEME)[before..].to_vec();
    assert!(
        !failed.is_empty()
            && failed.iter().all(|read| read
                .as_ref()
                .is_err_and(|error| error.contains("file not found"))),
        "{failed:?}"
    );
    std::fs::write(fixture.checkout.join(THEME), THEME_BYTES).unwrap();
    let before = fixture.reads(THEME).len();
    press(cx, any, "files-document-retry");
    fixture.pump(cx, any, "ThemeSwitch.tsx back", |cx| {
        fixture.reads(THEME)[before..].contains(&Ok(sha256(THEME_BYTES)))
            && !painted(cx, any, "files-document-error")
    });

    // Still in the third run, another chat shows while both files are
    // renamed on disk — the palette.ts tab restored but never shown in this
    // run: the background chat's browser and tab follow, and so does what
    // the next run reopens.
    window
        .update(cx, |shell, _, cx| shell.open_chat(OTHER_CHAT.into(), cx))
        .unwrap();
    fixture.pump(cx, any, "the other chat shown", |cx| {
        cx.update(|cx| shell.read(cx).active_chat == OTHER_CHAT)
    });
    for (from, to) in [(THEME, THEME_RENAMED), (OTHER, OTHER_RENAMED)] {
        std::fs::rename(fixture.checkout.join(from), fixture.checkout.join(to)).unwrap();
    }
    fixture.pump(cx, any, "the background tabs renamed", |cx| {
        files_place(&shell, cx)
            == (
                Some(THEME_RENAMED.to_owned()),
                vec![None, Some(OTHER_RENAMED.to_owned())],
            )
    });
    fixture.quit(cx, window, shell);

    // Fourth run: the renamed files reopen and read.
    let (window, shell) = fixture.launch(cx);
    let any: AnyWindowHandle = window.into();
    fixture.pump(cx, any, "the renamed files restored", |cx| {
        restored(&shell, cx, THEME_RENAMED, OTHER_RENAMED)
    });
    fixture.pump(cx, any, "ThemeToggle.tsx read", |_| {
        fixture
            .reads(THEME_RENAMED)
            .contains(&Ok(sha256(THEME_BYTES)))
    });
    assert!(!painted(cx, any, "files-document-error"));
    fixture.quit(cx, window, shell);
    fixture.shutdown();
}
