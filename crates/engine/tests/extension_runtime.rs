//! CORE-07 / E09: supervised native extension workers and the capability
//! broker, driven through the engine's public RPC methods with real package
//! bytes, real child processes and readback the engine does not produce:
//! process and group existence from the OS, file bytes in each task's
//! checkout, and the grants and runtime-state files.
//!
//! The worker is an independently written protocol-1.0 peer
//! (`support/extension_runtime_fixture.rs`). It is configured per package
//! through a listed `fixture.json`, so every case is its own package hash.
//! Native workers are user-level code: these tests prove what the
//! supervisor bounds and brokers, and that it never claims an OS sandbox.
#![cfg(unix)]

#[path = "support/extension_runtime_fixture.rs"]
mod fixture;

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fixture::{
    Engine, NATIVE_CODE, TASKS_READ, WORKSPACE_READ, WORKSPACE_WRITE, evidence, group_exists,
    native_package, pid_of, process_exists, sha256, tree_digest, wait_process_gone, write_package,
};
use serde_json::{Value, json};

fn code(answer: &Value) -> &str {
    if answer["ok"] == true {
        "ok"
    } else {
        answer["error"]["code"].as_str().unwrap_or("<none>")
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_owned())
        .collect()
}

/// Every handshake failure is refused, recorded and leaves no process of
/// the worker's group; every preflight refusal starts nothing at all. A
/// conforming worker starts with the server's principal, not the one its
/// hello claims, and is described honestly. Regressions caught: a wrong
/// major, a wrong correlation id, a hello for another package, a missing
/// required operation, invalid or oversized JSON, an early exit or silence
/// activates a worker or leaves it running; a data, disabled or tampered
/// package spawns a process; unknown hello fields widen grants; the view
/// claims an OS sandbox.
#[tokio::test(flavor = "multi_thread")]
async fn handshake_and_preflight_refusals_leave_nothing_running() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let engine = Engine::open(&data);

    let ok = native_package(&sources, "hello-ok", json!({}), None, json!([]));
    engine.install_enabled(&ok, &[NATIVE_CODE]).await;
    let started = engine.start(&ok, None, &[]).await.expect("start");
    let pid = pid_of(&started);
    assert_eq!(started["state"], "running");
    assert!(process_exists(pid), "the started worker is not running");
    // The worker said 1.3 and claimed workspace.write in its hello.
    assert_eq!(started["protocol"], json!({"major": 1, "minor": 0}));
    assert_eq!(started["grants"], json!([NATIVE_CODE]));
    assert_eq!(started["trust"], "native");
    assert_eq!(started["isolation"]["osSandbox"], false);
    let not_enforced = strings(&started["isolation"]["notEnforced"]);
    for gap in ["filesystem", "network", "processSpawn", "loopbackOwnerIpc"] {
        assert!(
            not_enforced.iter().any(|item| item == gap),
            "the native view does not admit {gap}: {not_enforced:?}"
        );
    }
    let echo = engine
        .ok(
            started["instanceId"].as_str().unwrap(),
            "fixture.echo",
            json!({"n": 1}),
        )
        .await;
    assert_eq!(echo["pid"], pid);

    let mut refusals = Vec::new();
    let cases: [(&str, Value, &[&str], &str); 8] = [
        (
            "major2",
            json!({"hello": "major2"}),
            &[],
            "speaks protocol 2.3",
        ),
        (
            "wrong-id",
            json!({"hello": "wrongId"}),
            &[],
            "answers a different request id",
        ),
        (
            "wrong-hash",
            json!({"hello": "wrongHash"}),
            &[],
            "answered for a different package",
        ),
        (
            "silent",
            json!({"hello": "silent"}),
            &[],
            "no hello within 1000 ms",
        ),
        (
            "exit",
            json!({"hello": "exit"}),
            &[],
            "exited before its hello",
        ),
        (
            "json",
            json!({"hello": "json"}),
            &[],
            "the hello reply is invalid",
        ),
        (
            "oversize",
            json!({"hello": "oversize"}),
            &[],
            "the hello is larger than 64 KiB",
        ),
        (
            "missing-op",
            json!({"operations": ["fixture.echo"]}),
            &["fixture.provider"],
            "required operation fixture.provider",
        ),
    ];
    for (name, config, require, expected) in cases {
        let package = native_package(&sources, name, config, None, json!([]));
        engine.install_enabled(&package, &[NATIVE_CODE]).await;
        let began = Instant::now();
        let error = engine.start(&package, None, require).await.expect_err(name);
        assert!(error.contains(expected), "{name}: {error}");
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{name}: the refusal took {:?}",
            began.elapsed()
        );
        let views = engine.instances_of(&package).await;
        assert_eq!(views.len(), 1, "{name}: {views:?}");
        let view = &views[0];
        assert_eq!(view["state"], "failed", "{name}: {view}");
        assert_eq!(view["lastExit"]["reason"], "handshakeFailed", "{name}");
        assert_eq!(view["lastExit"]["groupCleared"], true, "{name}");
        let failed_pid = view["lastExit"]["pid"].as_u64().unwrap();
        assert!(
            !group_exists(failed_pid),
            "{name}: its process group survived"
        );
        refusals.push(json!({"case": name, "error": error, "exit": view["lastExit"]}));
    }

    // Refused before any process exists.
    let notes: &[u8] = b"notes\n";
    let data_package = fixture::Package {
        dir: sources.join("data-only"),
        id: "core07.fixture/data-only".into(),
        hash: write_package(
            &sources.join("data-only"),
            fixture::base_manifest(
                "core07.fixture/data-only",
                json!({"kind": "data"}),
                json!([]),
            ),
            &[("notes.txt", notes)],
        ),
    };
    engine.install_enabled(&data_package, &[]).await;
    let disabled = native_package(&sources, "not-enabled", json!({}), None, json!([]));
    engine
        .call("InstallExtension", json!({"path": disabled.dir}))
        .await
        .expect("install");
    let tampered = native_package(&sources, "tampered", json!({}), None, json!([]));
    engine.install_enabled(&tampered, &[NATIVE_CODE]).await;
    let installed_worker = data
        .join("extensions/packages")
        .join(&tampered.hash)
        .join("bin/worker");
    fs::set_permissions(&installed_worker, fs::Permissions::from_mode(0o755)).unwrap();
    let mut changed = fs::read(&installed_worker).unwrap();
    let at = changed.len() - 2;
    changed[at] = b'#';
    fs::write(&installed_worker, &changed).unwrap();
    for (package, expected) in [
        (&data_package, "data package"),
        (&disabled, "not enabled in this profile"),
        (&tampered, "failed verification"),
    ] {
        let error = engine
            .start(package, None, &[])
            .await
            .expect_err(&package.id);
        assert!(error.contains(expected), "{}: {error}", package.id);
        assert!(
            engine.instances_of(package).await.is_empty(),
            "{} created an instance",
            package.id
        );
        refusals.push(json!({"case": package.id, "error": error}));
    }

    let instance = started["instanceId"].as_str().unwrap().to_owned();
    let stopped = engine.stop(&instance).await;
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(stopped["lastExit"]["reason"], "stopped");
    assert_eq!(stopped["lastExit"]["groupCleared"], true);
    assert!(wait_process_gone(pid, Duration::from_secs(2)).await);
    engine.close().await;
    evidence(
        "e09-handshake",
        &json!({"started": started, "refusals": refusals, "stopped": stopped}),
    );
}

