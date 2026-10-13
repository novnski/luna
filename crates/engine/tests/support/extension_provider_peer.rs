//! CORE-08 / E10 provider packages: real package bytes whose native worker
//! is an independently written Python peer of provider profile 1.0 over
//! worker protocol 1.0. It is not the SDK and not LUNA code.
//!
//! On `provider.run.start` the peer starts a real child process standing in
//! for the provider CLI (in the worker's process group, as a CLI started by a
//! worker is), writes its pid to `peer-cli.pid` in the run's working
//! directory and appends every start, stop and approval answer it receives to
//! `peer-calls.log` there. Its behaviour comes from a listed `fixture.json`,
//! so every scenario is its own package hash.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::fixture::{Package, platform, write_package};

pub const PROVIDER_REGISTER: &str = "luna.capability/provider.register";

/// A provider package whose provider id is its package id,
/// `core08.peer/<name>`.
pub fn provider_package(
    sources: &Path,
    name: &str,
    fixture: Value,
    declaration: Option<Value>,
    register_required: bool,
) -> Package {
    let id = format!("core08.peer/{name}");
    let manifest = json!({
        "manifestVersion": 1,
        "id": id,
        "version": "1.0.0",
        "displayName": format!("CORE-08 peer {name}"),
        "publisher": "CORE-08 E10",
        "api": {"major": 1, "minMinor": 0},
        "runtime": {
            "kind": "native",
            "protocol": {"major": 1, "minMinor": 0},
            "entries": {platform(): "bin/worker"},
        },
        "contributions": [{"id": id, "kind": "provider", "title": "Peer"}],
        "capabilities": [{"id": PROVIDER_REGISTER, "required": register_required}],
    });
    let dir = sources.join(name);
    let config = serde_json::to_vec(&fixture).unwrap();
    let declared = declaration.map(|declaration| serde_json::to_vec_pretty(&declaration).unwrap());
    let mut files: Vec<(&str, &[u8])> = vec![
        ("bin/worker", PEER.as_bytes()),
        ("fixture.json", config.as_slice()),
    ];
    if let Some(bytes) = &declared {
        files.push(("luna-provider.json", bytes.as_slice()));
    }
    let hash = write_package(&dir, manifest, &files);
    Package { dir, id, hash }
}

/// The ordinary declaration: Full only, with the resume and approvals
/// profiles; `extra` fields replace the defaults.
pub fn declaration(id: &str, extra: Value) -> Value {
    let mut provider = json!({
        "id": id,
        "displayName": "Peer Provider",
        "profile": {"major": 1, "minor": 0},
        "optional": ["resume", "approvals"],
        "permissionModes": {"full": "the peer runs every action without asking"},
        "unsupported": [{"capability": "steer", "reason": "the peer takes one prompt per run"}],
        "limitations": ["Test peer only."],
    });
    if let (Some(provider), Some(extra)) = (provider.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            provider.insert(key.clone(), value.clone());
        }
    }
    json!({"declarationVersion": 1, "providers": [provider]})
}

