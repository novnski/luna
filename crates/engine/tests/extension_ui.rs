//! CORE-12 / E12 (engine side): UI contributions of installed packages,
//! driven through the public RPC methods with real package bytes, the real
//! install → enable → update → safe-mode → disable path, real worker
//! processes, and readback the engine does not produce: the declaration
//! bytes the test wrote, the grants file, the worker's own journal of what
//! it was asked and with which server-bound target, its side-effect file,
//! and process existence from the OS.
//!
//! The worker is an independently written protocol-1.0 Python peer
//! (`UI_WORKER`). It is configured per package through a listed
//! `fixture.json`, so every behaviour is its own package hash. Set
//! `LUNA_CORE12_EVIDENCE_DIR` to retain each test's readback as JSON.
#![cfg(unix)]

#[path = "support/extension_runtime_fixture.rs"]
mod fixture;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fixture::{
    Engine, NATIVE_CODE, Package, base_manifest, platform, process_exists, sha256,
    wait_process_gone, write_package,
};
use serde_json::{Value, json};

const UI_FILE: &str = "luna-ui.json";

fn evidence(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("LUNA_CORE12_EVIDENCE_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}

/// A package with `contributions` in its manifest and `ui` as the exact
/// bytes of `luna-ui.json` (`None` leaves the file out). `runtime` is
/// `data` or a native entry running [`UI_WORKER`] with `fixture`.
fn ui_package(
    sources: &Path,
    id: &str,
    version: &str,
    native: Option<Value>,
    contributions: Value,
    ui: Option<&[u8]>,
) -> Package {
    let dir = sources.join(format!("{}-{version}", id.replace('/', "+")));
    // `write_package` creates folders only for listed files; a package
    // with none (no declaration, data runtime) still needs its own.
    fs::create_dir_all(&dir).unwrap();
    let runtime = match native {
        None => json!({"kind": "data"}),
        Some(_) => json!({
            "kind": "native",
            "protocol": {"major": 1, "minMinor": 0},
            "entries": {platform(): "bin/worker"},
        }),
    };
    let mut manifest = base_manifest(id, runtime, json!([]));
    manifest["version"] = json!(version);
    manifest["displayName"] = json!("CORE-12 notes");
    manifest["api"] = json!({"major": 1, "minMinor": 3});
    manifest["contributions"] = contributions;
    let fixture = native.map(|fixture| serde_json::to_vec(&fixture).unwrap());
    let mut files: Vec<(&str, &[u8])> = Vec::new();
    if let Some(ui) = ui {
        files.push((UI_FILE, ui));
    }
    if let Some(fixture) = &fixture {
        files.push(("bin/worker", UI_WORKER.as_bytes()));
        files.push(("fixture.json", fixture.as_slice()));
    }
    let hash = write_package(&dir, manifest, &files);
    Package {
        dir,
        id: id.to_owned(),
        hash,
    }
}

fn contribution(id: &str, kind: &str) -> Value {
    json!({"id": id, "kind": kind})
}

async fn list(engine: &Engine) -> Value {
    engine
        .call("ListUiContributions", json!({}))
        .await
        .expect("list UI contributions")
}

fn ids(snapshot: &Value) -> Vec<String> {
    snapshot["contributions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap().to_owned())
        .collect()
}

fn refusal<'a>(snapshot: &'a Value, id: &str) -> Option<&'a str> {
    snapshot["refused"]
        .as_array()
        .unwrap()
        .iter()
        .find(|refusal| refusal["contributionId"] == id)
        .and_then(|refusal| refusal["reason"].as_str())
}

async fn view(engine: &Engine, id: &str, hash: &str, task: Option<&str>) -> Value {
    let mut params = json!({"contributionId": id, "packageHash": hash});
    if let Some(task) = task {
        params["taskId"] = json!(task);
    }
    engine
        .call("GetUiContributionView", params)
        .await
        .expect("view request admitted")
}

async fn act(
    engine: &Engine,
    id: &str,
    hash: &str,
    action: &str,
    task: Option<&str>,
    request: Option<&str>,
) -> Value {
    let mut params = json!({"contributionId": id, "packageHash": hash, "actionId": action});
    if let Some(task) = task {
        params["taskId"] = json!(task);
    }
    if let Some(request) = request {
        params["requestId"] = json!(request);
    }
    engine
        .call("InvokeUiAction", params)
        .await
        .expect("action request admitted")
}

fn lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn find(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name().to_str() == Some(name) {
                found.push(path.clone());
            }
            if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
                pending.push(path);
            }
        }
    }
    found
}

