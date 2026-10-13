//! CORE-19 / E08, E09, E16: extension version switches, rollback, removal,
//! data erasure and safe mode, driven through the engine's public RPC
//! methods with real package bytes, real child processes and readback the
//! engine does not produce: process existence from the OS, the package
//! index and trees, the profile's grants and runtime-state files, and the
//! bytes of every data generation folder at its published path.
//!
//! The worker is an independently written protocol-1.0 peer
//! (`LIFECYCLE_WORKER` below) that keeps its own journal of the calls it
//! applied, so reconciliation is checked against the worker's record rather
//! than the engine's. The package hash is recomputed from the published
//! rule. Interrupted switches are reproduced two ways: a migration worker
//! that really dies or fails mid-migration, and (as in the CORE-06 suite)
//! the exact on-disk residue a power loss leaves at each durable step,
//! followed by a reopen. Set `LUNA_CORE19_EVIDENCE_DIR` to keep each test's
//! readback as JSON.
#![cfg(unix)]

#[path = "support/extension_runtime_fixture.rs"]
mod fixture;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fixture::{
    Engine, NATIVE_CODE, Package, TASKS_READ, pid_of, platform, process_exists, sha256,
    tree_digest, wait_process_gone, write_package,
};
use serde_json::{Value, json};

const NETWORK: &str = "luna.capability/network";
const WORKSPACE_WRITE: &str = "luna.capability/workspace.write";

/// What one fixture package is.
struct Spec<'a> {
    id: &'a str,
    version: &'a str,
    fixture: Value,
    data: Option<Value>,
    capabilities: Value,
    dependencies: Value,
}

impl<'a> Spec<'a> {
    fn new(id: &'a str, version: &'a str) -> Self {
        Self {
            id,
            version,
            fixture: json!({}),
            data: None,
            capabilities: json!([]),
            dependencies: json!([]),
        }
    }

    fn fixture(mut self, fixture: Value) -> Self {
        self.fixture = fixture;
        self
    }

    fn data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    fn capabilities(mut self, capabilities: Value) -> Self {
        self.capabilities = capabilities;
        self
    }

    fn dependencies(mut self, dependencies: Value) -> Self {
        self.dependencies = dependencies;
        self
    }

    /// A native package (extension API 1.2) running [`LIFECYCLE_WORKER`].
    fn write(self, sources: &Path) -> Package {
        let dir = sources.join(format!("{}-{}", self.id.replace('/', "-"), self.version));
        let mut manifest = json!({
            "manifestVersion": 1,
            "id": self.id,
            "version": self.version,
            "displayName": "CORE-19 fixture",
            "publisher": "CORE-19 E16",
            "api": {"major": 1, "minMinor": 2},
            "runtime": {
                "kind": "native",
                "protocol": {"major": 1, "minMinor": 0},
                "entries": {platform(): "bin/worker"},
            },
            "capabilities": self.capabilities,
            "dependencies": self.dependencies,
        });
        if let Some(data) = self.data {
            manifest["data"] = data;
        }
        let mut fixture = self.fixture;
        fixture["version"] = json!(self.version);
        let config = serde_json::to_vec(&fixture).unwrap();
        let hash = write_package(
            &dir,
            manifest,
            &[
                ("bin/worker", LIFECYCLE_WORKER.as_bytes()),
                ("fixture.json", config.as_slice()),
            ],
        );
        Package {
            dir,
            id: self.id.to_owned(),
            hash,
        }
    }
}

/// The published layout, independent of the engine.
struct Layout {
    data: PathBuf,
    store: PathBuf,
}

impl Layout {
    fn new(data: &Path) -> Self {
        Self {
            data: data.to_path_buf(),
            store: data.join("orgs/org-a/user-a"),
        }
    }

    fn grants_path(&self) -> PathBuf {
        self.store.join("extension-grants.json")
    }

    fn grants(&self) -> Value {
        read_json(&self.grants_path())
    }

    fn grants_hash(&self) -> String {
        sha256(&fs::read(self.grants_path()).unwrap())
    }

    fn runtime_path(&self) -> PathBuf {
        self.store.join("extension-runtime.json")
    }

    fn runtime_state(&self) -> Value {
        read_json(&self.runtime_path())
    }

    fn index(&self) -> Value {
        read_json(&self.data.join("extensions/installed.json"))
    }

