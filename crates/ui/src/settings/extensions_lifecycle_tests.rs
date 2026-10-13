//! CORE-19 Settings → Extensions lifecycle, driven through a real engine over
//! the RPC transport with real package directories. The visible update,
//! rollback, remove, erase and safe-mode states come from the engine's own
//! answers; every change the page makes is read back independently, from
//! the engine's grants file on disk, a separate RPC client, or the data
//! folder itself. Each step also paints the page.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, TestAppContext, WindowHandle};
use luna_rpc::RpcService;
use serde_json::{Value, json};

use super::*;

const NOTES: &str = "core19.ui/notes";
const NETWORK: &str = "luna.capability/network";
const TASKS_READ: &str = "luna.capability/tasks.read";

/// Fields drop in order: the service and engine before the data directory,
/// and all of them before the runtime their tasks run on.
struct Fixture {
    service: Arc<dyn RpcService>,
    core: luna_engine::EngineCore,
    temp: tempfile::TempDir,
    runtime: tokio::runtime::Runtime,
}

impl Fixture {
    fn new(cx: &mut TestAppContext) -> Self {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Engine assembly spawns onto this runtime; the guard must end
        // before the runtime moves into the fixture.
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
            let service = core.rpc_service();
            (temp, core, service)
        };
        Self {
            service,
            core,
            temp,
            runtime,
        }
    }

    fn data_dir(&self) -> PathBuf {
        self.temp.path().join("engine")
    }

    fn window(&self, cx: &mut TestAppContext) -> WindowHandle<ExtensionsPage> {
        let _entered = self.runtime.enter();
        let client = luna_rpc::memory_client(self.service.clone());
        cx.add_window(move |_, cx| {
            let state = cx.new(move |_| {
                let mut state = crate::state::AppState::new();
                state.set_test_engine(crate::state::EngineHandle::from_test_client(client));
                state
            });
            ExtensionsPage::new(state, cx)
        })
    }

    /// One engine call on a client of its own, not the page's.
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let _entered = self.runtime.enter();
        let client = luna_rpc::memory_client(self.service.clone());
        self.runtime
            .block_on(client.call(method, params))
            .map_err(|error| error.to_string())
    }

    /// The profile's grants file as the engine wrote it.
    fn grants_file(&self) -> Value {
        let files = find(&self.data_dir(), "extension-grants.json");
        assert_eq!(files.len(), 1, "one profile grants file: {files:?}");
        serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap()
    }

    /// Drive both executors and paint until `done` holds for an idle page.
    fn pump(
        &self,
        cx: &mut TestAppContext,
        window: WindowHandle<ExtensionsPage>,
        what: &str,
        mut done: impl FnMut(&ExtensionsPage) -> bool,
    ) {
        for _ in 0..1500 {
            cx.run_until_parked();
            self.runtime.block_on(tick());
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear();
            })
            .unwrap();
            let reached = window
                .read_with(cx, |page, _| !page.busy && !page.loading && done(page))
                .unwrap();
            if reached {
                return;
            }
        }
        panic!("the extensions page never reached: {what}");
    }

    fn draw(&self, cx: &mut TestAppContext, window: WindowHandle<ExtensionsPage>) {
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear();
        })
        .unwrap();
    }

    fn shutdown(self) {
        let Self {
            service,
            core,
            temp,
            runtime,
        } = self;
        runtime.block_on(core.shutdown());
        let _entered = runtime.enter();
        drop(service);
        drop(core);
        drop(temp);
    }
}

/// One short real-time wait driven by the runtime; the timer is created
/// inside it.
async fn tick() {
    tokio::time::sleep(Duration::from_millis(2)).await;
}

/// A data package (nothing runs) with `data.schemaVersion` 1.
fn package(root: &Path, version: &str, capabilities: Value) -> PathBuf {
    let dir = root.join(format!("notes-{version}"));
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = json!({
        "manifestVersion": 1,
        "id": NOTES,
        "version": version,
        "displayName": "UI notes",
        "publisher": "CORE-19 UI",
        "api": {"major": 1, "minMinor": 0},
        "runtime": {"kind": "data"},
        "capabilities": capabilities,
        "data": {"schemaVersion": 1},
        "files": [],
    });
    std::fs::write(
        dir.join("luna-plugin.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    dir
}

fn find(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name().to_str() == Some(name) {
                found.push(path.clone());
            }
            if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
                pending.push(path);
            }
        }
    }
    found
}

fn listed(page: &ExtensionsPage, version: &str) -> Option<ExtensionPackageView> {
    page.snapshot
        .as_ref()?
        .packages
        .iter()
        .find(|package| package.version == version)
        .cloned()
}