/// The data package's four placements: a command, a settings section, a
/// sidebar page and a task panel, each with a static view or host action.
fn notes_declaration() -> Value {
    json!({
        "declarationVersion": 1,
        "contributions": [
            {
                "id": "core12.e2e/notes.hello",
                "placement": "command",
                "title": "Open notes appearance",
                "actions": [{"id": "run", "title": "Open notes appearance",
                             "run": {"host": {"openSettings": {"section": "appearance"}}}}]
            },
            {
                "id": "core12.e2e/notes.settings",
                "placement": "settingsSection",
                "title": "Notes",
                "icon": "notebook",
                "view": {"static": {"nodes": [
                    {"heading": {"text": "Notes settings"}},
                    {"field": {"label": "Storage", "value": "This profile"}}
                ]}}
            },
            {
                "id": "core12.e2e/notes.page",
                "placement": "sidebarSection",
                "title": "Notes",
                "icon": "list",
                "order": 10,
                "view": {"static": {"nodes": [
                    {"heading": {"text": "Recent notes"}},
                    {"item": {"id": "n1", "title": "First note", "badge": "new", "action": "open"}},
                    "divider",
                    {"button": {"action": "open", "label": "Open settings section"}}
                ]}},
                "actions": [{"id": "open", "title": "Open settings section",
                             "run": {"host": {"openContribution": {"contributionId": "core12.e2e/notes.settings"}}}}]
            },
            {
                "id": "core12.e2e/notes.panel",
                "placement": "panel",
                "title": "Notes",
                "context": "task",
                "view": {"static": {"nodes": [{"text": {"text": "Notes for this task", "tone": "muted"}}]}}
            }
        ]
    })
}

fn notes_contributions() -> Value {
    json!([
        contribution("core12.e2e/notes.hello", "command"),
        contribution("core12.e2e/notes.settings", "settingsSection"),
        contribution("core12.e2e/notes.page", "sidebarSection"),
        contribution("core12.e2e/notes.panel", "panel"),
    ])
}

