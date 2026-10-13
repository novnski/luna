//! Shared fixtures for the CORE-07 / E09 suites (`extension_runtime.rs`,
//! `extension_portable.rs`): real package bytes in disposable directories, a
//! real engine behind its public RPC methods, real child processes, and
//! readback the engine does not produce (process and group existence from
//! the OS, file bytes, the grants and runtime-state files).
//!
//! The package hash is recomputed here from the published rule (SHA-256 of
//! the exact `luna-plugin.json` bytes). Set `LUNA_CORE07_EVIDENCE_DIR` to
//! retain each test's readback as JSON.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use luna_engine::{
    AgentAccountsConfig, EngineCore, EngineProfile, HarnessId, HarnessRegistry, InstanceLock,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

pub const MANIFEST: &str = "luna-plugin.json";
pub const NATIVE_CODE: &str = "luna.capability/native-code";
pub const WORKSPACE_READ: &str = "luna.capability/workspace.read";
pub const WORKSPACE_WRITE: &str = "luna.capability/workspace.write";
pub const TASKS_READ: &str = "luna.capability/tasks.read";

pub struct Engine {
    pub core: EngineCore,
    pub client: luna_rpc::RpcClient,
    root: PathBuf,
}

impl Engine {
    pub fn open(root: &Path) -> Self {
        let accounts = AgentAccountsConfig {
            data_dir: root.join("synthetic-provider-state"),
            claude_config_dir: root.join("synthetic-claude"),
            claude_config_file: root.join("synthetic-claude.json"),
            codex_home: root.join("synthetic-codex"),
        };
        accounts
            .disable_claude_keychain_fallback()
            .expect("seal synthetic Claude home");
        let core =
            EngineCore::assemble_with_profile_and_agent_accounts_config_without_codex_metadata(
                EngineProfile::development(root, "org-a", "user-a"),
                Arc::new(HarnessRegistry::new()),
                HarnessId::Mock,
                None,
                accounts,
            )
            .expect("assemble profile");
        let client = luna_rpc::memory_client(core.rpc_service());
        Self {
            core,
            client,
            root: root.to_path_buf(),
        }
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.client
            .call(method, params)
            .await
            .map_err(|error| error.to_string())
    }

    /// Install `package` and enable it with exactly `grants`.
    pub async fn install_enabled(&self, package: &Package, grants: &[&str]) {
        let installed = self
            .call("InstallExtension", json!({"path": package.dir}))
            .await
            .expect("install");
        assert_eq!(installed["packageHash"], package.hash);
        assert_eq!(installed["state"], "disabled");
        self.set_enabled(package, true, grants)
            .await
            .expect("enable with exact grants");
    }

    pub async fn set_enabled(
        &self,
        package: &Package,
        enabled: bool,
        grants: &[&str],
    ) -> Result<Value, String> {
        self.call(
            "SetExtensionEnabled",
            json!({
                "extensionId": package.id,
                "packageHash": package.hash,
                "enabled": enabled,
                "grants": grants,
            }),
        )
        .await
    }

    pub async fn start(
        &self,
        package: &Package,
        task: Option<&str>,
        require: &[&str],
    ) -> Result<Value, String> {
        let mut params = json!({"extensionId": package.id, "packageHash": package.hash});
        if let Some(task) = task {
            params["taskId"] = json!(task);
        }
        if !require.is_empty() {
            params["require"] = json!(require);
        }
        self.call("StartExtensionInstance", params).await
    }

    /// One call; the outcome (an `Err` only when the engine refused it).
    pub async fn invoke(
        &self,
        instance: &str,
        method: &str,
        params: Value,
        timeout_ms: u64,
    ) -> Result<Value, String> {
        self.call(
            "CallExtensionInstance",
            json!({
                "instanceId": instance,
                "method": method,
                "params": params,
                "timeoutMs": timeout_ms,
            }),
        )
        .await
    }