/// The lines the peer logged in `checkout`.
pub fn calls(checkout: &Path) -> Vec<String> {
    fs::read_to_string(checkout.join("peer-calls.log"))
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The `start` records the peer logged, decoded.
pub fn starts(checkout: &Path) -> Vec<Value> {
    calls(checkout)
        .iter()
        .filter_map(|line| line.strip_prefix("start "))
        .map(|json| serde_json::from_str(json).unwrap())
        .collect()
}

/// The pid of the stand-in CLI the peer started in `checkout`.
pub async fn cli_pid(checkout: &Path, within: Duration) -> u64 {
    let path: PathBuf = checkout.join("peer-cli.pid");
    let deadline = Instant::now() + within;
    loop {
        if let Some(pid) = fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "the peer never started its CLI in {}",
            checkout.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Provider profile 1.0 peer (test bytes only). The main thread only reads
/// and dispatches frames, so it always drains its input and answers cancels;
/// long polls and runs use their own threads. Every call is answered once.
pub const PEER: &str = r##"#!/usr/bin/env python3
"""CORE-08 E10 provider-profile peer (test bytes only, never part of LUNA)."""
import json, os, signal, subprocess, sys, threading, time, uuid

config = json.load(open("fixture.json"))
scenario = config.get("run", "complete")
if scenario == "ignoreStop":
    # A worker that ignores both its stop and SIGTERM; the CLI inherits it.
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
out_lock = threading.Lock()

def send(frame):
    line = json.dumps(frame, separators=(",", ":")) + "\n"
    with out_lock:
        sys.stdout.write(line)
        sys.stdout.flush()

first = sys.stdin.readline()
if not first:
    sys.exit(0)
hello = json.loads(first)
OPS = ["provider.describe", "provider.models", "provider.run.start",
       "provider.run.events", "provider.run.stop", "provider.run.answer"]
send({
    "type": "hello",
    "id": hello["id"],
    "protocol": {"major": 2 if config.get("hello") == "major2" else 1, "minor": 0},
    "extensionId": hello["instance"]["extensionId"],
    "packageHash": hello["instance"]["packageHash"],
    "operations": config.get("operations", OPS),
})

state = threading.Condition()
runs = {}
pending = {}

def respond(fid, result=None, error=None):
    with state:
        if not pending.pop(fid, False):
            return
        state.notify_all()
    if error is None:
        send({"type": "response", "id": fid, "ok": True, "result": result})
    else:
        send({"type": "response", "id": fid, "ok": False, "error": error})

def log(cwd, line):
    with open(os.path.join(cwd, "peer-calls.log"), "a") as out:
        out.write(line + "\n")

def push(run, event):
    with state:
        run["seq"] += 1
        event["seq"] = run["seq"]
        run["events"].append(event)
        state.notify_all()

def finish(run, status, **extra):
    event = {"type": "done", "status": status}
    event.update(extra)
    push(run, event)

def end_cli(run):
    child = run.get("child")
    if child is not None and child.poll() is None:
        child.terminate()
        child.wait()

def wait_stop(run, seconds):
    with state:
        state.wait_for(lambda: run["stop"], timeout=seconds)
        return run["stop"]

def answer_of(run, request_id, seconds):
    with state:
        state.wait_for(lambda: request_id in run["answers"], timeout=seconds)
        return run["answers"].get(request_id)

def drive(run):
    session = config.get("session") or run["resume"] or ("peer-session-" + uuid.uuid4().hex[:8])
    if scenario == "wrongSession" and run["resume"]:
        session = run["resume"] + "-other"
    push(run, {"type": "session", "sessionId": session, "model": run["model"] or "peer-fast"})
    if scenario == "approvalWithoutProfile":
        push(run, {"type": "approvalRequest", "requestId": "approval-1", "kind": "command", "subject": "echo hi"})
        wait_stop(run, 30)
        end_cli(run)
        finish(run, "interrupted")
        return
    push(run, {"type": "text", "text": config.get("text", "PEER-REPLY")})
    if scenario == "crash":
        time.sleep(0.3)
        os._exit(3)
    if scenario in ("hang", "ignoreStop"):
        wait_stop(run, 60)
        if scenario == "ignoreStop":
            log(run["cwd"], "ignored-stop")
            time.sleep(600)
            return
        end_cli(run)
        log(run["cwd"], "stopped")
        finish(run, "interrupted")
        return
    push(run, {"type": "toolCall", "id": "tool-1", "name": "run_shell_command",
               "call": {"kind": "exec", "command": "echo peer"}, "input": {"command": "echo peer"}})
    push(run, {"type": "toolResult", "id": "tool-1", "isError": False, "output": "peer"})
    if config.get("approval"):
        push(run, {"type": "approvalRequest", "requestId": "approval-1", "kind": "command",
                   "subject": "rm -rf build"})
        decision = answer_of(run, "approval-1", 30)
        log(run["cwd"], "approval " + json.dumps(decision, sort_keys=True))
    push(run, {"type": "usage", "inputTokens": 12, "outputTokens": 3})
    end_cli(run)
    finish(run, "completed")

def start(fid, params):
    cwd = params["cwd"]
    keys = ("prompt", "model", "permissionMode", "resume", "attachments")
    log(cwd, "start " + json.dumps({key: params.get(key) for key in keys}, sort_keys=True))
    if config.get("start") == "reject":
        respond(fid, error={"code": "rejected", "message": "the peer refused before starting its CLI"})
        return
    # The stand-in CLI has its own stdio, like a CLI the worker pipes to.
    child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"],
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL)
    with open(os.path.join(cwd, "peer-cli.pid"), "w") as out:
        out.write(str(child.pid))
    run = {"id": "run-" + uuid.uuid4().hex[:8], "seq": 0, "events": [], "stop": False,
           "answers": {}, "child": child, "cwd": cwd, "resume": params.get("resume"),
           "model": params.get("model")}
    with state:
        runs[run["id"]] = run
    respond(fid, {"runId": run["id"]})
    threading.Thread(target=drive, args=(run,), daemon=True).start()

def events(fid, params, deadline_ms):
    run = runs.get(params.get("runId"))
    if run is None:
        respond(fid, error={"code": "unknownRun", "message": "no such run"})
        return
    after = params.get("after", 0)
    wait = min(params.get("waitMs", 1000), max(deadline_ms - 500, 0)) / 1000.0
    with state:
        state.wait_for(lambda: run["seq"] > after or fid not in pending, timeout=wait)
        page = [event for event in run["events"] if event["seq"] > after]
    done = bool(page) and page[-1]["type"] == "done"
    respond(fid, {"events": page, "done": done})

def shutdown():
    if scenario == "ignoreStop":
        return
    for run in list(runs.values()):
        end_cli(run)
    os._exit(0)

def dispatch(frame):
    kind = frame.get("type")
    if kind == "shutdown":
        shutdown()
        return
    if kind == "cancel":
        fid = frame.get("id")
        with state:
            live = pending.get(fid, False)
        if live:
            respond(fid, error={"code": "cancelled", "message": "the peer stopped waiting"})
        return
    if kind != "call":
        return
    fid, method, params = frame["id"], frame["method"], frame.get("params") or {}
    with state:
        pending[fid] = True
    if method == "provider.describe":
        described = {"profile": {"major": 1, "minor": 0},
                     "providerId": hello["instance"]["extensionId"],
                     "available": True,
                     "executable": {"path": "/peer/cli", "version": "1.0.0"}}
        described.update(config.get("describe", {}))
        respond(fid, described)
    elif method == "provider.models":
        respond(fid, {"models": config.get("models", [
            {"id": "peer-fast", "label": "Peer Fast"},
            {"id": "peer-deep", "label": "Peer Deep", "description": "slower"},
        ])})
    elif method == "provider.run.start":
        start(fid, params)
    elif method == "provider.run.events":
        threading.Thread(target=events, args=(fid, params, frame.get("deadlineMs", 30000)),
                         daemon=True).start()
    elif method == "provider.run.stop":
        run = runs.get(params.get("runId"))
        if run is None:
            respond(fid, error={"code": "unknownRun", "message": "no such run"})
            return
        log(run["cwd"], "stop")
        with state:
            run["stop"] = True
            state.notify_all()
        respond(fid, {"stopping": True})
    elif method == "provider.run.answer":
        run = runs.get(params.get("runId"))
        if run is None:
            respond(fid, error={"code": "unknownRun", "message": "no such run"})
            return
        kept = {key: params[key] for key in ("decision", "reason", "answers", "cancelled") if key in params}
        with state:
            run["answers"][params.get("requestId")] = kept
            state.notify_all()
        respond(fid, {})
    else:
        respond(fid, error={"code": "unknownOperation", "message": method})

for line in sys.stdin:
    dispatch(json.loads(line))
shutdown()
while True:
    time.sleep(60)
"##;