/// A package's UI contributions exist exactly while it is enabled in this
/// profile and safe mode is off; they survive a restart unchanged; the
/// watch publishes each change; a static view is the declaration's own
/// bytes and a host action is returned, never performed by the engine.
/// Regressions caught: installing places UI; disabling leaves an entry;
/// a restart loses or duplicates entries; safe mode leaves extension views
/// or actions live; the watch misses an enablement change; the engine
/// rewrites a static view or claims a host action it did not declare.
#[tokio::test(flavor = "multi_thread")]
async fn contributions_follow_enablement_restart_and_safe_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let declaration = serde_json::to_vec_pretty(&notes_declaration()).unwrap();
    let notes = ui_package(
        &sources,
        "core12.e2e/notes",
        "1.0.0",
        None,
        notes_contributions(),
        Some(&declaration),
    );
    let engine = Engine::open(&data);

    let mut watch = engine
        .client
        .subscribe_checked("WatchUiContributions", json!({}))
        .await
        .expect("watch UI contributions");
    let first = watch.recv().await.expect("the current snapshot");
    assert_eq!(ids(&first), Vec::<String>::new());
    assert_eq!(first["safeMode"], false);

    let installed = engine
        .call("InstallExtension", json!({"path": notes.dir}))
        .await
        .expect("install");
    assert_eq!(installed["state"], "disabled");
    assert_eq!(
        ids(&list(&engine).await),
        Vec::<String>::new(),
        "installing must not place anything"
    );

    engine
        .set_enabled(&notes, true, &[])
        .await
        .expect("enable the data package with no grants");
    let enabled = tokio::time::timeout(Duration::from_secs(10), watch.recv())
        .await
        .expect("the watch publishes the enablement")
        .expect("a snapshot");
    // Placement order, then each entry's own order, then id.
    assert_eq!(
        ids(&enabled),
        [
            "core12.e2e/notes.hello",
            "core12.e2e/notes.settings",
            "core12.e2e/notes.page",
            "core12.e2e/notes.panel",
        ]
    );
    for entry in enabled["contributions"].as_array().unwrap() {
        assert_eq!(entry["packageHash"], notes.hash, "{entry}");
        assert_eq!(entry["extensionId"], "core12.e2e/notes");
        assert_eq!(entry["extensionName"], "CORE-12 notes");
        assert_eq!(entry["trust"], "data");
    }
    let page = &enabled["contributions"][2];
    assert_eq!(page["placement"], "sidebarSection");
    assert_eq!(page["order"], 10);
    assert_eq!(page["view"], "static");
    assert_eq!(page["actions"][0]["runs"], "host");
    assert_eq!(enabled["contributions"][3]["context"], "task");
    let grants = find(&data, "extension-grants.json");
    assert_eq!(grants.len(), 1, "one profile grants file: {grants:?}");
    let grants: Value = serde_json::from_slice(&fs::read(&grants[0]).unwrap()).unwrap();
    assert_eq!(
        grants["extensions"]["core12.e2e/notes"]["packageHash"],
        notes.hash
    );

    // The view is exactly the declared nodes, read back from the bytes the
    // test wrote, not from the engine.
    let written: Value =
        serde_json::from_slice(&fs::read(notes.dir.join(UI_FILE)).unwrap()).unwrap();
    let page_view = view(&engine, "core12.e2e/notes.page", &notes.hash, None).await;
    assert_eq!(page_view["status"], "ok", "{page_view}");
    assert_eq!(
        page_view["view"],
        written["contributions"][2]["view"]["static"]
    );
    let command = act(
        &engine,
        "core12.e2e/notes.hello",
        &notes.hash,
        "run",
        None,
        None,
    )
    .await;
    assert_eq!(command["status"], "ok", "{command}");
    assert_eq!(
        command["host"],
        json!({"openSettings": {"section": "appearance"}})
    );
    let opened = act(
        &engine,
        "core12.e2e/notes.page",
        &notes.hash,
        "open",
        None,
        None,
    )
    .await;
    assert_eq!(
        opened["host"],
        json!({"openContribution": {"contributionId": "core12.e2e/notes.settings"}})
    );
    let revision = enabled["revision"].clone();
    drop(watch);

    // Restart: nothing is cached; the same entries come from durable state.
    engine.close().await;
    let engine = Engine::open(&data);
    let reopened = list(&engine).await;
    assert_eq!(reopened["revision"], revision, "{reopened}");
    assert_eq!(ids(&reopened), ids(&enabled));

    // Safe mode hides every extension view and refuses its actions.
    engine
        .call("SetExtensionSafeMode", json!({"enabled": true}))
        .await
        .expect("safe mode on");
    let safe = list(&engine).await;
    assert_eq!(safe["safeMode"], true);
    assert_eq!(ids(&safe), Vec::<String>::new());
    for id in ids(&enabled) {
        assert!(
            refusal(&safe, &id).is_some_and(|reason| reason.contains("safe mode")),
            "{id} is not refused for safe mode: {safe}"
        );
    }
    let refused_view = view(&engine, "core12.e2e/notes.page", &notes.hash, None).await;
    assert_eq!(refused_view["status"], "refused");
    assert!(refused_view.get("view").is_none());
    let refused_action = act(
        &engine,
        "core12.e2e/notes.hello",
        &notes.hash,
        "run",
        None,
        None,
    )
    .await;
    assert_eq!(refused_action["status"], "refused");
    assert!(refused_action.get("host").is_none());
    engine
        .call("SetExtensionSafeMode", json!({"enabled": false}))
        .await
        .expect("safe mode off");
    assert_eq!(ids(&list(&engine).await), ids(&enabled));

    // Disabling withdraws everything; a late view request is stale.
    engine
        .set_enabled(&notes, false, &[])
        .await
        .expect("disable");
    let disabled = list(&engine).await;
    assert_eq!(ids(&disabled), Vec::<String>::new());
    let late = view(&engine, "core12.e2e/notes.page", &notes.hash, None).await;
    assert_eq!(late["status"], "stale", "{late}");

    evidence(
        "contributions-follow-enablement",
        &json!({
            "packageHash": notes.hash,
            "declarationSha256": sha256(&declaration),
            "first": first,
            "enabled": enabled,
            "afterRestart": reopened,
            "safeMode": safe,
            "disabled": disabled,
            "pageView": page_view,
            "command": command,
        }),
    );
    engine.close().await;
}