    /// A call that must be answered successfully; returns its result.
    pub async fn ok(&self, instance: &str, method: &str, params: Value) -> Value {
        let outcome = self
            .invoke(instance, method, params, 10_000)
            .await
            .expect("call admitted");
        assert_eq!(outcome["status"], "ok", "{method}: {outcome}");
        outcome["result"].clone()
    }

    pub async fn stop(&self, instance: &str) -> Value {
        self.call("StopExtensionInstance", json!({"instanceId": instance}))
            .await
            .expect("stop")
    }

    pub async fn snapshot(&self) -> Value {
        let snapshot = self
            .call("ListExtensionInstances", json!({}))
            .await
            .expect("list instances");
        assert_eq!(snapshot["stateError"], Value::Null, "{snapshot}");
        snapshot
    }

    pub async fn instance(&self, instance: &str) -> Value {
        self.snapshot().await["instances"]
            .as_array()
            .unwrap()
            .iter()
            .find(|view| view["instanceId"] == instance)
            .cloned()
            .unwrap_or_else(|| panic!("instance {instance} is not listed"))
    }

    pub async fn instances_of(&self, package: &Package) -> Vec<Value> {
        self.snapshot().await["instances"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|view| view["packageHash"] == package.hash)
            .cloned()
            .collect()
    }