    fn installed_hashes(&self) -> Vec<String> {
        self.index()["packages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["packageHash"].as_str().unwrap().to_owned())
            .collect()
    }

    fn package_dir(&self, package: &Package) -> PathBuf {
        self.data.join("extensions/packages").join(&package.hash)
    }

    /// `{store}/extensions/data/<id with / as +>/`.
    fn data_dir(&self, id: &str) -> PathBuf {
        self.store
            .join("extensions/data")
            .join(id.replace('/', "+"))
    }

    fn generation(&self, id: &str, generation: &str) -> PathBuf {
        self.data_dir(id).join(generation)
    }

    /// Generation folder names on disk, sorted.
    fn generations(&self, id: &str) -> Vec<String> {
        let mut names: Vec<String> = match fs::read_dir(self.data_dir(id)) {
            Ok(entries) => entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        };
        names.sort();
        names
    }

    /// The grants file's data record for `id`.
    fn data_record(&self, id: &str) -> Value {
        self.grants()["data"][id].clone()
    }

    fn current_generation(&self, id: &str) -> String {
        self.data_record(id)["current"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{id} has no current data: {}", self.grants()))
            .to_owned()
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(
        &fs::read(path)
            .unwrap_or_else(|error| panic!("{} could not be read: {error}", path.display())),
    )
    .unwrap()
}

fn evidence(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("LUNA_CORE19_EVIDENCE_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}

fn update(from: &Package, to: &Package, grants: &[&str], active: &str) -> Value {
    json!({
        "extensionId": to.id,
        "fromPackageHash": from.hash,
        "toPackageHash": to.hash,
        "grants": grants,
        "active": active,
    })
}

async fn switch(engine: &Engine, params: Value) -> Result<Value, String> {
    engine.call("UpdateExtension", params).await
}

async fn started(engine: &Engine, package: &Package) -> (String, u64) {
    let view = engine.start(package, None, &[]).await.expect("start");
    assert_eq!(view["state"], "running", "{view}");
    (
        view["instanceId"].as_str().unwrap().to_owned(),
        pid_of(&view),
    )
}

async fn put(engine: &Engine, instance: &str, path: &str, text: &str) {
    let answer = engine
        .ok(instance, "fixture.put", json!({"path": path, "text": text}))
        .await;
    assert_eq!(answer["ok"], true, "data.write failed: {answer}");
}

/// What the worker reads back through `data.read`: the text, or the
/// broker's error code.
async fn get(engine: &Engine, instance: &str, path: &str) -> Value {
    engine
        .ok(instance, "fixture.get", json!({"path": path}))
        .await
}

async fn packages(engine: &Engine) -> Value {
    let snapshot = engine
        .call("ListExtensions", json!({}))
        .await
        .expect("list extensions");
    assert_eq!(snapshot["grantsError"], Value::Null, "{snapshot}");
    snapshot
}

async fn package_view(engine: &Engine, package: &Package) -> Value {
    packages(engine).await["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|view| view["packageHash"] == package.hash)
        .cloned()
        .unwrap_or_else(|| panic!("{} {} is not listed", package.id, package.hash))
}

async fn wait_until(what: &str, within: Duration, mut done: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !done().await {
        assert!(
            Instant::now() < deadline,
            "{what} did not happen within {within:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A refused install or switch changes nothing: the grants file, the
/// enabled package, its running worker and its data stay exactly as they
/// were, and an accepted switch grants exactly what the request names.
/// Regressions caught: a tampered candidate is staged or installed; a switch
/// with an unmet dependency, a dependent's range, a missing or unrequested
/// grant, a stale `from` hash, a data schema the candidate cannot migrate or
/// a candidate changed after install stops the running worker or rewrites
/// grants; a capability the new version adds is carried over silently; the
/// data is lost across a compatible update.
#[tokio::test(flavor = "multi_thread")]
async fn refused_installs_and_switches_change_nothing_and_grants_never_widen() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let layout = Layout::new(&data);
    let notes = "core19.fixture/notes";
    let base = "core19.fixture/base";
    let schema1 = json!({"schemaVersion": 1});
    let on_base = |min: &str| json!([{"id": base, "minVersion": min}]);

    let base1 = Spec::new(base, "1.0.0").write(&sources);
    let base2 = Spec::new(base, "2.0.0").write(&sources);
    let v1 = Spec::new(notes, "1.0.0")
        .data(schema1.clone())
        .dependencies(on_base("1.0.0"))
        .write(&sources);
    let dependent = Spec::new("core19.fixture/dependent", "1.0.0")
        .dependencies(json!([{"id": notes, "minVersion": "1.0.0", "maxVersion": "2.0.0"}]))
        .write(&sources);

    let engine = Engine::open(&data);
    engine.install_enabled(&base1, &[NATIVE_CODE]).await;
    engine
        .call("InstallExtension", json!({"path": base2.dir}))
        .await
        .expect("install base 2.0.0");
    engine.install_enabled(&v1, &[NATIVE_CODE]).await;
    engine.install_enabled(&dependent, &[NATIVE_CODE]).await;
    let (instance, pid) = started(&engine, &v1).await;
    put(
        &engine,
        &instance,
        "state.json",
        r#"{"schema":1,"count":5}"#,
    )
    .await;
    let generation = layout.current_generation(notes);
    let data_before = tree_digest(&layout.generation(notes, &generation));
    let grants_before = layout.grants_hash();

    // A tampered candidate never stages or installs.
    let tampered = Spec::new(notes, "1.9.0")
        .data(schema1.clone())
        .dependencies(on_base("1.0.0"))
        .write(&sources);
    // Same size, one byte different: only the hash can tell.
    let config = tampered.dir.join("fixture.json");
    let changed = fs::read_to_string(&config)
        .unwrap()
        .replace("1.9.0", "1.8.0");
    fs::write(&config, changed).unwrap();
    let error = engine
        .call("InstallExtension", json!({"path": tampered.dir}))
        .await
        .expect_err("a tampered candidate must not install");
    assert!(
        error.contains("does not match its manifest hash"),
        "{error}"
    );
    assert!(!layout.installed_hashes().contains(&tampered.hash));
    assert!(!layout.package_dir(&tampered).exists());
    let staging = data.join("extensions/staging");
    assert!(
        !staging.exists() || fs::read_dir(&staging).unwrap().next().is_none(),
        "a refused install left staging behind"
    );

    let needs_base2 = Spec::new(notes, "1.1.0")
        .data(schema1.clone())
        .dependencies(on_base("2.0.0"))
        .write(&sources);
    let major = Spec::new(notes, "2.0.0")
        .data(schema1.clone())
        .dependencies(on_base("1.0.0"))
        .write(&sources);
    let expanded = Spec::new(notes, "1.2.0")
        .data(schema1.clone())
        .dependencies(on_base("1.0.0"))
        .capabilities(json!([
            {"id": TASKS_READ, "reason": "list tasks"},
            {"id": NETWORK, "required": false},
        ]))
        .write(&sources);
    let no_migration = Spec::new(notes, "1.3.0")
        .data(json!({"schemaVersion": 2}))
        .dependencies(on_base("1.0.0"))
        .write(&sources);
    let changed_later = Spec::new(notes, "1.4.0")
        .data(schema1.clone())
        .dependencies(on_base("1.0.0"))
        .write(&sources);
    for candidate in [
        &needs_base2,
        &major,
        &expanded,
        &no_migration,
        &changed_later,
    ] {
        let installed = engine
            .call("InstallExtension", json!({"path": candidate.dir}))
            .await
            .expect("install a candidate");
        assert_eq!(
            installed["state"], "disabled",
            "installing enabled a candidate"
        );
    }
    let installed_worker = layout.package_dir(&changed_later).join("bin/worker");
    fs::set_permissions(&installed_worker, fs::Permissions::from_mode(0o755)).unwrap();
    let mut bytes = fs::read(&installed_worker).unwrap();
    bytes.extend_from_slice(b"\n# changed after install\n");
    fs::write(&installed_worker, &bytes).unwrap();

    // The listing explains each candidate before anything is attempted.
    let view = package_view(&engine, &expanded).await;
    assert_eq!(view["switch"]["kind"], "update", "{view}");
    assert_eq!(view["switch"]["fromVersion"], "1.0.0");
    assert_eq!(
        view["switch"]["newCapabilities"],
        json!([TASKS_READ, NETWORK])
    );
    assert_eq!(
        view["switch"]["data"],
        json!({"kind": "keep", "schemaVersion": 1})
    );
    assert_eq!(view["switch"]["blocked"], Value::Null);
    let view = package_view(&engine, &major).await;
    assert!(
        view["switch"]["blocked"]
            .as_str()
            .is_some_and(|reason| reason.contains("would break it")),
        "{view}"
    );

    let mut refusals = Vec::new();
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "unmet dependency",
            update(&v1, &needs_base2, &[NATIVE_CODE], "drain"),
            "requires core19.fixture/base >= 2.0.0; the enabled version is 1.0.0",
        ),
        (
            "dependent range",
            update(&v1, &major, &[NATIVE_CODE], "drain"),
            "core19.fixture/dependent 1.0.0 requires core19.fixture/notes >= 1.0.0, < 2.0.0; 2.0.0 would break it",
        ),
        (
            "new required capability not granted",
            update(&v1, &expanded, &[NATIVE_CODE], "drain"),
            "luna.capability/tasks.read is required",
        ),
        (
            "unrequested capability",
            update(
                &v1,
                &expanded,
                &[NATIVE_CODE, TASKS_READ, WORKSPACE_WRITE],
                "drain",
            ),
            "luna.capability/workspace.write was not requested",
        ),
        (
            "stale from hash",
            update(&needs_base2, &expanded, &[NATIVE_CODE, TASKS_READ], "drain"),
            "is the enabled version, not",
        ),
        (
            "unmigratable data",
            update(&v1, &no_migration, &[NATIVE_CODE], "drain"),
            "cannot migrate this profile's schema 1 (it declares no migration)",
        ),
        (
            "changed after install",
            update(&v1, &changed_later, &[NATIVE_CODE], "drain"),
            "failed verification",
        ),
    ];
    for (case, params, expected) in cases {
        let error = switch(&engine, params)
            .await
            .expect_err(&format!("{case} must be refused"));
        assert!(error.contains(expected), "{case}: {error}");
        assert_eq!(layout.grants_hash(), grants_before, "{case} rewrote grants");
        assert!(process_exists(pid), "{case} stopped the running worker");
        let view = engine.instance(&instance).await;
        assert_eq!(view["state"], "running", "{case}: {view}");
        assert_eq!(view["generation"], 1, "{case} restarted the worker");
        refusals.push(json!({"case": case, "error": error}));
    }
    // A switch without an explicit grant set is malformed, not "keep them".
    let mut missing = update(&v1, &expanded, &[], "drain");
    missing.as_object_mut().unwrap().remove("grants");
    let error = switch(&engine, missing)
        .await
        .expect_err("grants are required");
    assert!(error.contains("grants"), "{error}");
    assert_eq!(layout.grants_hash(), grants_before);
    assert_eq!(
        get(&engine, &instance, "state.json").await["text"],
        r#"{"schema":1,"count":5}"#
    );
    assert_eq!(
        tree_digest(&layout.generation(notes, &generation)),
        data_before
    );
    assert_eq!(layout.generations(notes), vec![generation.clone()]);

    // Accepted: exactly the named grants, the optional new one left off.
    let outcome = switch(
        &engine,
        update(&v1, &expanded, &[NATIVE_CODE, TASKS_READ], "drain"),
    )
    .await
    .expect("switch to 1.2.0");
    assert_eq!(outcome["kind"], "update");
    assert_eq!(outcome["fromVersion"], "1.0.0");
    assert_eq!(outcome["expandedGrants"], json!([TASKS_READ]));
    assert_eq!(outcome["stoppedInstances"], json!([instance]));
    assert_eq!(outcome["package"]["state"], "enabled");
    let grants = layout.grants();
    assert_eq!(grants["extensions"][notes]["packageHash"], expanded.hash);
    assert_eq!(
        grants["extensions"][notes]["grants"],
        json!([NATIVE_CODE, TASKS_READ]),
        "the update widened or narrowed the named grants"
    );
    assert!(wait_process_gone(pid, Duration::from_secs(3)).await);
    let old = engine.instance(&instance).await;
    assert_eq!(old["state"], "stopped", "{old}");
    assert_eq!(old["lastExit"]["reason"], "updating");
    assert_eq!(
        layout.current_generation(notes),
        generation,
        "compatible data was replaced"
    );
    let (renewed, _) = started(&engine, &expanded).await;
    assert_eq!(
        get(&engine, &renewed, "state.json").await["text"],
        r#"{"schema":1,"count":5}"#,
        "the data did not carry over"
    );
    let error = engine
        .start(&v1, None, &[])
        .await
        .expect_err("the replaced version must not start");
    assert!(error.contains("not enabled"), "{error}");
    engine.close().await;
    evidence(
        "e08-e16-refused-switches",
        &json!({
            "refusals": refusals,
            "outcome": outcome,
            "grants": grants,
            "oldInstance": old,
            "dataGeneration": generation,
            "dataDigest": data_before,
        }),
    );
}

/// The enabled version's instances are pinned to its hash. A drain takes no
/// new calls and waits for the ones in progress; a drain that cannot finish
/// changes nothing; a deliberate stop records every unanswered call as
/// unresolved, and the next version's `luna.reconcile` settles each one from
/// the worker's own journal. Regressions caught: a worker is replaced under
/// a call in progress, a new call or start slips into a draining version, a
/// drain timeout switches anyway, an interrupted call is replayed, a resend
/// of an unknown or applied request runs again, a resolution is lost on
/// restart.
#[tokio::test(flavor = "multi_thread")]
async fn active_runs_drain_or_stop_and_unknown_outcomes_are_reconciled_not_replayed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let layout = Layout::new(&data);
    let journal = root.join("journal.jsonl");
    let effects = root.join("effects.txt");
    let notes = "core19.fixture/notes";
    let worker = json!({"reconcile": true, "journal": journal, "effects": effects});
    let v1 = Spec::new(notes, "1.0.0")
        .fixture(worker.clone())
        .write(&sources);
    let v2 = Spec::new(notes, "2.0.0")
        .fixture(worker.clone())
        .write(&sources);
    let v3 = Spec::new(notes, "3.0.0")
        .fixture(worker.clone())
        .write(&sources);

    let engine = Engine::open(&data);
    engine.install_enabled(&v1, &[NATIVE_CODE]).await;
    for candidate in [&v2, &v3] {
        engine
            .call("InstallExtension", json!({"path": candidate.dir}))
            .await
            .expect("install");
    }
    let (a, a_pid) = started(&engine, &v1).await;

    // Drain: the call in progress finishes; nothing new gets in.
    let slow = engine.invoke(&a, "fixture.slow", json!({"ms": 700}), 10_000);
    let drained = async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut params = update(&v1, &v2, &[NATIVE_CODE], "drain");
        params["drainTimeoutMs"] = json!(10_000);
        switch(&engine, params).await
    };
    let observed = async {
        wait_until("the drain", Duration::from_secs(3), async || {
            engine.instance(&a).await["state"] == "draining"
        })
        .await;
        let refused = engine
            .invoke(&a, "fixture.echo", json!({}), 1000)
            .await
            .expect_err("a draining instance takes no new calls");
        let start = engine
            .start(&v1, None, &[])
            .await
            .expect_err("no start while the version switches");
        (refused, start)
    };
    let (slow, drained, (refused, start)) = tokio::join!(slow, drained, observed);
    let slow = slow.expect("the slow call was admitted");
    assert_eq!(
        slow["status"], "ok",
        "the drain cut the call in progress: {slow}"
    );
    assert!(refused.contains("takes no new calls"), "{refused}");
    assert!(start.contains("being updated"), "{start}");
    let drained = drained.expect("the drained switch");
    assert_eq!(drained["stoppedInstances"], json!([a]));
    assert!(wait_process_gone(a_pid, Duration::from_secs(3)).await);
    assert_eq!(engine.instance(&a).await["lastExit"]["reason"], "updating");
    assert_eq!(layout.grants()["extensions"][notes]["packageHash"], v2.hash);

    // Two calls that will never answer: y before applying, x after.
    let (b, b_pid) = started(&engine, &v2).await;
    let held = |line: &str, hold: &str, request: &str| {
        engine.call(
            "CallExtensionInstance",
            json!({
                "instanceId": b,
                "method": "fixture.apply",
                "params": {"line": line, "hold": hold},
                "timeoutMs": 30_000,
                "requestId": request,
            }),
        )
    };
    let held_y = held("y", "before", "req-y");
    let held_x = held("x", "after", "req-x");
    let driver = async {
        wait_until("x to apply", Duration::from_secs(3), async || {
            fs::read_to_string(&effects).is_ok_and(|text| text.lines().any(|line| line == "x"))
        })
        .await;
        let grants = layout.grants_hash();
        let mut params = update(&v2, &v3, &[NATIVE_CODE], "drain");
        params["drainTimeoutMs"] = json!(300);
        let timed_out = switch(&engine, params)
            .await
            .expect_err("a drain that cannot finish must change nothing");
        let after_timeout = engine.instance(&b).await;
        assert_eq!(layout.grants_hash(), grants, "a timed-out drain switched");
        let stopped = switch(&engine, update(&v2, &v3, &[NATIVE_CODE], "stop")).await;
        (timed_out, after_timeout, stopped)
    };
    let (held_y, held_x, (timed_out, after_timeout, stopped)) =
        tokio::join!(held_y, held_x, driver);
    assert!(
        timed_out.contains("still in progress after 300 ms"),
        "{timed_out}"
    );
    assert_eq!(after_timeout["state"], "running", "{after_timeout}");
    assert_eq!(pid_of(&after_timeout), b_pid);
    let stopped = stopped.expect("the deliberate stop");
    assert_eq!(stopped["stoppedInstances"], json!([b]));
    let held_y = held_y.expect("y was admitted");
    let held_x = held_x.expect("x was admitted");
    for held in [&held_y, &held_x] {
        assert_eq!(held["status"], "workerExited", "{held}");
        assert_eq!(held["exit"]["reason"], "updating", "{held}");
    }
    assert!(wait_process_gone(b_pid, Duration::from_secs(3)).await);

    // Both are unresolved, durably, under their exact call ids.
    let ledger = engine.snapshot().await["unresolved"].clone();
    let record = |request: &str| -> Value {
        ledger
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["requestId"] == request)
            .cloned()
            .unwrap_or_else(|| panic!("{request} is not in the ledger: {ledger}"))
    };
    assert_eq!(record("req-x")["callId"], held_x["callId"]);
    assert_eq!(record("req-y")["callId"], held_y["callId"]);
    for request in ["req-x", "req-y"] {
        let record = record(request);
        assert_eq!(record["version"], "2.0.0");
        assert_eq!(record["exitReason"], "updating");
        assert_eq!(record["resolution"], Value::Null);
    }
    let state = layout.runtime_state();
    assert_eq!(
        state["version"], 2,
        "the ledger was written as a file older LUNA rewrites"
    );
    assert_eq!(state["unresolved"].as_array().unwrap().len(), 2);

    // An unknown outcome is not sent again.
    let resend = |instance: &str, line: &str, request: &str| {
        engine.call(
            "CallExtensionInstance",
            json!({
                "instanceId": instance,
                "method": "fixture.apply",
                "params": {"line": line},
                "timeoutMs": 5000,
                "requestId": request,
            }),
        )
    };
    let error = resend(&b, "x", "req-x")
        .await
        .expect_err("an unknown outcome must not be resent");
    assert!(error.contains("outcome is unknown"), "{error}");

    // The next version reconciles from the worker's own journal.
    let (c, _) = started(&engine, &v3).await;
    let ledger = engine.snapshot().await["unresolved"].clone();
    let resolution = |request: &str| -> Value {
        ledger
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["requestId"] == request)
            .map(|record| record["resolution"].clone())
            .unwrap()
    };
    assert_eq!(resolution("req-x")["outcome"], "applied", "{ledger}");
    assert_eq!(resolution("req-y")["outcome"], "notApplied", "{ledger}");
    assert_eq!(resolution("req-x")["resolvedByVersion"], "3.0.0");
    assert_eq!(resolution("req-x")["resolvedByPackageHash"], v3.hash);
    let error = resend(&c, "x", "req-x")
        .await
        .expect_err("an applied request must not run again");
    assert!(error.contains("already applied"), "{error}");
    let resent = resend(&c, "y", "req-y").await.expect("y was not applied");
    assert_eq!(resent["status"], "ok", "{resent}");
    assert_eq!(
        fs::read_to_string(&effects).unwrap(),
        "x\ny\n",
        "a call was replayed or lost"
    );
    engine.close().await;

    let engine = Engine::open(&data);
    let reopened = engine.snapshot().await;
    assert_eq!(
        reopened["instances"],
        json!([]),
        "an instance was resurrected"
    );
    let kept: Vec<&Value> = reopened["unresolved"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|record| record["resolution"]["outcome"] == "applied")
        .collect();
    assert_eq!(
        kept.len(),
        1,
        "the resolution was lost on restart: {reopened}"
    );
    assert_eq!(fs::read_to_string(&effects).unwrap(), "x\ny\n");
    engine.close().await;
    evidence(
        "e09-e16-pinned-runs-and-reconciliation",
        &json!({
            "drained": drained,
            "slowCall": slow,
            "drainTimeout": timed_out,
            "stopped": stopped,
            "heldCalls": [held_x, held_y],
            "ledger": ledger,
            "runtimeState": state,
            "reopened": reopened,
            "effects": fs::read_to_string(&effects).unwrap(),
            "journal": fs::read_to_string(&journal).unwrap(),
        }),
    );
}

/// A crash loop quarantines the exact hash; removing the package stops its
/// workers, deletes its grant, index entry and tree, and releases the
/// quarantine, but keeps the profile's data, which only an explicit erase
/// removes. Regressions caught: removal under an enabled dependent, a
/// quarantine that outlives the package (and blocks a clean reinstall),
/// uninstall that erases data, erase of data in use, erase that leaves a
/// copy or a pointer behind.
#[tokio::test(flavor = "multi_thread")]
async fn removal_releases_quarantine_and_keeps_data_until_it_is_erased() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let layout = Layout::new(&data);
    let notes = "core19.fixture/notes";
    let v1 = Spec::new(notes, "1.0.0")
        .data(json!({"schemaVersion": 1}))
        .write(&sources);
    let user = Spec::new("core19.fixture/user", "1.0.0")
        .dependencies(json!([{"id": notes, "minVersion": "1.0.0"}]))
        .write(&sources);

    let engine = Engine::open(&data);
    engine.install_enabled(&v1, &[NATIVE_CODE]).await;
    engine.install_enabled(&user, &[NATIVE_CODE]).await;
    let (a, _) = started(&engine, &v1).await;
    put(&engine, &a, "state.json", "kept").await;
    let generation = layout.current_generation(notes);

    let remove = json!({"extensionId": v1.id, "packageHash": v1.hash});
    let error = engine
        .call("RemoveExtension", remove.clone())
        .await
        .expect_err("an enabled dependent blocks removal");
    assert!(error.contains("core19.fixture/user depends on"), "{error}");
    assert!(layout.package_dir(&v1).exists());
    engine
        .set_enabled(&user, false, &[])
        .await
        .expect("disable the dependent");

    // Three crashes quarantine the exact hash.
    let mut pids = Vec::new();
    for round in 1..=3_u64 {
        pids.push(pid_of(&engine.instance(&a).await));
        let crash = engine
            .invoke(&a, "fixture.crash", json!({}), 10_000)
            .await
            .unwrap();
        assert_eq!(crash["status"], "workerExited", "{crash}");
        if round < 3 {
            engine.ok(&a, "fixture.echo", json!({})).await;
        }
    }
    let quarantined = engine.snapshot().await["quarantined"].clone();
    assert_eq!(quarantined[0]["packageHash"], v1.hash, "{quarantined}");
    assert!(
        layout.runtime_state()["quarantined"]
            .get(&v1.hash)
            .is_some()
    );

    let removed = engine
        .call("RemoveExtension", remove.clone())
        .await
        .expect("remove");
    assert_eq!(removed["wasEnabled"], true);
    assert_eq!(removed["quarantineReleased"], true);
    assert_eq!(removed["dataKept"], true);
    assert!(!layout.installed_hashes().contains(&v1.hash));
    assert!(
        !layout.package_dir(&v1).exists(),
        "the tree outlived removal"
    );
    let grants = layout.grants();
    assert_eq!(
        grants["extensions"].get(notes),
        None,
        "the grant outlived removal"
    );
    assert!(
        layout.runtime_state()["quarantined"]
            .get(&v1.hash)
            .is_none(),
        "the quarantine outlived removal"
    );
    for pid in &pids {
        assert!(
            !process_exists(*pid),
            "a worker of the removed package runs"
        );
    }
    assert_eq!(
        fs::read_to_string(layout.generation(notes, &generation).join("state.json")).unwrap(),
        "kept",
        "removal erased data"
    );
    let listed = packages(&engine).await;
    assert_eq!(listed["data"][0]["extensionId"], notes, "{listed}");
    assert_eq!(listed["data"][0]["installed"], false);

    // A clean reinstall of the same bytes is not quarantined and finds its data.
    engine.install_enabled(&v1, &[NATIVE_CODE]).await;
    assert_eq!(engine.snapshot().await["quarantined"], json!([]));
    let (b, _) = started(&engine, &v1).await;
    assert_eq!(get(&engine, &b, "state.json").await["text"], "kept");

    let erase = json!({"extensionId": notes});
    let error = engine
        .call("EraseExtensionData", erase.clone())
        .await
        .expect_err("data in use must not be erased");
    assert!(error.contains("is running"), "{error}");
    assert!(layout.generation(notes, &generation).exists());
    engine.stop(&b).await;
    let erased = engine
        .call("EraseExtensionData", erase.clone())
        .await
        .expect("erase");
    assert_eq!(erased["erasedCopies"], 1);
    assert_eq!(erased["erasedBytes"], 4);
    assert!(
        !layout.data_dir(notes).exists(),
        "a data copy outlived the erase"
    );
    let grants = layout.grants();
    assert_eq!(
        grants["data"].get(notes),
        None,
        "the data pointer outlived the erase"
    );
    assert_eq!(
        grants["extensions"][notes]["packageHash"], v1.hash,
        "erasing data changed enablement"
    );
    let (c, _) = started(&engine, &v1).await;
    assert_eq!(get(&engine, &c, "state.json").await["code"], "notFound");
    engine.close().await;
    evidence(
        "e09-e16-removal-and-erase",
        &json!({
            "quarantined": quarantined,
            "removed": removed,
            "erased": erased,
            "grants": grants,
        }),
    );
}

/// A newer schema is produced by the new version on a copy. A migration
/// that fails, crashes, or that the candidate cannot run leaves the old
/// version enabled with byte-identical data and no stray copy or process.
/// A rollback restores the kept copy instead of letting older code read the
/// newer schema; without a kept copy it is refused, by switch or by
/// disable-and-enable. A power loss at each durable step reopens to one
/// version with its own data. Regressions caught: a half-migrated copy
/// becomes current, the old data is touched, a migration worker outlives
/// its failure, a rollback silently reads a newer schema, crash residue
/// becomes current or deletes kept data, a removed package stays
/// quarantined.
#[tokio::test(flavor = "multi_thread")]
async fn migrations_commit_on_a_copy_and_rollback_never_reads_a_newer_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let layout = Layout::new(&data);
    let notes = "core19.fixture/notes";
    let schema2 = json!({"schemaVersion": 2, "migratesFrom": 1});
    let v1 = Spec::new(notes, "1.0.0")
        .data(json!({"schemaVersion": 1}))
        .write(&sources);
    let fails = Spec::new(notes, "2.0.0")
        .data(schema2.clone())
        .fixture(json!({"migrate": "fail"}))
        .write(&sources);
    let crashes = Spec::new(notes, "2.0.1")
        .data(schema2.clone())
        .fixture(json!({"migrate": "crash"}))
        .write(&sources);
    let cannot = Spec::new(notes, "2.0.2")
        .data(schema2.clone())
        .write(&sources);
    let v2 = Spec::new(notes, "2.1.0")
        .data(schema2.clone())
        .fixture(json!({"migrate": "ok"}))
        .write(&sources);
    let original = r#"{"schema":1,"count":5}"#;