/// Declarations are validated before anything is placed, each refusal
/// names its reason, and one bad contribution does not hide its valid
/// siblings. Regressions caught: a missing, malformed, unknown-field,
/// tampered or wrong-version declaration places entries; a title that
/// names a protected control (Safe mode) is shown beside the host's own;
/// a data package declares code; a panel without a task is placed; an
/// undeclared button action, an unknown icon or host target is accepted;
/// a declaration for an id the manifest does not list is accepted.
#[tokio::test(flavor = "multi_thread")]
async fn invalid_declarations_are_refused_with_their_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let engine = Engine::open(&data);

    let page = |id: &str| contribution(id, "sidebarSection");
    let unlisted = ui_package(
        &sources,
        "core12.bad/unlisted",
        "1.0.0",
        None,
        json!([page("core12.bad/unlisted.page")]),
        None,
    );
    let unknown_field = serde_json::to_vec(&json!({
        "declarationVersion": 1,
        "contributions": [{"id": "core12.bad/fields.page", "placement": "sidebarSection",
                           "title": "Page", "view": {"static": {"nodes": []}}, "onRender": "x"}]
    }))
    .unwrap();
    let fields = ui_package(
        &sources,
        "core12.bad/fields",
        "1.0.0",
        None,
        json!([page("core12.bad/fields.page")]),
        Some(&unknown_field),
    );
    let future =
        serde_json::to_vec(&json!({"declarationVersion": 2, "contributions": []})).unwrap();
    let version = ui_package(
        &sources,
        "core12.bad/version",
        "1.0.0",
        None,
        json!([page("core12.bad/version.page")]),
        Some(&future),
    );
    let stray = serde_json::to_vec(&json!({
        "declarationVersion": 1,
        "contributions": [{"id": "core12.bad/stray.other", "placement": "sidebarSection",
                           "title": "Other", "view": {"static": {"nodes": []}}}]
    }))
    .unwrap();
    let strayed = ui_package(
        &sources,
        "core12.bad/stray",
        "1.0.0",
        None,
        json!([page("core12.bad/stray.page")]),
        Some(&stray),
    );
    let mixed_declaration = serde_json::to_vec(&json!({
        "declarationVersion": 1,
        "contributions": [
            {"id": "core12.bad/mixed.ok", "placement": "command", "title": "Start a task",
             "actions": [{"id": "run", "title": "Start a task", "run": {"host": "newTask"}}]},
            {"id": "core12.bad/mixed.spoof", "placement": "settingsSection", "title": "Safe mode",
             "view": {"static": {"nodes": [{"text": {"text": "off"}}]}}},
            {"id": "core12.bad/mixed.code", "placement": "sidebarSection", "title": "Code",
             "view": {"worker": {"method": "ui.view"}}},
            {"id": "core12.bad/mixed.loose", "placement": "panel", "title": "Loose",
             "view": {"static": {"nodes": [{"text": {"text": "no task"}}]}}},
            {"id": "core12.bad/mixed.button", "placement": "sidebarSection", "title": "Buttons",
             "view": {"static": {"nodes": [{"button": {"action": "missing", "label": "Go"}}]}}},
            {"id": "core12.bad/mixed.icon", "placement": "sidebarSection", "title": "Icon",
             "icon": "rocket", "view": {"static": {"nodes": [{"text": {"text": "x"}}]}}},
            {"id": "core12.bad/mixed.target", "placement": "command", "title": "Elsewhere",
             "actions": [{"id": "run", "title": "Elsewhere",
                          "run": {"host": {"openSettings": {"section": "secrets"}}}}]}
        ]
    }))
    .unwrap();
    let mixed = ui_package(
        &sources,
        "core12.bad/mixed",
        "1.0.0",
        None,
        json!([
            contribution("core12.bad/mixed.ok", "command"),
            contribution("core12.bad/mixed.spoof", "settingsSection"),
            contribution("core12.bad/mixed.code", "sidebarSection"),
            contribution("core12.bad/mixed.loose", "panel"),
            contribution("core12.bad/mixed.button", "sidebarSection"),
            contribution("core12.bad/mixed.icon", "sidebarSection"),
            contribution("core12.bad/mixed.target", "command"),
            contribution("core12.bad/mixed.missing", "sidebarSection"),
        ]),
        Some(&mixed_declaration),
    );
    let good_declaration = serde_json::to_vec(&notes_declaration()).unwrap();
    let tampered = ui_package(
        &sources,
        "core12.e2e/notes",
        "1.0.0",
        None,
        notes_contributions(),
        Some(&good_declaration),
    );
    for package in [&unlisted, &fields, &version, &strayed, &mixed, &tampered] {
        engine.install_enabled(package, &[]).await;
    }
    let before_tamper = list(&engine).await;
    assert!(ids(&before_tamper).contains(&"core12.e2e/notes.page".to_owned()));

    // Change the installed declaration's bytes (the package is read-only).
    let installed = data
        .join("extensions")
        .join("packages")
        .join(&tampered.hash);
    let installed = if installed.exists() {
        installed
    } else {
        find(&data, &tampered.hash)
            .into_iter()
            .find(|path| path.is_dir())
            .expect("the installed package tree")
    };
    let target = installed.join(UI_FILE);
    let mut permissions = fs::metadata(&installed).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&installed, permissions).unwrap();
    let mut permissions = fs::metadata(&target).unwrap().permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&target, permissions).unwrap();
    let mut bytes = fs::read(&target).unwrap();
    bytes.extend_from_slice(b"\n ");
    fs::write(&target, bytes).unwrap();

    let snapshot = list(&engine).await;
    assert_eq!(
        ids(&snapshot),
        ["core12.bad/mixed.ok"],
        "only the valid contribution is placed: {snapshot}"
    );
    let expect = |id: &str, needle: &str| {
        let reason = refusal(&snapshot, id).unwrap_or_else(|| panic!("{id} is not refused"));
        assert!(
            reason.contains(needle),
            "{id}: `{reason}` does not say `{needle}`"
        );
    };
    expect("core12.bad/unlisted.page", "lists no luna-ui.json");
    expect("core12.bad/fields.page", "is invalid");
    expect("core12.bad/version.page", "version 2");
    expect(
        "core12.bad/stray.page",
        "does not list as a UI contribution",
    );
    expect("core12.bad/mixed.spoof", "protected LUNA control");
    expect("core12.bad/mixed.code", "data package runs no code");
    expect("core12.bad/mixed.loose", "declare context `task`");
    expect("core12.bad/mixed.button", "undeclared action `missing`");
    expect("core12.bad/mixed.icon", "the icon `rocket`");
    expect(
        "core12.bad/mixed.target",
        "unknown settings section `secrets`",
    );
    expect("core12.bad/mixed.missing", "has no declaration");
    for id in ids(&before_tamper)
        .into_iter()
        .filter(|id| id.starts_with("core12.e2e/"))
    {
        expect(&id, "changed after installation");
    }
    // A refused contribution has no view, whatever the caller remembers.
    let gone = view(&engine, "core12.e2e/notes.page", &tampered.hash, None).await;
    assert_eq!(gone["status"], "stale");
    assert!(
        gone["message"]
            .as_str()
            .is_some_and(|message| message.contains("changed after installation")),
        "{gone}"
    );
    evidence(
        "invalid-declarations",
        &json!({"beforeTamper": before_tamper, "snapshot": snapshot}),
    );
    engine.close().await;
}

