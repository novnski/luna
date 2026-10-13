//! CORE-07 / E09, portable profile: Wasm packages run in LUNA's separate
//! portable host process (`luna-extension-host`, Wasmtime 49.0.2, core
//! modules, no WASI) under the same supervisor and broker as native
//! workers. Driven through the engine's public RPC methods with real module
//! bytes assembled here from independently written WAT, a real host process
//! and OS readback.
//!
//! Build the host first and point the engine at it:
//! `LUNA_EXTENSION_HOST=$PWD/target/debug/luna-extension-host`.
#![cfg(unix)]

#[path = "support/extension_runtime_fixture.rs"]
mod fixture;

use std::fs;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use fixture::{
    Engine, Package, WORKSPACE_READ, WORKSPACE_WRITE, base_manifest, evidence, group_exists,
    pid_of, sha256, tree_digest, wait_process_gone, write_package,
};
use serde_json::{Value, json};

/// Memory layout: answers and requests at fixed addresses below 4096, the
/// host-written parameters at 4096, broker answers from 8192.
const GUEST: &str = r##"
(module
  (import "luna_broker_v1" "call" (func $call (param i32 i32 i32 i32) (result i32)))
  (import "luna_broker_v1" "log" (func $log (param i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{\"result\":\"steady\"}")
  (data (i32.const 64) "{\"result\":\"denied\"}")
  (data (i32.const 128) "{\"result\":\"grown\"}")
  (data (i32.const 192) "{\"result\":")
  (data (i32.const 256) "{\"error\":{\"code\":\"brokerCall\",\"message\":\"the broker call failed\"}}")
  (data (i32.const 384) "{\"op\":\"host.info\",\"params\":{}}")
  (data (i32.const 448) "guest log line")
  (func $pack (param $ptr i32) (param $len i32) (result i64)
    (i64.or
      (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
      (i64.extend_i32_u (local.get $len))))
  (func $delay
    (local $i i32)
    (local.set $i (i32.const 100000))
    (loop $spin
      (local.set $i (i32.sub (local.get $i) (i32.const 1)))
      (br_if $spin (i32.ne (local.get $i) (i32.const 0)))))
  (func (export "luna_alloc") (param $len i32) (result i32)
    (if (result i32) (i32.gt_u (local.get $len) (i32.const 4096))
      (then (i32.const 0))
      (else (i32.const 4096))))
  (func (export "luna.op/echo") (param i32 i32) (result i64)
    (call $pack (i32.const 0) (i32.const 19)))
  (func (export "luna.op/spin") (param i32 i32) (result i64)
    (loop $forever (br $forever))
    (call $pack (i32.const 0) (i32.const 19)))
  (func (export "luna.op/grow") (param i32 i32) (result i64)
    (if (result i64) (i32.eq (memory.grow (i32.const 512)) (i32.const -1))
      (then (call $pack (i32.const 64) (i32.const 19)))
      (else (call $pack (i32.const 128) (i32.const 18)))))
  (func (export "luna.op/trap") (param i32 i32) (result i64)
    unreachable)
  (func (export "luna.op/host") (param $ptr i32) (param $len i32) (result i64)
    (local $n i32)
    (local.set $n
      (call $call (local.get $ptr) (local.get $len) (i32.const 8202) (i32.const 50000)))
    (if (i32.lt_s (local.get $n) (i32.const 0))
      (then (return (call $pack (i32.const 256) (i32.const 66)))))
    (memory.copy (i32.const 8192) (i32.const 192) (i32.const 10))
    (i32.store8 (i32.add (i32.const 8202) (local.get $n)) (i32.const 125))
    (call $pack (i32.const 8192) (i32.add (local.get $n) (i32.const 11))))
  (func (export "luna.op/hostloop") (param i32 i32) (result i64)
    (loop $again
      (call $delay)
      (drop (call $call (i32.const 384) (i32.const 30) (i32.const 8192) (i32.const 50000)))
      (br $again))
    (call $pack (i32.const 0) (i32.const 19)))
  (func (export "luna.op/logs") (param i32 i32) (result i64)
    (local $i i32)
    (loop $more
      (call $log (i32.const 1) (i32.const 448) (i32.const 14))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $more (i32.lt_u (local.get $i) (i32.const 5000))))
    (call $pack (i32.const 0) (i32.const 19)))
)
"##;

/// Asks for WASI, which the portable profile never provides.
const AMBIENT: &str = r##"
(module
  (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1))
"##;

fn wasm_package(sources: &Path, name: &str, module: &[u8], capabilities: Value) -> Package {
    let id = format!("core07.fixture/{name}");
    let dir = sources.join(name);
    let hash = write_package(
        &dir,
        base_manifest(
            &id,
            json!({"kind": "wasm", "protocol": {"major": 1, "minMinor": 0}, "module": "guest.wasm"}),
            capabilities,
        ),
        &[("guest.wasm", module)],
    );
    Package { dir, id, hash }
}

fn code(answer: &Value) -> &str {
    if answer["ok"] == true {
        "ok"
    } else {
        answer["error"]["code"].as_str().unwrap_or("<none>")
    }
}

/// A portable guest gets the broker and nothing else, within its memory,
/// fuel and deadline: an ambient (WASI) import refuses the start, a spinning
/// guest is interrupted and frees the host, memory growth past 16 MiB is
/// denied, a trap fails only its call, broker requests are confined to the
/// bound task and grants, a cancelled guest's host calls stop, its log
/// burst is capped, and a killed host restarts on the next call. The host
/// reports the hash of the exact module bytes it compiled. Regressions
/// caught: WASI or any unlisted import is linked, a guest loop holds a host
/// thread past its deadline, the store limit is not applied, a trap kills
/// the host, a guest reaches another task or an ungranted capability, host
/// calls continue after cancellation, logs flood the engine, a host crash is
/// not contained, or the module compiled differs from the one pinned.
#[tokio::test(flavor = "multi_thread")]
async fn portable_guests_reach_only_the_broker_within_their_bounds() {
    let host = std::env::var_os("LUNA_EXTENSION_HOST")
        .expect("build luna-extension-host and set LUNA_EXTENSION_HOST to its absolute path");
    assert!(Path::new(&host).is_file(), "{host:?} is not the built host");

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let sources = root.join("sources");
    let task_a = root.join("task-a");
    let task_b = root.join("task-b");
    fs::create_dir_all(&task_a).unwrap();
    fs::create_dir_all(&task_b).unwrap();
    fs::write(task_a.join("notes.txt"), b"task A notes\n").unwrap();
    fs::write(task_b.join("secret.txt"), b"task B secret\n").unwrap();
    let a_before = tree_digest(&task_a);
    let b_before = tree_digest(&task_b);
    let guest_wasm = wat::parse_str(GUEST).expect("assemble the guest");
    let ambient_wasm = wat::parse_str(AMBIENT).expect("assemble the WASI guest");

    let engine = Engine::open(&data);
    engine.create_task("task-a", &task_a);
    engine.create_task("task-b", &task_b);

    let ambient = wasm_package(&sources, "ambient", &ambient_wasm, json!([]));
    engine.install_enabled(&ambient, &[]).await;
    let error = engine
        .start(&ambient, None, &[])
        .await
        .expect_err("a WASI import must be refused");
    assert!(error.contains("wasi_snapshot_preview1"), "{error}");
    let refused = engine.instances_of(&ambient).await;
    assert_eq!(refused[0]["state"], "failed", "{refused:?}");
    assert_eq!(refused[0]["lastExit"]["groupCleared"], true);
    assert!(!group_exists(
        refused[0]["lastExit"]["pid"].as_u64().unwrap()
    ));

    let guest = wasm_package(
        &sources,
        "guest",
        &guest_wasm,
        json!([{"id": WORKSPACE_READ}, {"id": WORKSPACE_WRITE, "required": false}]),
    );
    engine.install_enabled(&guest, &[WORKSPACE_READ]).await;
    let started = engine
        .start(&guest, Some("task-a"), &[])
        .await
        .expect("start the guest");
    let id = started["instanceId"].as_str().unwrap().to_owned();
    let pid = pid_of(&started);
    assert_eq!(started["trust"], "portable");
    assert_eq!(started["grants"], json!([WORKSPACE_READ]));
    assert_eq!(started["portable"]["moduleSha256"], sha256(&guest_wasm));
    assert_eq!(
        started["portable"]["imports"],
        json!(["luna_broker_v1::call", "luna_broker_v1::log"])
    );
    assert_eq!(
        started["operations"],
        json!(["echo", "spin", "grow", "trap", "host", "hostloop", "logs"])
    );
    assert_eq!(started["limits"]["guestMemoryBytes"], 16 << 20);
    assert_eq!(started["isolation"]["osSandbox"], false);
    assert!(
        started["isolation"]["enforced"]
            .as_array()
            .unwrap()
            .contains(&json!("noAmbientImports"))
    );
    assert_eq!(engine.ok(&id, "echo", json!({})).await, "steady");

    // Two spinning guests would hold both host threads unless the deadline
    // really interrupts them; the next call must still be answered at once.
    for _ in 0..2 {
        let spin = engine.invoke(&id, "spin", json!({}), 200).await.unwrap();
        assert_eq!(spin["status"], "deadlineExceeded", "{spin}");
    }
    let echo = engine.invoke(&id, "echo", json!({}), 500).await.unwrap();
    assert_eq!(
        echo["status"], "ok",
        "a cancelled guest still holds the host: {echo}"
    );

    assert_eq!(engine.ok(&id, "grow", json!({})).await, "denied");
    let trapped = engine.invoke(&id, "trap", json!({}), 5000).await.unwrap();
    assert_eq!(trapped["status"], "failed", "{trapped}");
    assert_eq!(trapped["error"]["code"], "trapped");
    assert_eq!(engine.ok(&id, "echo", json!({})).await, "steady");
    assert_eq!(
        pid_of(&engine.instance(&id).await),
        pid,
        "a trap ended the host"
    );

    // Broker requests from inside the guest.
    let read = engine
        .ok(
            &id,
            "host",
            json!({"op": "workspace.read", "params": {"path": "notes.txt"}}),
        )
        .await;
    assert_eq!(code(&read), "ok", "{read}");
    assert_eq!(
        BASE64
            .decode(read["result"]["data"].as_str().unwrap())
            .unwrap(),
        b"task A notes\n"
    );
    let mut refusals = Vec::new();
    for (request, expected) in [
        (
            json!({"op": "workspace.read", "params": {"path": "../task-b/secret.txt"}}),
            "outsideTarget",
        ),
        (
            json!({"op": "workspace.read", "params": {"path": "notes.txt", "taskId": "task-b"}}),
            "badRequest",
        ),
        (
            json!({"op": "workspace.write", "params": {"path": "out.txt", "data": BASE64.encode(b"x"), "create": true}}),
            "denied",
        ),
        (
            json!({"op": "SetExtensionEnabled", "params": {}}),
            "unknownOperation",
        ),
    ] {
        let answer = engine.ok(&id, "host", request.clone()).await;
        assert_eq!(code(&answer), expected, "{request}: {answer}");
        refusals.push(answer);
    }
    // A field beside op/params is refused by the host before the engine.
    let forged = engine
        .invoke(
            &id,
            "host",
            json!({"op": "workspace.read", "params": {"path": "notes.txt"}, "instanceId": "ext-other"}),
            5000,
        )
        .await
        .unwrap();
    assert_eq!(forged["status"], "failed", "{forged}");
    assert_eq!(forged["error"]["code"], "brokerCall");

    // A guest calling the broker in a loop is cancelled at its deadline;
    // after the grace period no host call is running or still arriving.
    let looped = engine
        .invoke(&id, "hostloop", json!({}), 100)
        .await
        .unwrap();
    assert_eq!(looped["status"], "deadlineExceeded", "{looped}");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let settled = engine.instance(&id).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let later = engine.instance(&id).await;
    assert!(settled["usage"]["hostCalls"].as_u64().unwrap() > 6);
    assert_eq!(
        later["usage"]["hostCalls"], settled["usage"]["hostCalls"],
        "host calls continued after cancellation"
    );
    assert_eq!(later["usage"]["hostCallsInFlight"], 0);
    assert_eq!(later["state"], "running");
    assert_eq!(pid_of(&later), pid);

    // 5,000 log calls are capped per call and bounded in the window.
    assert_eq!(engine.ok(&id, "logs", json!({})).await, "steady");
    let view = engine.instance(&id).await;
    let guest_lines = view["logs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|line| line["message"] == "guest log line")
        .count();
    assert!(
        (1..=256).contains(&guest_lines),
        "{guest_lines} guest lines"
    );

    // A host killed from outside is contained and restarted on demand.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    let crashed = engine
        .wait_for(&id, Duration::from_secs(5), "crash record", |view| {
            view["state"] == "crashed"
        })
        .await;
    assert_eq!(crashed["lastExit"]["reason"], "crashed");
    assert_eq!(crashed["lastExit"]["signal"], libc::SIGKILL);
    let echo = engine.invoke(&id, "echo", json!({}), 5000).await.unwrap();
    assert_eq!(echo["status"], "ok", "{echo}");
    assert_eq!(echo["generation"], 2);
    let restarted = engine.instance(&id).await;
    assert_ne!(pid_of(&restarted), pid);

    assert_eq!(tree_digest(&task_a), a_before);
    assert_eq!(tree_digest(&task_b), b_before);
    let last_pid = pid_of(&restarted);
    engine.close().await;
    assert!(wait_process_gone(last_pid, Duration::from_secs(2)).await);
    evidence(
        "e09-portable",
        &json!({
            "started": started,
            "refusedAmbient": refused,
            "refusals": refusals,
            "forged": forged,
            "hostLoop": later,
            "crashed": crashed,
            "restarted": restarted,
        }),
    );
}