    let engine = Engine::open(&data);
    engine.install_enabled(&v1, &[NATIVE_CODE]).await;
    for candidate in [&fails, &crashes, &cannot, &v2] {
        engine
            .call("InstallExtension", json!({"path": candidate.dir}))
            .await
            .expect("install");
    }
    let (a, _) = started(&engine, &v1).await;
    put(&engine, &a, "state.json", original).await;
    let g1 = layout.current_generation(notes);
    let d1 = tree_digest(&layout.generation(notes, &g1));
    let grants_before = layout.grants_hash();

    let mut failures = Vec::new();
    for (candidate, expected) in [
        (
            &fails,
            "migration from data schema 1 to 2 failed (migrationFailed",
        ),
        (
            &crashes,
            "migration from data schema 1 to 2 failed (workerExited",
        ),
        (
            &cannot,
            "does not implement the required operation luna.migrate",
        ),
    ] {
        let error = switch(&engine, update(&v1, candidate, &[NATIVE_CODE], "stop"))
            .await
            .expect_err("a failed migration must not switch");
        assert!(error.contains(expected), "{}: {error}", candidate.hash);
        assert!(
            error.contains("1.0.0 is still the enabled version"),
            "{error}"
        );
        assert_eq!(
            layout.grants_hash(),
            grants_before,
            "a failed migration switched"
        );
        assert_eq!(
            tree_digest(&layout.generation(notes, &g1)),
            d1,
            "the old data changed"
        );
        assert_eq!(
            layout.generations(notes),
            vec![g1.clone()],
            "a migration copy remains"
        );
        let migrations: Vec<Value> = engine
            .instances_of(candidate)
            .await
            .into_iter()
            .filter(|view| view["purpose"] == "migration")
            .collect();
        for view in &migrations {
            assert_eq!(
                view["pid"],
                Value::Null,
                "a migration worker still runs: {view}"
            );
            if let Some(pid) = view["lastExit"]["pid"].as_u64() {
                assert!(wait_process_gone(pid, Duration::from_secs(3)).await);
            }
        }
        failures
            .push(json!({"candidate": candidate.hash, "error": error, "migrations": migrations}));
    }
    let (check, _) = started(&engine, &v1).await;
    assert_eq!(get(&engine, &check, "state.json").await["text"], original);