/// The worker package's panel (task context) and command.
fn worker_declaration() -> Value {
    json!({
        "declarationVersion": 1,
        "contributions": [
            {
                "id": "core12.e2e/worker.panel",
                "placement": "panel",
                "title": "Task notes",
                "context": "task",
                "view": {"worker": {"method": "ui.view"}},
                "actions": [
                    {"id": "save", "title": "Save", "run": {"worker": {"method": "ui.save"}}},
                    {"id": "crash", "title": "Crash", "run": {"worker": {"method": "ui.crash"}}}
                ]
            },
            {
                "id": "core12.e2e/worker.new",
                "placement": "command",
                "title": "New task from notes",
                "actions": [{"id": "run", "title": "New task from notes", "run": {"host": "newTask"}}]
            }
        ]
    })
}

fn worker_contributions() -> Value {
    json!([
        contribution("core12.e2e/worker.panel", "panel"),
        contribution("core12.e2e/worker.new", "command"),
    ])
}

fn worker_package(
    sources: &Path,
    version: &str,
    journal: &Path,
    effects: &Path,
    view: &str,
) -> Package {
    let declaration = serde_json::to_vec_pretty(&worker_declaration()).unwrap();
    ui_package(
        &sources.join(view),
        "core12.e2e/worker",
        version,
        Some(json!({
            "journal": journal,
            "effects": effects,
            "view": view,
            "version": version,
        })),
        worker_contributions(),
        Some(&declaration),
    )
}

async fn assert_worker_gone(journal: &Path) {
    for entry in lines(journal) {
        let pid = entry["pid"].as_u64().unwrap();
        assert!(
            wait_process_gone(pid, Duration::from_secs(5)).await,
            "worker {pid} still runs after its view or action"
        );
    }
}