/// The broker acts only for the server-bound principal and target: reads
/// and writes stay inside the bound task's checkout, ungranted operations
/// and owner RPC methods are refused, a worker cannot name another task or
/// instance, and nothing of LUNA's environment or descriptors reaches the
/// worker. Disabling a package withdraws a running instance at once.
/// Regressions caught: `..`, absolute paths or a symlink reach another
/// task; a `taskId` parameter or frame-level `instanceId`/`target` field is
/// honoured; an ungranted capability is used; `SetExtensionEnabled` or the
/// local owner claim is reachable from a worker; `CARGO_*`/`LUNA_*`
/// variables or an inheritable descriptor leak into the worker; the worker
/// shares LUNA's session; a disabled package keeps running.
#[tokio::test(flavor = "multi_thread")]
async fn broker_confines_workers_to_their_grants_and_bound_task() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let task_a = root.join("task-a");
    let task_b = root.join("task-b");
    fs::create_dir_all(task_a.join("nested")).unwrap();
    fs::create_dir_all(&task_b).unwrap();
    fs::write(task_a.join("notes.txt"), b"task A notes\n").unwrap();
    fs::write(task_a.join("nested/deep.txt"), b"deep\n").unwrap();
    fs::write(task_b.join("secret.txt"), b"task B secret\n").unwrap();
    std::os::unix::fs::symlink("../task-b", task_a.join("link")).unwrap();
    let b_before = tree_digest(&task_b);
    let a_before = tree_digest(&task_a);

    // An inheritable descriptor in the engine's process (no O_CLOEXEC).
    let leak = root.join("leak.txt");
    fs::write(&leak, b"leak\n").unwrap();
    let leak_path = CString::new(leak.as_os_str().as_bytes()).unwrap();
    let leaked_fd = unsafe { libc::open(leak_path.as_ptr(), libc::O_RDONLY) };
    assert!(leaked_fd > 2);
    assert!(
        std::env::var_os("CARGO_MANIFEST_DIR").is_some(),
        "cargo runs tests with CARGO_* set, the control for the environment check"
    );

    let engine = Engine::open(&data);
    engine.create_task("task-a", &task_a);
    engine.create_task("task-b", &task_b);
    let reader = native_package(
        &sources,
        "reader",
        json!({}),
        None,
        json!([
            {"id": WORKSPACE_READ},
            {"id": WORKSPACE_WRITE, "required": false},
            {"id": TASKS_READ, "required": false},
        ]),
    );
    let writer = native_package(
        &sources,
        "writer",
        json!({}),
        None,
        json!([{"id": WORKSPACE_READ}, {"id": WORKSPACE_WRITE}]),
    );
    engine
        .install_enabled(&reader, &[NATIVE_CODE, WORKSPACE_READ])
        .await;
    engine
        .install_enabled(&writer, &[NATIVE_CODE, WORKSPACE_READ, WORKSPACE_WRITE])
        .await;
    let grants_file = data.join("orgs/org-a/user-a/extension-grants.json");
    let grants_before = sha256(&fs::read(&grants_file).unwrap());
    let reader_view = engine
        .start(&reader, Some("task-a"), &[])
        .await
        .expect("start reader");
    let writer_view = engine
        .start(&writer, Some("task-a"), &[])
        .await
        .expect("start writer");
    let reader_id = reader_view["instanceId"].as_str().unwrap().to_owned();
    let writer_id = writer_view["instanceId"].as_str().unwrap().to_owned();
    let reader_pid = pid_of(&reader_view);

    let env = engine.ok(&reader_id, "fixture.env", json!({})).await;
    let keys = strings(&env["env"]);
    assert!(
        !keys
            .iter()
            .any(|key| key.starts_with("CARGO_") || key.starts_with("LUNA_")),
        "LUNA's or cargo's environment reached the worker: {keys:?}"
    );
    assert!(keys.iter().any(|key| key == "PATH"), "{keys:?}");
    let fds = env["fds"].as_array().unwrap();
    assert!(
        !fds.contains(&json!(leaked_fd)),
        "descriptor {leaked_fd} leaked into the worker: {fds:?}"
    );
    assert_eq!(
        env["cwd"],
        json!(data.join("extensions/packages").join(&reader.hash))
    );
    assert_eq!(env["pgid"], env["pid"], "the worker is not its own group");
    assert_eq!(env["sid"], env["pid"], "the worker shares a session");
    assert_ne!(env["sid"], json!(unsafe { libc::getsid(0) }));

    let info = engine
        .ok(
            &reader_id,
            "fixture.broker",
            json!({"calls": [{"op": "host.info"}]}),
        )
        .await;
    assert_eq!(info[0]["ok"], true, "{info}");
    assert_eq!(info[0]["result"]["instanceId"], reader_id);
    assert_eq!(
        info[0]["result"]["grants"],
        json!([NATIVE_CODE, WORKSPACE_READ])
    );
    assert_eq!(
        info[0]["result"]["target"],
        json!({"kind": "task", "taskId": "task-a"})
    );

    let secret_path = task_b.join("secret.txt").to_string_lossy().into_owned();
    let battery = json!([
        {"op": "workspace.read", "params": {"path": "notes.txt"}},
        {"op": "workspace.read", "params": {"path": "nested/deep.txt"}},
        {"op": "workspace.read", "params": {"path": "../task-b/secret.txt"}},
        {"op": "workspace.read", "params": {"path": secret_path}},
        {"op": "workspace.read", "params": {"path": "link/secret.txt"}},
        {"op": "workspace.read", "params": {"path": "notes.txt", "taskId": "task-b"}},
        {
            "op": "workspace.read",
            "params": {"path": "notes.txt"},
            "frame": {"instanceId": writer_id, "target": {"kind": "task", "taskId": "task-b"}},
        },
        {"op": "workspace.write", "params": {"path": "out.txt", "data": BASE64.encode(b"x"), "create": true}},
        {"op": "tasks.list"},
        {
            "op": "SetExtensionEnabled",
            "params": {
                "extensionId": reader.id,
                "packageHash": reader.hash,
                "enabled": true,
                "grants": [NATIVE_CODE, WORKSPACE_READ, WORKSPACE_WRITE, TASKS_READ],
            },
        },
        {"op": "ListExtensions"},
        {"op": "LocalOwnerRootClaim"},
    ]);
    let expected = [
        "ok",
        "ok",
        "outsideTarget",
        "outsideTarget",
        "outsideTarget",
        "badRequest",
        "badRequest",
        "denied",
        "denied",
        "unknownOperation",
        "unknownOperation",
        "unknownOperation",
    ];
    let answers = engine
        .ok(&reader_id, "fixture.broker", json!({"calls": battery}))
        .await;
    for (index, expected) in expected.iter().enumerate() {
        assert_eq!(
            code(&answers[index]),
            *expected,
            "call {index}: {}",
            answers[index]
        );
    }
    let notes = BASE64
        .decode(answers[0]["result"]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(notes, b"task A notes\n");
    assert!(
        !answers.to_string().contains("task B secret"),
        "a refusal carried the other task's bytes"
    );
    let reader_after = engine.instance(&reader_id).await;
    assert_eq!(reader_after["usage"]["hostCallsDenied"], 10);
    assert_eq!(reader_after["usage"]["faults"], 0);
    assert_eq!(pid_of(&reader_after), reader_pid, "a refusal restarted it");

    let payload: &[u8] = b"written by the writer\n";
    let writes = json!([
        {"op": "workspace.write", "params": {"path": "out.txt", "data": BASE64.encode(payload), "create": true}},
        {"op": "workspace.write", "params": {"path": "link/evil.txt", "data": BASE64.encode(b"evil"), "create": true}},
        {"op": "workspace.write", "params": {"path": "../task-b/evil.txt", "data": BASE64.encode(b"evil"), "create": true}},
        {"op": "workspace.write", "params": {"path": "missing.txt", "data": BASE64.encode(b"x")}},
        {"op": "workspace.read", "params": {"path": "out.txt"}},
    ]);
    let answers = engine
        .ok(&writer_id, "fixture.broker", json!({"calls": writes}))
        .await;
    for (index, expected) in ["ok", "outsideTarget", "outsideTarget", "notFound", "ok"]
        .iter()
        .enumerate()
    {
        assert_eq!(
            code(&answers[index]),
            *expected,
            "write {index}: {}",
            answers[index]
        );
    }
    assert_eq!(fs::read(task_a.join("out.txt")).unwrap(), payload);

    // Independent readback: the other task is byte-for-byte unchanged, this
    // task gained exactly the granted write, and no worker changed a grant.
    assert_eq!(tree_digest(&task_b), b_before, "task B's checkout changed");
    let mut a_expected = a_before.clone();
    a_expected.insert("out.txt".into(), sha256(payload));
    assert_eq!(tree_digest(&task_a), a_expected);
    assert_eq!(sha256(&fs::read(&grants_file).unwrap()), grants_before);

    // Disabling the writer withdraws its running instance.
    let writer_pid = pid_of(&engine.instance(&writer_id).await);
    engine
        .set_enabled(&writer, false, &[])
        .await
        .expect("disable writer");
    let revoked = engine
        .wait_for(&writer_id, Duration::from_secs(5), "revocation", |view| {
            view["state"] == "revoked" && view["pid"].is_null()
        })
        .await;
    assert_eq!(revoked["lastExit"]["reason"], "revoked");
    assert!(wait_process_gone(writer_pid, Duration::from_secs(2)).await);
    let error = engine
        .invoke(&writer_id, "fixture.echo", json!({}), 1000)
        .await
        .expect_err("a revoked instance must not restart");
    assert!(error.contains("disabled or its grants changed"), "{error}");
    let reader_final = engine.instance(&reader_id).await;
    assert_eq!(pid_of(&reader_final), reader_pid);
    assert_eq!(reader_final["generation"], 1);

    engine.close().await;
    assert!(
        wait_process_gone(reader_pid, Duration::from_secs(2)).await,
        "a worker outlived engine shutdown"
    );
    unsafe {
        libc::close(leaked_fd);
    }
    evidence(
        "e09-broker",
        &json!({
            "environment": env,
            "hostInfo": info,
            "reader": reader_final,
            "revokedWriter": revoked,
            "taskB": b_before,
        }),
    );
}