    // A migration that succeeds commits its copy and keeps the old one.
    let outcome = switch(&engine, update(&v1, &v2, &[NATIVE_CODE], "stop"))
        .await
        .expect("migrate to 2.1.0");
    assert_eq!(
        outcome["data"],
        json!({"kind": "migrate", "fromSchemaVersion": 1, "toSchemaVersion": 2})
    );
    let record = layout.data_record(notes);
    let g2 = record["current"]["id"].as_str().unwrap().to_owned();
    assert_ne!(g2, g1);
    assert_eq!(record["current"]["schemaVersion"], 2);
    assert_eq!(record["current"]["packageHash"], v2.hash);
    assert_eq!(record["kept"][0]["id"], g1.as_str());
    assert_eq!(record["kept"][0]["schemaVersion"], 1);
    assert_eq!(layout.grants()["version"], 2);
    assert_eq!(
        tree_digest(&layout.generation(notes, &g1)),
        d1,
        "migration touched the old copy"
    );
    let migrated: Value = read_json(&layout.generation(notes, &g2).join("state.json"));
    assert_eq!(
        migrated,
        json!({"schema": 2, "count": 5, "migratedBy": "2.1.0"})
    );
    let (b, _) = started(&engine, &v2).await;
    put(&engine, &b, "after.txt", "written by 2.1.0").await;
    engine.stop(&b).await;