/// Views and actions run only against the exact package and task they were
/// rendered for, start one bounded worker whose server-bound target is
/// that task, never replay an unknown outcome, and leave no process behind.
/// Regressions caught: a callback rendered from an old package hash runs
/// after an update (stale callback); a task panel's request runs without
/// its task, for an unknown task, or bound to another task; a non-task
/// command takes a task; a forged field is accepted; an action whose worker
/// crashed is replayed under the same request id; a worker outlives its
/// call.
#[tokio::test(flavor = "multi_thread")]
async fn stale_and_misdirected_calls_run_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let journal = root.join("journal.jsonl");
    let effects = root.join("effects.jsonl");
    let engine = Engine::open(&data);
    let checkout_a = root.join("checkout-a");
    let checkout_b = root.join("checkout-b");
    fs::create_dir_all(&checkout_a).unwrap();
    fs::create_dir_all(&checkout_b).unwrap();
    engine.create_task("task-a", &checkout_a);
    engine.create_task("task-b", &checkout_b);

    let v1 = worker_package(&sources.join("v1"), "1.0.0", &journal, &effects, "ok");
    engine.install_enabled(&v1, &[NATIVE_CODE]).await;
    let panel = "core12.e2e/worker.panel";
    let listed = list(&engine).await;
    // The published order: placement (command, settings section, sidebar
    // section, panel), then each entry's order, then id.
    assert_eq!(ids(&listed), ["core12.e2e/worker.new", panel]);
    assert_eq!(listed["contributions"][1]["trust"], "native");
    assert_eq!(listed["contributions"][1]["view"], "worker");
    assert!(lines(&journal).is_empty(), "listing started a worker");

    // The view for task A is the worker's, bound by the server to task A.
    let for_a = view(&engine, panel, &v1.hash, Some("task-a")).await;
    assert_eq!(for_a["status"], "ok", "{for_a}");
    assert_eq!(for_a["taskId"], "task-a");
    assert_eq!(for_a["view"]["nodes"][1]["field"]["value"], "task-a");
    let asked = lines(&journal);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0]["method"], "ui.view");
    assert_eq!(asked[0]["target"]["kind"], "task");
    assert_eq!(asked[0]["target"]["taskId"], "task-a");
    assert_worker_gone(&journal).await;

    // Misdirected requests are refused before any worker starts.
    let no_task = view(&engine, panel, &v1.hash, None).await;
    assert_eq!(no_task["status"], "refused", "{no_task}");
    let unknown_task = view(&engine, panel, &v1.hash, Some("task-missing")).await;
    assert_eq!(unknown_task["status"], "refused");
    assert!(
        unknown_task["message"]
            .as_str()
            .is_some_and(|message| message.contains("does not exist")),
        "{unknown_task}"
    );
    let command_with_task = act(
        &engine,
        "core12.e2e/worker.new",
        &v1.hash,
        "run",
        Some("task-a"),
        None,
    )
    .await;
    assert_eq!(command_with_task["status"], "refused");
    let unknown_action = act(&engine, panel, &v1.hash, "delete", Some("task-a"), None).await;
    assert_eq!(unknown_action["status"], "refused");
    let unknown_id = view(&engine, "core12.e2e/worker.gone", &v1.hash, None).await;
    assert_eq!(unknown_id["status"], "stale");
    let forged = engine
        .call(
            "InvokeUiAction",
            json!({"contributionId": panel, "packageHash": v1.hash, "actionId": "save",
                   "taskId": "task-a", "confirmed": true}),
        )
        .await;
    assert!(forged.is_err(), "a forged field was accepted: {forged:?}");
    assert_eq!(
        lines(&journal).len(),
        1,
        "a refused request reached the worker"
    );

    // An action runs once for exactly its task.
    let saved = act(
        &engine,
        panel,
        &v1.hash,
        "save",
        Some("task-a"),
        Some("save-1"),
    )
    .await;
    assert_eq!(saved["status"], "ok", "{saved}");
    assert_eq!(saved["taskId"], "task-a");
    assert_eq!(saved["message"], "Saved for task-a");
    assert_eq!(saved["view"]["nodes"][0]["text"]["text"], "Saved");
    assert_eq!(
        lines(&effects),
        [json!({"taskId": "task-a", "requestId": "save-1"})]
    );
    let asked = lines(&journal);
    assert_eq!(asked.last().unwrap()["target"]["taskId"], "task-a");

    // A worker that exits mid-action is not replayed under that request id.
    let crashed = act(
        &engine,
        panel,
        &v1.hash,
        "crash",
        Some("task-b"),
        Some("crash-1"),
    )
    .await;
    assert_eq!(crashed["status"], "failed", "{crashed}");
    // The supervisor records the unanswered call (CORE-19) as the worker's
    // exit is handled; resend only once it is on record.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = engine.snapshot().await;
        let recorded = snapshot["unresolved"].as_array().is_some_and(|records| {
            records
                .iter()
                .any(|record| record["requestId"] == "crash-1")
        });
        if recorded {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the crashed call was never recorded as unresolved: {snapshot}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let again = act(
        &engine,
        panel,
        &v1.hash,
        "crash",
        Some("task-b"),
        Some("crash-1"),
    )
    .await;
    assert_eq!(again["status"], "failed", "{again}");
    let crashes: Vec<Value> = lines(&effects)
        .into_iter()
        .filter(|line| line.get("crash").is_some())
        .collect();
    assert_eq!(
        crashes,
        [json!({"crash": "crash-1"})],
        "the crashed action ran twice"
    );
    assert_worker_gone(&journal).await;

    // After an update, a callback rendered from 1.0.0 is stale and runs
    // nothing; the new package hash runs.
    let v2 = worker_package(&sources.join("v2"), "1.0.1", &journal, &effects, "ok");
    let installed = engine
        .call("InstallExtension", json!({"path": v2.dir}))
        .await
        .expect("install 1.0.1");
    assert_eq!(installed["state"], "disabled");
    engine
        .call(
            "UpdateExtension",
            json!({"extensionId": v1.id, "fromPackageHash": v1.hash, "toPackageHash": v2.hash,
                   "grants": [NATIVE_CODE], "active": "stop"}),
        )
        .await
        .expect("update to 1.0.1");
    let calls_before = lines(&journal).len();
    let effects_before = lines(&effects);
    let stale_view = view(&engine, panel, &v1.hash, Some("task-a")).await;
    assert_eq!(stale_view["status"], "stale", "{stale_view}");
    assert!(stale_view.get("view").is_none());
    let stale_action = act(
        &engine,
        panel,
        &v1.hash,
        "save",
        Some("task-a"),
        Some("save-2"),
    )
    .await;
    assert_eq!(stale_action["status"], "stale", "{stale_action}");
    assert_eq!(
        lines(&journal).len(),
        calls_before,
        "a stale callback started a worker"
    );
    assert_eq!(
        lines(&effects),
        effects_before,
        "a stale callback had an effect"
    );
    let fresh = view(&engine, panel, &v2.hash, Some("task-b")).await;
    assert_eq!(fresh["status"], "ok", "{fresh}");
    assert_eq!(fresh["view"]["nodes"][1]["field"]["value"], "task-b");
    assert_eq!(
        lines(&journal).last().unwrap()["target"]["taskId"],
        "task-b"
    );
    assert_worker_gone(&journal).await;
    for entry in lines(&journal) {
        assert!(!process_exists(entry["pid"].as_u64().unwrap()));
    }

    evidence(
        "stale-and-misdirected",
        &json!({
            "v1": v1.hash, "v2": v2.hash,
            "forA": for_a, "noTask": no_task, "unknownTask": unknown_task,
            "commandWithTask": command_with_task, "saved": saved,
            "crashed": crashed, "crashResent": again,
            "staleView": stale_view, "staleAction": stale_action, "fresh": fresh,
            "journal": lines(&journal), "effects": lines(&effects),
        }),
    );
    engine.close().await;
}