/// A hung call returns at its deadline and the unacknowledging worker is
/// stopped; floods of logs, stderr, host calls and frames stay bounded; the
/// memory, CPU and process limits stop only the worker that crossed them;
/// another instance, another task's files and the engine keep working
/// throughout. Regressions caught: a caller waits past its deadline, a
/// cancelled call keeps its worker alive forever, an acknowledged cancel
/// kills the worker, a ninth concurrent call is queued without bound, logs
/// or stderr grow without bound, host calls are not rate limited, a frame
/// flood starves the engine, a group grows past its memory or process cap
/// or spins past its CPU limit, or one fault disturbs unrelated work.
#[tokio::test(flavor = "multi_thread")]
async fn hangs_floods_and_resource_limits_stop_only_the_faulty_worker() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let task_b = root.join("task-b");
    fs::create_dir_all(&task_b).unwrap();
    fs::write(task_b.join("work.txt"), b"unrelated work\n").unwrap();
    let b_before = tree_digest(&task_b);

    let engine = Engine::open(&data);
    engine.create_task("task-b", &task_b);
    let task_row = |engine: &Engine| {
        let chat = engine.core.workspace.chat("task-b").unwrap().unwrap();
        json!({"id": chat.id, "device": chat.device_id, "cwd": chat.cwd, "title": chat.title, "archived": chat.archived})
    };
    let chat_before = task_row(&engine);
    let steady = native_package(&sources, "steady", json!({}), None, json!([]));
    let noisy = native_package(&sources, "noisy", json!({}), None, json!([]));
    let memory = native_package(
        &sources,
        "memory",
        json!({}),
        Some(json!({"memoryMiB": 64})),
        json!([]),
    );
    let cpu = native_package(
        &sources,
        "cpu",
        json!({}),
        Some(json!({"cpuSeconds": 1})),
        json!([]),
    );
    let forks = native_package(&sources, "forks", json!({}), None, json!([]));
    let mut ids = Vec::new();
    for package in [&steady, &noisy, &memory, &cpu, &forks] {
        engine.install_enabled(package, &[NATIVE_CODE]).await;
        let view = engine.start(package, None, &[]).await.expect("start");
        ids.push(view["instanceId"].as_str().unwrap().to_owned());
    }
    let [steady_id, noisy_id, memory_id, cpu_id, forks_id] = ids.try_into().unwrap();
    let steady_pid = pid_of(&engine.instance(&steady_id).await);
    let noisy_pid = pid_of(&engine.instance(&noisy_id).await);

    // A cooperative worker acknowledges a cancelled call and keeps running.
    let waited = engine
        .invoke(&noisy_id, "fixture.wait", json!({}), 300)
        .await
        .unwrap();
    assert_eq!(waited["status"], "deadlineExceeded", "{waited}");
    assert!(waited["elapsedMs"].as_u64().unwrap() < 1000, "{waited}");
    let echo = engine.ok(&noisy_id, "fixture.echo", json!({})).await;
    assert!(
        echo["lastCancelled"].is_string(),
        "no cancel arrived: {echo}"
    );
    // Past the cancel grace period: an acknowledged cancel is not a hang.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let view = engine.instance(&noisy_id).await;
    assert_eq!(pid_of(&view), noisy_pid);
    assert_eq!(view["usage"]["faults"], 0);

    // At most eight calls per instance are in flight; the ninth is refused.
    let outcomes = futures::future::join_all(
        (0..9).map(|_| engine.invoke(&noisy_id, "fixture.wait", json!({}), 500)),
    )
    .await;
    let statuses: Vec<String> = outcomes
        .iter()
        .map(|outcome| {
            outcome.as_ref().unwrap()["status"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        statuses.iter().filter(|s| *s == "busy").count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| *s == "deadlineExceeded").count(),
        8,
        "{statuses:?}"
    );

    // A burst of log frames: the window keeps the newest lines.
    engine
        .ok(&noisy_id, "fixture.logs", json!({"count": 1000}))
        .await;
    let view = engine.instance(&noisy_id).await;
    let logs = view["logs"].as_array().unwrap();
    assert!(logs.len() <= 256, "{} lines retained", logs.len());
    assert!(view["usage"]["logLines"].as_u64().unwrap() >= 1000);
    assert!(view["usage"]["logLinesEvicted"].as_u64().unwrap() >= 1000 - 256);

    // 4 MiB of stderr is drained, counted and not retained.
    engine
        .ok(&noisy_id, "fixture.stderr", json!({"bytes": 4 << 20}))
        .await;
    let view = engine
        .wait_for(&noisy_id, Duration::from_secs(10), "stderr drain", |view| {
            view["usage"]["stderrBytes"].as_u64().unwrap() >= 4 << 20
        })
        .await;
    let retained: usize = view["logs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line["message"].as_str().unwrap().len())
        .sum();
    assert!(retained <= 64 * 1024, "{retained} bytes of logs retained");

    // A burst of host calls past the broker's budget is answered `busy`.
    let codes = engine
        .ok(&noisy_id, "fixture.hostflood", json!({"count": 300}))
        .await;
    let accepted = codes["ok"].as_u64().unwrap_or(0);
    let busy = codes["busy"].as_u64().unwrap_or(0);
    assert_eq!(accepted + busy, 300, "{codes}");
    assert!(accepted >= 1 && busy >= 1, "{codes}");
    let view = engine.instance(&noisy_id).await;
    assert!(view["usage"]["hostCallsBusy"].as_u64().unwrap() >= busy);
    assert_eq!(
        pid_of(&view),
        noisy_pid,
        "a bounded flood stopped the worker"
    );

    // An unbounded frame flood stops that worker; the engine and another
    // instance keep answering meanwhile.
    let probe = async {
        let mut worst = Duration::ZERO;
        for _ in 0..10 {
            let started = Instant::now();
            engine
                .call("EngineInfo", json!({}))
                .await
                .expect("engine info");
            let echo = engine.ok(&steady_id, "fixture.echo", json!({})).await;
            assert_eq!(echo["pid"], steady_pid);
            worst = worst.max(started.elapsed());
        }
        worst
    };
    let (flood, worst) = tokio::join!(
        engine.invoke(&noisy_id, "fixture.flood", json!({}), 20_000),
        probe
    );
    let flood = flood.unwrap();
    assert_eq!(flood["status"], "workerExited", "{flood}");
    assert_eq!(flood["exit"]["reason"], "flood");
    assert_eq!(flood["exit"]["groupCleared"], true);
    assert!(
        worst < Duration::from_secs(2),
        "the engine stalled for {worst:?}"
    );
    assert!(!group_exists(noisy_pid));

    // A worker that ignores a cancelled call is stopped after the grace.
    let hung = engine
        .invoke(&noisy_id, "fixture.hang", json!({}), 300)
        .await
        .unwrap();
    assert_eq!(hung["status"], "deadlineExceeded", "{hung}");
    assert_eq!(
        hung["generation"], 2,
        "the flooded worker was not restarted"
    );
    assert!(hung["elapsedMs"].as_u64().unwrap() < 1000);
    let hung_pid = pid_of(&engine.instance(&noisy_id).await);
    assert_ne!(hung_pid, noisy_pid);
    let hung_view = engine
        .wait_for(&noisy_id, Duration::from_secs(5), "hang stop", |view| {
            view["lastExit"]["generation"] == 2 && view["lastExit"]["reason"] == "hung"
        })
        .await;
    assert_eq!(hung_view["state"], "crashed");
    assert_eq!(hung_view["usage"]["faults"], 2);
    assert!(wait_process_gone(hung_pid, Duration::from_secs(2)).await);

    // Resident memory past the package's 64 MiB budget.
    let memory_pid = pid_of(&engine.instance(&memory_id).await);
    let out = engine
        .invoke(&memory_id, "fixture.alloc", json!({"mib": 256}), 20_000)
        .await
        .unwrap();
    assert_eq!(out["status"], "workerExited", "{out}");
    assert_eq!(out["exit"]["reason"], "memoryLimit", "{out}");
    assert_eq!(out["exit"]["groupCleared"], true);
    let view = engine.instance(&memory_id).await;
    assert_eq!(view["limits"]["memoryBytes"], 64 << 20);
    assert!(view["usage"]["peakResidentBytes"].as_u64().unwrap() > 64 << 20);
    assert!(!group_exists(memory_pid));

    // A spin past the package's one CPU second.
    let cpu_pid = pid_of(&engine.instance(&cpu_id).await);
    let out = engine
        .invoke(&cpu_id, "fixture.spin", json!({}), 20_000)
        .await
        .unwrap();
    assert_eq!(out["status"], "workerExited", "{out}");
    assert_eq!(out["exit"]["reason"], "cpuLimit", "{out}");
    assert_eq!(out["exit"]["signal"], libc::SIGXCPU);
    assert!(!group_exists(cpu_pid));

    // More processes than the group may hold: every one of them is gone.
    let forks_pid = pid_of(&engine.instance(&forks_id).await);
    let out = engine
        .invoke(&forks_id, "fixture.fork", json!({"count": 80}), 20_000)
        .await
        .unwrap();
    assert_eq!(out["status"], "workerExited", "{out}");
    assert_eq!(out["exit"]["reason"], "processLimit", "{out}");
    assert_eq!(out["exit"]["groupCleared"], true);
    assert!(!group_exists(forks_pid), "a forked child survived");

    // Unrelated work is untouched.
    let steady_view = engine.instance(&steady_id).await;
    assert_eq!(pid_of(&steady_view), steady_pid);
    assert_eq!(steady_view["generation"], 1);
    assert_eq!(steady_view["usage"]["faults"], 0);
    assert_eq!(
        engine.ok(&steady_id, "fixture.echo", json!({})).await["pid"],
        steady_pid
    );
    assert_eq!(tree_digest(&task_b), b_before);
    assert_eq!(task_row(&engine), chat_before);
    let snapshot = engine.snapshot().await;
    assert_eq!(
        snapshot["quarantined"],
        json!([]),
        "two faults quarantined a package"
    );
    engine.close().await;
    assert!(wait_process_gone(steady_pid, Duration::from_secs(2)).await);
    evidence(
        "e09-bounds",
        &json!({
            "statuses": statuses,
            "hostFlood": codes,
            "flood": flood,
            "worstProbe": worst.as_millis() as u64,
            "hung": hung_view,
            "steady": steady_view,
            "snapshot": snapshot,
        }),
    );
}

/// A crash mid-call fails that call, is never replayed and leaves other
/// work running; the next call restarts the worker after re-verification;
/// the third fault quarantines the exact package hash, durably across an
/// engine restart, until the owner releases it; nothing starts on reopen;
/// graceful shutdown leaves no worker. Regressions caught: a crash takes the
/// engine or another instance down, a call is retried after a crash, a
/// crash loop restarts forever, quarantine is lost on restart, an instance
/// is resurrected on reopen, release does not work, or a package tampered
/// with after enable restarts.
#[tokio::test(flavor = "multi_thread")]
async fn crashes_are_contained_never_replayed_and_quarantined_across_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let marker = root.join("received.txt");
    let steady = native_package(&sources, "steady", json!({}), None, json!([]));
    let crashy = native_package(&sources, "crashy", json!({}), None, json!([]));

    let engine = Engine::open(&data);
    engine.install_enabled(&steady, &[NATIVE_CODE]).await;
    engine.install_enabled(&crashy, &[NATIVE_CODE]).await;
    let steady_view = engine.start(&steady, None, &[]).await.expect("start");
    let steady_id = steady_view["instanceId"].as_str().unwrap().to_owned();
    let steady_pid = pid_of(&steady_view);
    let crashy_view = engine.start(&crashy, None, &[]).await.expect("start");
    let crashy_id = crashy_view["instanceId"].as_str().unwrap().to_owned();
    let mut crashy_pid = pid_of(&crashy_view);

    let mut crashes = Vec::new();
    for round in 1..=3_u64 {
        let crash = engine
            .invoke(
                &crashy_id,
                "fixture.record",
                json!({"path": marker, "line": format!("call {round}"), "crash": true}),
                10_000,
            )
            .await
            .unwrap();
        assert_eq!(crash["status"], "workerExited", "{crash}");
        assert_eq!(crash["generation"], round);
        assert_eq!(crash["exit"]["reason"], "crashed");
        assert_eq!(crash["exit"]["code"], 3);
        assert_eq!(crash["exit"]["groupCleared"], true);
        assert!(!group_exists(crashy_pid));
        crashes.push(crash);
        let echo = engine.ok(&steady_id, "fixture.echo", json!({})).await;
        assert_eq!(
            echo["pid"], steady_pid,
            "a crash disturbed another instance"
        );
        if round < 3 {
            let echo = engine.ok(&crashy_id, "fixture.echo", json!({})).await;
            let view = engine.instance(&crashy_id).await;
            assert_eq!(view["generation"], round + 1);
            assert_eq!(view["usage"]["restarts"], round);
            assert_eq!(echo["pid"], view["pid"]);
            assert_ne!(pid_of(&view), crashy_pid, "the crashed process answered");
            crashy_pid = pid_of(&view);
        }
    }
    let view = engine.instance(&crashy_id).await;
    assert_eq!(view["state"], "quarantined", "{view}");
    assert_eq!(view["usage"]["faults"], 3);
    let error = engine
        .invoke(&crashy_id, "fixture.echo", json!({}), 1000)
        .await
        .expect_err("a quarantined instance must not restart");
    assert!(error.contains("quarantined"), "{error}");
    let error = engine
        .start(&crashy, None, &[])
        .await
        .expect_err("a quarantined package must not start");
    assert!(error.contains("quarantined"), "{error}");
    let snapshot = engine.snapshot().await;
    assert_eq!(snapshot["quarantined"][0]["packageHash"], crashy.hash);
    assert_eq!(snapshot["quarantined"][0]["faults"], 3);
    let state_file = data.join("orgs/org-a/user-a/extension-runtime.json");
    let state: Value = serde_json::from_slice(&fs::read(&state_file).unwrap()).unwrap();
    assert_eq!(
        state["quarantined"][crashy.hash.as_str()]["extensionId"],
        crashy.id
    );
    let received = fs::read_to_string(&marker).unwrap();
    assert_eq!(received, "call 1\ncall 2\ncall 3\n", "a call was replayed");
    engine.close().await;
    assert!(
        wait_process_gone(steady_pid, Duration::from_secs(2)).await,
        "a worker outlived engine shutdown"
    );

    // Reopen: nothing runs, the quarantine holds, nothing is replayed.
    let engine = Engine::open(&data);
    let reopened = engine.snapshot().await;
    assert_eq!(
        reopened["instances"],
        json!([]),
        "an instance was resurrected"
    );
    assert_eq!(reopened["quarantined"][0]["packageHash"], crashy.hash);
    let error = engine
        .start(&crashy, None, &[])
        .await
        .expect_err("still quarantined after restart");
    assert!(error.contains("quarantined"), "{error}");
    assert_eq!(fs::read_to_string(&marker).unwrap(), received);

    // The owner releases it; the exact package runs again.
    let released = engine
        .call(
            "ReleaseExtensionQuarantine",
            json!({"extensionId": crashy.id, "packageHash": crashy.hash}),
        )
        .await
        .expect("release");
    assert_eq!(released["quarantined"], json!([]));
    let restarted = engine
        .start(&crashy, None, &[])
        .await
        .expect("start after release");
    let restarted_id = restarted["instanceId"].as_str().unwrap().to_owned();
    let restarted_pid = pid_of(&restarted);
    assert_eq!(
        engine.ok(&restarted_id, "fixture.echo", json!({})).await["pid"],
        restarted_pid
    );
    engine.stop(&restarted_id).await;
    assert!(wait_process_gone(restarted_pid, Duration::from_secs(2)).await);

    // Changed bytes after enable: re-verification refuses before spawning.
    let installed_worker = data
        .join("extensions/packages")
        .join(&crashy.hash)
        .join("bin/worker");
    fs::set_permissions(&installed_worker, fs::Permissions::from_mode(0o755)).unwrap();
    let mut changed = fs::read(&installed_worker).unwrap();
    let at = changed.len() - 2;
    changed[at] = b'#';
    fs::write(&installed_worker, &changed).unwrap();
    let before = engine.instances_of(&crashy).await.len();
    let error = engine
        .start(&crashy, None, &[])
        .await
        .expect_err("a tampered package must not start");
    assert!(error.contains("failed verification"), "{error}");
    assert_eq!(engine.instances_of(&crashy).await.len(), before);
    engine.close().await;
    evidence(
        "e09-crash-quarantine",
        &json!({
            "crashes": crashes,
            "quarantinedView": view,
            "stateFile": state,
            "received": received,
            "reopened": reopened,
            "released": released,
        }),
    );
}