    // Rollback restores the schema-1 copy; 2.1.0's data is kept aside.
    let rolled = switch(&engine, update(&v2, &v1, &[NATIVE_CODE], "drain"))
        .await
        .expect("roll back to 1.0.0");
    assert_eq!(rolled["kind"], "rollback");
    assert_eq!(rolled["data"]["kind"], "restore");
    assert_eq!(rolled["data"]["schemaVersion"], 1);
    let record = layout.data_record(notes);
    assert_eq!(record["current"]["id"], g1.as_str());
    assert_eq!(record["kept"][0]["id"], g2.as_str());
    let (c, _) = started(&engine, &v1).await;
    assert_eq!(get(&engine, &c, "state.json").await["text"], original);
    assert_eq!(get(&engine, &c, "after.txt").await["code"], "notFound");
    engine.stop(&c).await;

    // Without a kept copy of its schema, older code never gets the data.
    let fresh = "core19.fixture/fresh";
    let fresh2 = Spec::new(fresh, "2.0.0")
        .data(json!({"schemaVersion": 2}))
        .write(&sources);
    let fresh1 = Spec::new(fresh, "1.0.0")
        .data(json!({"schemaVersion": 1}))
        .write(&sources);
    engine.install_enabled(&fresh2, &[NATIVE_CODE]).await;
    engine
        .call("InstallExtension", json!({"path": fresh1.dir}))
        .await
        .expect("install the older version");
    let error = switch(&engine, update(&fresh2, &fresh1, &[NATIVE_CODE], "drain"))
        .await
        .expect_err("a rollback must not read a newer schema");
    assert!(error.contains("no schema-1 copy is kept"), "{error}");
    engine
        .set_enabled(&fresh2, false, &[])
        .await
        .expect("disable 2.0.0");
    let error = engine
        .set_enabled(&fresh1, true, &[NATIVE_CODE])
        .await
        .expect_err("enable must not bypass the schema check");
    assert!(error.contains("is schema 2"), "{error}");
    assert_eq!(layout.data_record(fresh)["current"]["schemaVersion"], 2);
    let profile_id = engine
        .call("EngineInfo", json!({}))
        .await
        .expect("engine info")["profileId"]
        .as_str()
        .unwrap()
        .to_owned();
    engine.close().await;