/// A worker's view is held to the static view's bounds before the shell
/// sees it, and one that never answers costs the caller its deadline, not
/// a hang. Regressions caught: an oversized view or one naming an
/// undeclared action is returned for rendering; a hung worker blocks the
/// request (and so the panel) indefinitely or keeps running.
#[tokio::test(flavor = "multi_thread")]
async fn worker_views_are_bounded_before_the_shell_sees_them() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let engine = Engine::open(&data);
    let checkout = root.join("checkout");
    fs::create_dir_all(&checkout).unwrap();
    engine.create_task("task-a", &checkout);
    let panel = "core12.e2e/worker.panel";
    let mut outcomes = serde_json::Map::new();
    // One version per behaviour: an id and version name one package.
    for (mode, version, needle) in [
        ("oversize", "1.1.0", "bytes; at most 32768"),
        ("undeclared", "1.2.0", "undeclared action `missing`"),
        ("slow", "1.3.0", "did not answer ui.view within 5000 ms"),
    ] {
        let journal = root.join(format!("{mode}.jsonl"));
        let effects = root.join(format!("{mode}-effects.jsonl"));
        let package = worker_package(&sources, version, &journal, &effects, mode);
        engine.install_enabled(&package, &[NATIVE_CODE]).await;
        let started = Instant::now();
        let outcome = view(&engine, panel, &package.hash, Some("task-a")).await;
        let elapsed = started.elapsed();
        assert_eq!(outcome["status"], "failed", "{mode}: {outcome}");
        assert!(outcome.get("view").is_none(), "{mode}: a view was returned");
        assert!(
            outcome["message"]
                .as_str()
                .is_some_and(|message| message.contains(needle)),
            "{mode}: {outcome}"
        );
        // The 5 s deadline, the cancel grace and the stop; never the
        // worker's own 30 s.
        assert!(
            elapsed < Duration::from_secs(12),
            "{mode}: the request took {elapsed:?}"
        );
        assert_worker_gone(&journal).await;
        engine
            .set_enabled(&package, false, &[])
            .await
            .expect("disable");
        outcomes.insert(
            mode.into(),
            json!({"outcome": outcome, "elapsedMs": elapsed.as_millis() as u64}),
        );
    }
    evidence("worker-view-bounds", &Value::Object(outcomes));
    engine.close().await;
}