    /// Poll the listing until `done` holds for the instance, or panic after
    /// `within`. The supervisor reports exits asynchronously (a hung worker
    /// is stopped after the cancel grace period).
    pub async fn wait_for(
        &self,
        instance: &str,
        within: Duration,
        what: &str,
        done: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = Instant::now() + within;
        loop {
            let view = self.instance(instance).await;
            if done(&view) {
                return view;
            }
            assert!(
                Instant::now() < deadline,
                "{what} did not happen within {within:?}: {view}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub fn create_task(&self, id: &str, checkout: &Path) {
        self.core
            .workspace
            .create_chat(
                id,
                None,
                Some(&self.core.device_id),
                None,
                Some(checkout.to_string_lossy().into_owned()),
            )
            .expect("create task");
    }

    /// Shut down and drop the engine; its data root must then be free
    /// (no worker, task or retained handle keeps the instance lock), which
    /// is what a backup export or a fresh engine checks.
    pub async fn close(self) {
        drop(self.client);
        self.core.shutdown().await;
        drop(self.core);
        assert_eq!(
            InstanceLock::holder(&self.root),
            None,
            "the data root is still locked after shutdown and drop"
        );
    }
}

pub struct Package {
    pub dir: PathBuf,
    pub id: String,
    pub hash: String,
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Write a package directory with the exact manifest bytes for `files`.
pub fn write_package(dir: &Path, mut manifest: Value, files: &[(&str, &[u8])]) -> String {
    for (path, bytes) in files {
        let target = dir.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, bytes).unwrap();
    }
    manifest["files"] = json!(
        files
            .iter()
            .map(
                |(path, bytes)| json!({"path": path, "size": bytes.len(), "sha256": sha256(bytes)})
            )
            .collect::<Vec<_>>()
    );
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    fs::write(dir.join(MANIFEST), &bytes).unwrap();
    sha256(&bytes)
}

pub fn base_manifest(id: &str, runtime: Value, capabilities: Value) -> Value {
    json!({
        "manifestVersion": 1,
        "id": id,
        "version": "1.0.0",
        "displayName": "CORE-07 fixture",
        "publisher": "CORE-07 E09",
        "api": {"major": 1, "minMinor": 1},
        "runtime": runtime,
        "capabilities": capabilities,
    })
}

/// A native package running [`FIXTURE_WORKER`], configured by `fixture`.
pub fn native_package(
    sources: &Path,
    name: &str,
    fixture: Value,
    limits: Option<Value>,
    capabilities: Value,
) -> Package {
    let id = format!("core07.fixture/{name}");
    let mut runtime = json!({
        "kind": "native",
        "protocol": {"major": 1, "minMinor": 0},
        "entries": {platform(): "bin/worker"},
    });
    if let Some(limits) = limits {
        runtime["limits"] = limits;
    }
    let dir = sources.join(name);
    let config = serde_json::to_vec(&fixture).unwrap();
    let hash = write_package(
        &dir,
        base_manifest(&id, runtime, capabilities),
        &[
            ("bin/worker", FIXTURE_WORKER.as_bytes()),
            ("fixture.json", config.as_slice()),
        ],
    );
    Package { dir, id, hash }
}

/// Whether a process with this pid exists (zombies included), by the OS.
pub fn process_exists(pid: u64) -> bool {
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Whether any process of the group exists, by the OS.
pub fn group_exists(pgid: u64) -> bool {
    let result = unsafe { libc::kill(-(pgid as libc::pid_t), 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Wait until the OS no longer has the process (it may be a zombie of its
/// new parent for a moment after the supervisor kills a child group).
pub async fn wait_process_gone(pid: u64, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    !process_exists(pid)
}

/// SHA-256 of every file under `root`, keyed by relative path; links are
/// recorded by their target, never followed.
pub fn tree_digest(root: &Path) -> BTreeMap<String, String> {
    let mut digest = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).unwrap();
                digest.insert(relative, format!("link:{}", target.display()));
            } else if metadata.is_dir() {
                pending.push(path);
            } else {
                digest.insert(relative, sha256(&fs::read(&path).unwrap()));
            }
        }
    }
    digest
}

pub fn evidence(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("LUNA_CORE07_EVIDENCE_DIR") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}

pub fn pid_of(view: &Value) -> u64 {
    view["pid"]
        .as_u64()
        .unwrap_or_else(|| panic!("no running pid: {view}"))
}

/// A worker for protocol 1.0, written independently of the engine. It reads
/// stdin on its own thread (so it never stops draining its input), takes
/// its hello behaviour from `fixture.json` in its package directory (the
/// working directory) and its runtime behaviour from each call's method.
pub const FIXTURE_WORKER: &str = r##"#!/usr/bin/env python3
"""CORE-07 E09 fixture worker (test bytes only, never part of LUNA)."""
import json, os, queue, sys, threading, time

def send(frame):
    sys.stdout.write(json.dumps(frame, separators=(",", ":")) + "\n")
    sys.stdout.flush()

config = json.load(open("fixture.json"))
mode = config.get("hello", "ok")
first = sys.stdin.readline()
if not first:
    sys.exit(0)
hello = json.loads(first)
if mode == "silent":
    while True:
        time.sleep(60)
if mode == "exit":
    sys.exit(5)
if mode == "json":
    sys.stdout.write("{not json\n")
    sys.stdout.flush()
    time.sleep(30)
    sys.exit(0)
if mode == "oversize":
    sys.stdout.write("x" * (70 * 1024))
    sys.stdout.flush()
    time.sleep(30)
    sys.exit(0)
operations = config.get("operations", [
    "fixture.echo", "fixture.record", "fixture.hang", "fixture.wait", "fixture.env",
    "fixture.broker", "fixture.logs", "fixture.stderr", "fixture.flood", "fixture.hostflood",
    "fixture.alloc", "fixture.spin", "fixture.fork",
])
reply = {
    "type": "hello",
    "id": hello["id"],
    "protocol": {"major": 1, "minor": 3},
    "extensionId": hello["instance"]["extensionId"],
    "packageHash": hello["instance"]["packageHash"],
    "operations": operations,
    "grants": ["luna.capability/workspace.write"],
    "futureField": {"anything": True},
}
if mode == "major2":
    reply["protocol"]["major"] = 2
if mode == "wrongId":
    reply["id"] = "not-the-request"
if mode == "wrongHash":
    reply["packageHash"] = "0" * 64
send(reply)

inbox = queue.Queue()
def drain():
    for line in sys.stdin:
        inbox.put(json.loads(line))
    os._exit(0)
threading.Thread(target=drain, daemon=True).start()

backlog = []
waiting = set()
ignored = set()
last_cancelled = [None]
counter = [0]

def next_frame():
    return backlog.pop(0) if backlog else inbox.get()

def host_call(op, params, extra=None):
    counter[0] += 1
    hid = "hc-%d" % counter[0]
    frame = {"type": "hostCall", "id": hid, "op": op, "params": params}
    if extra:
        frame.update(extra)
    send(frame)
    while True:
        frame = inbox.get()
        if frame.get("type") == "hostResult" and frame.get("id") == hid:
            del frame["type"]
            del frame["id"]
            return frame
        backlog.append(frame)

def respond(fid, result=None, error=None):
    if error is None:
        send({"type": "response", "id": fid, "ok": True, "result": result})
    else:
        send({"type": "response", "id": fid, "ok": False, "error": error})

while True:
    frame = next_frame()
    kind = frame.get("type")
    if kind == "shutdown":
        sys.exit(0)
    if kind == "cancel":
        if frame["id"] in waiting:
            waiting.discard(frame["id"])
            last_cancelled[0] = frame["id"]
            respond(frame["id"], error={"code": "cancelled", "message": "the fixture stopped waiting"})
        continue
    if kind != "call":
        continue
    fid, method, params = frame["id"], frame["method"], frame.get("params") or {}
    if method == "fixture.echo":
        respond(fid, {"pid": os.getpid(), "params": params, "lastCancelled": last_cancelled[0]})
    elif method == "fixture.record":
        with open(params["path"], "a") as out:
            out.write(params["line"] + "\n")
        if params.get("crash"):
            os._exit(3)
        respond(fid, {"recorded": True})
    elif method == "fixture.hang":
        ignored.add(fid)
    elif method == "fixture.wait":
        waiting.add(fid)
    elif method == "fixture.env":
        fds = []
        for fd in range(3, 1024):
            try:
                os.fstat(fd)
                fds.append(fd)
            except OSError:
                pass
        respond(fid, {
            "env": sorted(os.environ.keys()),
            "fds": fds,
            "cwd": os.getcwd(),
            "pid": os.getpid(),
            "pgid": os.getpgid(0),
            "sid": os.getsid(0),
        })
    elif method == "fixture.broker":
        respond(fid, [host_call(c["op"], c.get("params", {}), c.get("frame")) for c in params["calls"]])
    elif method == "fixture.logs":
        for i in range(params["count"]):
            send({"type": "log", "level": "info", "message": "line %d" % i})
        respond(fid, {"sent": params["count"]})
    elif method == "fixture.stderr":
        line = "e" * 1023 + "\n"
        for _ in range(params["bytes"] // 1024):
            sys.stderr.write(line)
        sys.stderr.flush()
        respond(fid, {"written": params["bytes"] // 1024 * 1024})
    elif method == "fixture.flood":
        while True:
            send({"type": "log", "level": "info", "message": "flood"})
    elif method == "fixture.hostflood":
        count = params["count"]
        for i in range(count):
            send({"type": "hostCall", "id": "flood-%d" % i, "op": "host.info", "params": {}})
        codes = {}
        seen = 0
        while seen < count:
            answer = inbox.get()
            if answer.get("type") == "hostResult" and str(answer.get("id", "")).startswith("flood-"):
                seen += 1
                code = "ok" if answer.get("ok") else answer["error"]["code"]
                codes[code] = codes.get(code, 0) + 1
            else:
                backlog.append(answer)
        respond(fid, codes)
    elif method == "fixture.alloc":
        blob = bytearray(b"\x01") * (params["mib"] * 1024 * 1024)
        time.sleep(30)
        respond(fid, len(blob))
    elif method == "fixture.spin":
        while True:
            pass
    elif method == "fixture.fork":
        for _ in range(params["count"]):
            if os.fork() == 0:
                time.sleep(30)
                os._exit(0)
        time.sleep(30)
        respond(fid, "forked")
    else:
        respond(fid, error={"code": "unknownOperation", "message": method})
"##;