    // Power loss after the migrated copy was written, before the grants
    // rename: a complete copy and a complete would-be commit in a temp file.
    let committed = layout.grants_hash();
    let residue = "0b5e2f0e-crash-copy";
    let copy = layout.generation(notes, residue);
    fs::create_dir_all(&copy).unwrap();
    fs::write(
        copy.join("state.json"),
        br#"{"schema":2,"count":5,"migratedBy":"2.1.0"}"#,
    )
    .unwrap();
    let mut would_be = layout.grants();
    would_be["revision"] = json!(would_be["revision"].as_u64().unwrap() + 1);
    would_be["extensions"][notes]["packageHash"] = json!(v2.hash);
    would_be["extensions"][notes]["version"] = json!("2.1.0");
    would_be["data"][notes]["current"]["id"] = json!(residue);
    would_be["data"][notes]["current"]["schemaVersion"] = json!(2);
    fs::write(
        layout.store.join(".extension-grants-crash.tmp"),
        serde_json::to_vec(&would_be).unwrap(),
    )
    .unwrap();
    // Power loss during an erase (renamed aside, not yet deleted) and during
    // a removal (index written, quarantine not yet released).
    let aside = layout.store.join("extensions/data/.erasing-crash");
    fs::create_dir_all(&aside).unwrap();
    fs::write(aside.join("old.txt"), b"erased").unwrap();
    let stale_hash = "f".repeat(64);
    let mut state = if layout.runtime_path().exists() {
        layout.runtime_state()
    } else {
        json!({"version": 1, "profileId": profile_id, "revision": 1, "quarantined": {}})
    };
    state["quarantined"][stale_hash.as_str()] = json!({
        "extensionId": "core19.fixture/gone", "version": "1.0.0",
        "reason": "residue", "faults": 3, "quarantinedAtMs": 1,
    });
    fs::write(layout.runtime_path(), serde_json::to_vec(&state).unwrap()).unwrap();