/// A protocol-1.0 worker written independently of the engine. It records
/// every call (with the target the server bound in its hello) to the
/// journal named in `fixture.json`, and takes its view behaviour from
/// `view`: `ok`, `oversize` (over the 32 KiB view bound, under the 64 KiB
/// frame), `undeclared` (a button for an action the declaration lacks) or
/// `slow` (never answers within the 5 s view deadline).
const UI_WORKER: &str = r##"#!/usr/bin/env python3
"""CORE-12 E12 UI fixture worker (test bytes only, never part of LUNA)."""
import json, os, sys, time

config = json.load(open("fixture.json"))

def send(frame):
    sys.stdout.write(json.dumps(frame, separators=(",", ":")) + "\n")
    sys.stdout.flush()

def record(path, entry):
    with open(path, "a") as out:
        out.write(json.dumps(entry) + "\n")

first = sys.stdin.readline()
if not first:
    sys.exit(0)
hello = json.loads(first)
target = hello["instance"].get("target")
send({
    "type": "hello",
    "id": hello["id"],
    "protocol": {"major": 1, "minor": 0},
    "extensionId": hello["instance"]["extensionId"],
    "packageHash": hello["instance"]["packageHash"],
    "operations": ["ui.view", "ui.save", "ui.crash"],
})

for line in sys.stdin:
    frame = json.loads(line)
    kind = frame.get("type")
    if kind == "shutdown":
        sys.exit(0)
    if kind != "call":
        continue
    fid, method, params = frame["id"], frame["method"], frame.get("params") or {}
    record(config["journal"], {"pid": os.getpid(), "method": method, "params": params, "target": target})
    mode = config.get("view", "ok")
    if method == "ui.view":
        if mode == "slow":
            time.sleep(30)
        if mode == "oversize":
            nodes = [{"text": {"text": "x" * 1900}} for _ in range(20)]
        elif mode == "undeclared":
            nodes = [{"button": {"action": "missing", "label": "Go"}}]
        else:
            nodes = [
                {"heading": {"text": "Task notes"}},
                {"field": {"label": "Task", "value": str(params.get("taskId"))}},
                {"button": {"action": "save", "label": "Save"}},
            ]
        send({"type": "response", "id": fid, "ok": True, "result": {"nodes": nodes}})
    elif method == "ui.save":
        record(config["effects"], {"taskId": params.get("taskId"), "requestId": params.get("requestId")})
        send({"type": "response", "id": fid, "ok": True, "result": {
            "message": "Saved for " + str(params.get("taskId")),
            "view": {"nodes": [{"text": {"text": "Saved"}}]},
        }})
    elif method == "ui.crash":
        record(config["effects"], {"crash": params.get("requestId")})
        os._exit(3)
    else:
        send({"type": "response", "id": fid, "ok": False,
              "error": {"code": "unknownOperation", "message": method}})
"##;