fn enabled(page: &ExtensionsPage, version: &str) -> bool {
    listed(page, version).is_some_and(|package| package.state == ExtensionState::Enabled)
}

fn row(
    window: WindowHandle<ExtensionsPage>,
    cx: &mut TestAppContext,
    version: &str,
) -> ExtensionPackageView {
    window
        .read_with(cx, |page, _| listed(page, version))
        .unwrap()
        .unwrap_or_else(|| panic!("{version} is not listed"))
}

/// Every lifecycle state the page shows comes from the engine, and every
/// action it takes lands there exactly. Regressions caught: a second version
/// offers "Review and enable" instead of a switch; the switch review turns
/// on a capability the new version adds (silent widening); the switch sends
/// other grants than shown; rollback is not offered; safe mode has no
/// visible state or exit; remove or erase skips its confirmation or does
/// not reach the engine; erased data stays on disk.
#[gpui::test]
fn lifecycle_states_and_actions_follow_the_engine(cx: &mut TestAppContext) {
    let fixture = Fixture::new(cx);
    let sources = fixture.temp.path().join("sources");
    let v1 = package(&sources, "1.0.0", json!([]));
    let v2 = package(
        &sources,
        "2.0.0",
        json!([{"id": TASKS_READ}, {"id": NETWORK, "required": false}]),
    );
    let window = fixture.window(cx);
    fixture.pump(cx, window, "the first listing", |page| {
        page.snapshot.is_some()
    });
    let mut trace = Vec::new();

    // Installed: a disabled candidate to review.
    window
        .update(cx, |page, _, cx| page.install(v1.clone(), cx))
        .unwrap();
    fixture.pump(cx, window, "1.0.0 listed", |page| {
        listed(page, "1.0.0").is_some()
    });
    let first = row(window, cx, "1.0.0");
    assert_eq!(row_actions(&first), [RowAction::Review, RowAction::Remove]);
    assert_eq!(row_badges(&first), ["Off"]);
    window
        .update(cx, |page, _, cx| {
            page.open_review(first.clone(), cx);
            page.confirm_review(cx);
        })
        .unwrap();
    fixture.pump(cx, window, "1.0.0 enabled", |page| enabled(page, "1.0.0"));
    let grants = fixture.grants_file();
    assert_eq!(
        grants["extensions"][NOTES]["packageHash"],
        first.package_hash
    );
    trace.push(json!({"step": "enabled", "row": row(window, cx, "1.0.0")}));

    // A second version: a switch, its added capabilities marked and off
    // unless required.
    window
        .update(cx, |page, _, cx| page.install(v2.clone(), cx))
        .unwrap();
    fixture.pump(cx, window, "2.0.0 listed", |page| {
        listed(page, "2.0.0").is_some()
    });
    let second = row(window, cx, "2.0.0");
    let update = RowAction::Switch {
        kind: ExtensionSwitchKind::Update,
        version: "2.0.0".into(),
        blocked: None,
    };
    assert_eq!(update.label(), "Update to v2.0.0");
    assert_eq!(row_actions(&second), [update, RowAction::Remove]);
    let switch = second.switch.clone().expect("a switch view");
    assert_eq!(switch.from_version, "1.0.0");
    let added: Vec<&str> = switch
        .new_capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect();
    assert_eq!(added, [TASKS_READ, NETWORK]);
    assert_eq!(
        data_plan_copy(&switch.data),
        "Its data (schema 1) carries over unchanged."
    );
    window
        .update(cx, |page, _, cx| page.open_switch(second.clone(), cx))
        .unwrap();
    fixture.draw(cx, window);
    let granted = window
        .read_with(cx, |page, _| page.review.as_ref().unwrap().granted.clone())
        .unwrap();
    assert_eq!(
        granted,
        BTreeSet::from([TASKS_READ.to_owned()]),
        "the review turned on a capability the new version adds"
    );
    window
        .update(cx, |page, _, cx| {
            page.toggle_grant(NETWORK, cx);
            page.confirm_review(cx);
        })
        .unwrap();
    fixture.pump(cx, window, "2.0.0 enabled", |page| enabled(page, "2.0.0"));
    let grants = fixture.grants_file();
    assert_eq!(
        grants["extensions"][NOTES]["packageHash"],
        second.package_hash
    );
    assert_eq!(
        grants["extensions"][NOTES]["grants"],
        json!([NETWORK, TASKS_READ]),
        "the switch sent other grants than the review showed"
    );

    // The previous version is now a rollback.
    let first = row(window, cx, "1.0.0");
    let rollback = RowAction::Switch {
        kind: ExtensionSwitchKind::Rollback,
        version: "1.0.0".into(),
        blocked: None,
    };
    assert_eq!(rollback.label(), "Roll back to v1.0.0");
    assert_eq!(row_actions(&first), [rollback, RowAction::Remove]);
    trace.push(json!({"step": "updated", "rows": [first, row(window, cx, "2.0.0")]}));
    window
        .update(cx, |page, _, cx| {
            page.open_switch(first.clone(), cx);
            page.confirm_review(cx);
        })
        .unwrap();
    fixture.pump(cx, window, "rolled back", |page| enabled(page, "1.0.0"));
    let grants = fixture.grants_file();
    assert_eq!(
        grants["extensions"][NOTES]["packageHash"],
        first.package_hash
    );
    assert_eq!(grants["extensions"][NOTES]["grants"], json!([]));

    // Safe mode: a visible state with its own exit.
    window
        .update(cx, |page, _, cx| page.set_safe_mode(true, cx))
        .unwrap();
    fixture.pump(cx, window, "safe mode on", |page| {
        page.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.safe_mode.is_some())
    });
    let safe_mode = window
        .read_with(cx, |page, _| {
            page.snapshot.as_ref().unwrap().safe_mode.clone().unwrap()
        })
        .unwrap();
    assert_eq!(safe_mode.source, ExtensionSafeModeSource::Profile);
    assert!(safe_mode.profile_enabled);
    assert!(safe_mode_copy(&safe_mode).starts_with("Safe mode is on."));
    let instances = fixture
        .call(methods::LIST_EXTENSION_INSTANCES, json!({}))
        .expect("instances");
    assert_eq!(instances["safeMode"], true, "{instances}");
    trace.push(json!({"step": "safeMode", "safeMode": safe_mode}));
    window
        .update(cx, |page, _, cx| page.set_safe_mode(false, cx))
        .unwrap();
    fixture.pump(cx, window, "safe mode off", |page| {
        page.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.safe_mode.is_none())
    });
    let instances = fixture
        .call(methods::LIST_EXTENSION_INSTANCES, json!({}))
        .expect("instances");
    assert_eq!(instances["safeMode"], false);

    // Remove asks first, then reaches the engine; the data stays.
    let second = row(window, cx, "2.0.0");
    window
        .update(cx, |page, _, cx| {
            page.ask(Confirm::Remove(Box::new(second.clone())), cx)
        })
        .unwrap();
    fixture.draw(cx, window);
    let asked = window
        .read_with(cx, |page, _| {
            matches!(page.confirm, Some(Confirm::Remove(_)))
        })
        .unwrap();
    assert!(asked, "remove did not ask first");
    let listed_before = fixture
        .call(methods::LIST_EXTENSIONS, json!({}))
        .expect("list");
    assert_eq!(listed_before["packages"].as_array().unwrap().len(), 2);
    window
        .update(cx, |page, _, cx| page.confirm_destructive(cx))
        .unwrap();
    fixture.pump(cx, window, "2.0.0 removed", |page| {
        listed(page, "2.0.0").is_none()
    });
    let listed_after = fixture
        .call(methods::LIST_EXTENSIONS, json!({}))
        .expect("list");
    let versions: Vec<&str> = listed_after["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|package| package["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, ["1.0.0"]);

    // Erase asks first, then deletes the folder.
    let data = window
        .read_with(cx, |page, _| page.snapshot.as_ref().unwrap().data.clone())
        .unwrap();
    assert_eq!(data.len(), 1, "{data:?}");
    assert_eq!(data[0].extension_id.as_str(), NOTES);
    assert!(data[0].installed);
    assert!(data_summary(&data[0]).starts_with("Schema 1 · 0 bytes · written by v1.0.0"));
    assert_eq!(find(&fixture.data_dir(), "core19.ui+notes").len(), 1);
    window
        .update(cx, |page, _, cx| {
            page.ask(Confirm::Erase(Box::new(data[0].clone())), cx)
        })
        .unwrap();
    fixture.draw(cx, window);
    window
        .update(cx, |page, _, cx| page.confirm_destructive(cx))
        .unwrap();
    fixture.pump(cx, window, "data erased", |page| {
        page.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.data.is_empty())
    });
    assert_eq!(
        find(&fixture.data_dir(), "core19.ui+notes"),
        Vec::<PathBuf>::new(),
        "erased data is still on disk"
    );
    assert_eq!(fixture.grants_file()["data"].get(NOTES), None);
    let error = window.read_with(cx, |page, _| page.error.clone()).unwrap();
    assert_eq!(error, None, "a lifecycle action failed: {error:?}");

    if let Ok(dir) = std::env::var("LUNA_CORE19_EVIDENCE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Path::new(&dir).join("ui-extensions-lifecycle.json"),
            serde_json::to_vec_pretty(&json!({
                "trace": trace,
                "grants": fixture.grants_file(),
                "listed": listed_after,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    fixture.shutdown();
}