    let engine = Engine::open(&data);
    assert_eq!(
        layout.grants_hash(),
        committed,
        "crash residue became the commit"
    );
    assert!(!layout.store.join(".extension-grants-crash.tmp").exists());
    assert!(!copy.exists(), "the uncommitted copy survived recovery");
    assert!(!aside.exists(), "an interrupted erase survived recovery");
    let mut expected = vec![g1.clone(), g2.clone()];
    expected.sort();
    assert_eq!(
        layout.generations(notes),
        expected,
        "recovery removed a kept copy"
    );
    assert_eq!(tree_digest(&layout.generation(notes, &g1)), d1);
    assert!(
        layout.runtime_state()["quarantined"]
            .get(&stale_hash)
            .is_none(),
        "the quarantine of a removed package survived recovery"
    );
    let (d, _) = started(&engine, &v1).await;
    assert_eq!(get(&engine, &d, "state.json").await["text"], original);
    engine.close().await;
    evidence(
        "e16-migration-rollback-power-loss",
        &json!({
            "failures": failures,
            "migrated": outcome,
            "rolledBack": rolled,
            "dataRecord": layout.data_record(notes),
            "keptDigest": d1,
            "grants": layout.grants(),
        }),
    );
}

/// Safe mode stops every worker and refuses every start, survives a
/// restart, leaves grants and data untouched and keeps the core working;
/// switches that run no extension code, removal and turning it off still
/// work. Regressions caught: an extension runs in safe mode, safe mode
/// rewrites or loses enablement, it is lost on restart, a migration runs
/// extension code in safe mode, recovery operations are blocked, leaving
/// safe mode loses the data.
#[tokio::test(flavor = "multi_thread")]
async fn safe_mode_starts_with_extensions_off_and_still_recovers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let layout = Layout::new(&data);
    let notes = "core19.fixture/notes";
    let schema1 = json!({"schemaVersion": 1});
    let v1 = Spec::new(notes, "1.0.0")
        .data(schema1.clone())
        .write(&sources);
    let v11 = Spec::new(notes, "1.1.0")
        .data(schema1.clone())
        .write(&sources);
    let v2 = Spec::new(notes, "2.0.0")
        .data(json!({"schemaVersion": 2, "migratesFrom": 1}))
        .fixture(json!({"migrate": "ok"}))
        .write(&sources);

    let engine = Engine::open(&data);
    engine.install_enabled(&v1, &[NATIVE_CODE]).await;
    let (a, a_pid) = started(&engine, &v1).await;
    put(&engine, &a, "state.json", "safe").await;
    let grants = layout.grants_hash();

    let snapshot = engine
        .call("SetExtensionSafeMode", json!({"enabled": true}))
        .await
        .expect("turn safe mode on");
    assert_eq!(snapshot["safeMode"]["source"], "profile", "{snapshot}");
    assert!(wait_process_gone(a_pid, Duration::from_secs(3)).await);
    assert_eq!(engine.instance(&a).await["lastExit"]["reason"], "safeMode");
    let error = engine
        .start(&v1, None, &[])
        .await
        .expect_err("nothing starts in safe mode");
    assert!(error.contains("safe mode is on"), "{error}");
    assert_eq!(layout.grants_hash(), grants, "safe mode changed enablement");
    assert!(layout.runtime_state()["safeMode"]["enabledAtMs"].is_i64());
    engine.close().await;

    let engine = Engine::open(&data);
    let listed = packages(&engine).await;
    assert_eq!(
        listed["safeMode"]["source"], "profile",
        "safe mode was lost"
    );
    let instances = engine.snapshot().await;
    assert_eq!(instances["instances"], json!([]));
    assert_eq!(instances["safeMode"], true);
    let error = engine
        .start(&v1, None, &[])
        .await
        .expect_err("nothing starts after a restart in safe mode");
    assert!(error.contains("safe mode is on"), "{error}");
    // The core itself keeps working, offline.
    let checkout = root.join("checkout");
    fs::create_dir_all(&checkout).unwrap();
    engine.create_task("task-safe", &checkout);
    assert!(
        engine.core.workspace.chat("task-safe").unwrap().is_some(),
        "the core could not create a task in safe mode"
    );
    engine
        .call("EngineInfo", json!({}))
        .await
        .expect("engine info");

    // Recovery that runs no extension code works; a migration does not.
    for candidate in [&v11, &v2] {
        engine
            .call("InstallExtension", json!({"path": candidate.dir}))
            .await
            .expect("install in safe mode");
    }
    let view = package_view(&engine, &v2).await;
    assert!(
        view["switch"]["blocked"]
            .as_str()
            .is_some_and(|reason| reason.contains("safe mode")),
        "{view}"
    );
    switch(&engine, update(&v1, &v11, &[NATIVE_CODE], "drain"))
        .await
        .expect("a switch that keeps the data works in safe mode");
    let error = switch(&engine, update(&v11, &v2, &[NATIVE_CODE], "drain"))
        .await
        .expect_err("no migration runs in safe mode");
    assert!(error.contains("safe mode is on"), "{error}");
    assert_eq!(layout.data_record(notes)["current"]["schemaVersion"], 1);
    switch(&engine, update(&v11, &v1, &[NATIVE_CODE], "drain"))
        .await
        .expect("a rollback works in safe mode");
    engine
        .call(
            "RemoveExtension",
            json!({"extensionId": v11.id, "packageHash": v11.hash}),
        )
        .await
        .expect("removal works in safe mode");

    let snapshot = engine
        .call("SetExtensionSafeMode", json!({"enabled": false}))
        .await
        .expect("turn safe mode off");
    assert_eq!(snapshot["safeMode"], Value::Null, "{snapshot}");
    let grants = layout.grants();
    let grant = &grants["extensions"][notes];
    assert_eq!(grant["packageHash"], v1.hash);
    assert_eq!(grant["grants"], json!([NATIVE_CODE]));
    let (b, _) = started(&engine, &v1).await;
    assert_eq!(get(&engine, &b, "state.json").await["text"], "safe");
    engine.close().await;
    evidence(
        "e16-safe-mode",
        &json!({"listed": listed, "instances": instances, "afterSafeMode": snapshot}),
    );
}

/// A protocol-1.0 worker for the lifecycle suite, written independently of
/// the engine. Its `fixture.json` selects `luna.migrate` (`ok`, `fail`,
/// `crash`) and `luna.reconcile`; it records every call it applies in its own
/// journal, which is what `luna.reconcile` answers from.
const LIFECYCLE_WORKER: &str = r##"#!/usr/bin/env python3
"""CORE-19 lifecycle fixture worker (test bytes only, never part of LUNA)."""
import base64, json, os, queue, sys, threading, time

lock = threading.Lock()

def send(frame):
    line = json.dumps(frame, separators=(",", ":")) + "\n"
    with lock:
        sys.stdout.write(line)
        sys.stdout.flush()

config = json.load(open("fixture.json"))
first = sys.stdin.readline()
if not first:
    sys.exit(0)
hello = json.loads(first)
operations = ["fixture.echo", "fixture.apply", "fixture.slow", "fixture.put", "fixture.get", "fixture.crash"]
if config.get("migrate"):
    operations.append("luna.migrate")
if config.get("reconcile"):
    operations.append("luna.reconcile")
send({
    "type": "hello",
    "id": hello["id"],
    "protocol": {"major": 1, "minor": 0},
    "extensionId": hello["instance"]["extensionId"],
    "packageHash": hello["instance"]["packageHash"],
    "operations": operations,
})

inbox = queue.Queue()
def drain():
    for line in sys.stdin:
        inbox.put(json.loads(line))
    os._exit(0)
threading.Thread(target=drain, daemon=True).start()

backlog = []
waiting = set()
counter = [0]

def next_frame():
    return backlog.pop(0) if backlog else inbox.get()

def host_call(op, params):
    counter[0] += 1
    hid = "hc-%d" % counter[0]
    send({"type": "hostCall", "id": hid, "op": op, "params": params})
    while True:
        frame = inbox.get()
        if frame.get("type") == "hostResult" and frame.get("id") == hid:
            return frame
        backlog.append(frame)

def respond(fid, result=None, error=None):
    if error is None:
        send({"type": "response", "id": fid, "ok": True, "result": result})
    else:
        send({"type": "response", "id": fid, "ok": False, "error": error})

def data_get(path):
    answer = host_call("data.read", {"path": path})
    if not answer.get("ok"):
        return {"text": None, "code": answer["error"]["code"]}
    return {"text": base64.b64decode(answer["result"]["data"]).decode(), "code": None}

def data_put(path, text):
    data = base64.b64encode(text.encode()).decode()
    return host_call("data.write", {"path": path, "data": data, "create": True})

def applied():
    journal = config.get("journal")
    if not journal or not os.path.exists(journal):
        return set()
    return {json.loads(line)["callId"] for line in open(journal) if line.strip()}

def later(fid, ms):
    time.sleep(ms / 1000.0)
    respond(fid, {"slept": ms})

while True:
    frame = next_frame()
    kind = frame.get("type")
    if kind == "shutdown":
        sys.exit(0)
    if kind == "cancel":
        if frame["id"] in waiting:
            waiting.discard(frame["id"])
            respond(frame["id"], error={"code": "cancelled", "message": "the fixture stopped waiting"})
        continue
    if kind != "call":
        continue
    fid, method, params = frame["id"], frame["method"], frame.get("params") or {}
    if method == "fixture.echo":
        respond(fid, {"pid": os.getpid(), "version": config.get("version")})
    elif method == "fixture.apply":
        hold = params.get("hold")
        if hold == "before":
            waiting.add(fid)
            continue
        with open(config["effects"], "a") as out:
            out.write(params["line"] + "\n")
        with open(config["journal"], "a") as out:
            out.write(json.dumps({"callId": fid, "line": params["line"]}) + "\n")
        if hold == "after":
            waiting.add(fid)
            continue
        respond(fid, {"applied": True})
    elif method == "fixture.slow":
        threading.Thread(target=later, args=(fid, params["ms"]), daemon=True).start()
    elif method == "fixture.put":
        respond(fid, data_put(params["path"], params["text"]))
    elif method == "fixture.get":
        respond(fid, data_get(params["path"]))
    elif method == "fixture.crash":
        os._exit(3)
    elif method == "luna.migrate":
        mode = config.get("migrate")
        if mode == "ok":
            state = json.loads(data_get("state.json")["text"])
            state["schema"] = params["toSchemaVersion"]
            state["migratedBy"] = config["version"]
            data_put("state.json", json.dumps(state, separators=(",", ":")))
            respond(fid, {"migrated": True})
        elif mode == "fail":
            data_put("state.json", "{partial")
            respond(fid, error={"code": "migrationFailed", "message": "the fixture refused"})
        else:
            data_put("state.json", "{partial")
            os._exit(4)
    elif method == "luna.reconcile":
        done = applied()
        results = [
            {"callId": call["callId"], "outcome": "applied" if call["callId"] in done else "notApplied"}
            for call in params.get("calls", [])
        ]
        respond(fid, {"results": results})
    else:
        respond(fid, error={"code": "unknownOperation", "message": method})
"##;
