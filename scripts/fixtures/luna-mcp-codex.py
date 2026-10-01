#!/usr/bin/env python3
"""Deterministic Codex app-server peer that uses LUNA tools over MCP.

Acts as Codex 0.159 does with `-c mcp_servers.luna.*`: it reads the endpoint
from argv and the bearer token from `LUNA_MCP_BEARER_TOKEN`, then calls tools
with Codex's `_meta` (threadId, callId, x-codex-turn-metadata.turn_id).
Every MCP exchange is appended to `$CODEX_HOME/luna-mcp-frames.jsonl` with the
token redacted. QA evidence only, never real-provider proof.
"""
import json
import os
import signal
import sys
import threading
import urllib.error
import urllib.request
from pathlib import Path

if "--version" in sys.argv:
    print("codex-cli 0.159.0")
    raise SystemExit(0)

home = Path(os.environ.get("CODEX_HOME", "."))
home.mkdir(parents=True, exist_ok=True)
frames = home / "luna-mcp-frames.jsonl"
state_file = home / "luna-mcp-fixture-threads.json"
token = os.environ.get("LUNA_MCP_BEARER_TOKEN", "")
url = ""
for arg in sys.argv:
    if arg.startswith("mcp_servers.luna.url="):
        url = arg.split("=", 1)[1].strip('"')
write_lock = threading.Lock()
thread_id = None
active_turn = None
turn_done = threading.Event()
title_only = False
counter = 0


def emit(value):
    with write_lock:
        print(json.dumps(value), flush=True)


def notify(method, params):
    emit({"method": method, "params": params})


def record(entry):
    with write_lock, frames.open("a") as handle:
        handle.write(json.dumps(entry) + "\n")


def post(body, bearer=None, label=None):
    request = urllib.request.Request(
        url,
        data=json.dumps(body).encode(),
        method="POST",
        headers={
            "Authorization": f"Bearer {token if bearer is None else bearer}",
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=600) as response:
            status, payload = response.status, response.read().decode()
    except urllib.error.HTTPError as error:
        status, payload = error.code, error.read().decode()
    except OSError as error:
        status, payload = None, str(error)
    try:
        parsed = json.loads(payload) if payload else None
    except ValueError:
        parsed = payload
    record({"pid": os.getpid(), "label": label, "threadId": thread_id, "turnId": active_turn,
            "request": body, "status": status, "response": parsed})
    return status, parsed


def tool(call_id, name, arguments, thread=None, turn=None, label=None):
    meta = {"threadId": thread or thread_id, "callId": call_id,
            "x-codex-turn-metadata": {"turn_id": turn or active_turn}}
    body = {"jsonrpc": "2.0", "id": call_id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments, "_meta": meta}}
    return post(body, label=label or name)


def finish(turn, status="completed"):
    if not turn_done.is_set():
        turn_done.set()
        notify("turn/completed", {"threadId": thread_id, "turn": {"id": turn, "status": status}})


scenarios = []


def drain(*_):
    # The adapter ends a stopped session with SIGTERM (then SIGKILL after its
    # grace): give an in-flight MCP call time to record its reply first.
    for worker in scenarios:
        worker.join(timeout=2)
    os._exit(0)


signal.signal(signal.SIGTERM, drain)


def scenario(text, turn):
    post({"jsonrpc": "2.0", "id": "init", "method": "initialize",
          "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                     "clientInfo": {"name": "luna-mcp-qa-peer", "version": "0"}}}, label="initialize")
    post({"jsonrpc": "2.0", "method": "notifications/initialized"}, label="initialized")
    post({"jsonrpc": "2.0", "id": "list", "method": "tools/list"}, label="tools/list")
    tool(f"{turn}-list", "list_threads", {"limit": 5})
    if "[mcp-qa:first]" in text:
        post({"jsonrpc": "2.0", "id": "denied", "method": "tools/list"}, bearer="wrong-token", label="wrong-token")
        tool(f"{turn}-foreign", "luna_status", {}, thread="foreign-thread", label="foreign-thread")
        # Parks on the local LUNA approval until QA stops the turn.
        record({"pid": os.getpid(), "label": "create_thread:sent", "threadId": thread_id, "turnId": active_turn})
        tool(f"{turn}-create", "create_thread",
             {"prompt": "MCP QA child brief", "idempotencyKey": "mcp-qa-child", "title": "MCP QA child"})
    finish(turn)


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    params = request.get("params", {})
    request_id = request.get("id")
    if request_id is None:
        continue
    result = {}
    if method == "initialize":
        result = {"userAgent": "luna-mcp-qa-fixture"}
    elif method in ("thread/start", "thread/resume"):
        title_only = params.get("ephemeral", False)
        threads = json.loads(state_file.read_text()) if state_file.exists() else {}
        thread_id = params.get("threadId") or f"luna-mcp-qa-{os.getpid()}"
        if not title_only:
            threads[thread_id] = {"id": thread_id, "turns": [], "status": {"type": "idle"}}
            state_file.write_text(json.dumps(threads))
            record({"pid": os.getpid(), "label": method, "threadId": thread_id,
                    "mcpUrlPresent": bool(url), "tokenPresent": bool(token),
                    "dynamicTools": "dynamicTools" in params})
        result = {"thread": {"id": thread_id, "turns": [], "status": {"type": "idle"}}}
    elif method == "thread/read":
        result = {"thread": {"id": params.get("threadId"), "turns": [], "status": {"type": "idle"}}}
    elif method == "thread/goal/get":
        result = {"goal": None}
    elif method in ("model/list", "thread/list"):
        result = {"data": [], "nextCursor": None}
    elif method == "skills/list":
        result = {"data": []}
    elif method == "config/read":
        result = {"config": {"mcp_servers": {}}}
    elif method == "account/read":
        result = {"account": None, "requiresOpenaiAuth": False}
    elif method == "turn/start" and title_only:
        turn = {"id": "title-turn", "status": "completed"}
        emit({"id": request_id, "result": {"turn": turn}})
        notify("item/agentMessage/delta", {"threadId": thread_id, "itemId": "title", "delta": "LUNA MCP QA"})
        notify("turn/completed", {"threadId": thread_id, "turn": turn})
        continue
    elif method == "turn/start":
        counter += 1
        active_turn = f"mcp-qa-turn-{os.getpid()}-{counter}"
        turn_done.clear()
        emit({"id": request_id, "result": {"turn": {"id": active_turn, "status": "inProgress", "items": []}}})
        notify("turn/started", {"threadId": thread_id, "turn": {"id": active_turn, "status": "inProgress"}})
        worker = threading.Thread(target=scenario, args=(json.dumps(params), active_turn), daemon=True)
        scenarios.append(worker)
        worker.start()
        continue
    elif method == "turn/interrupt":
        emit({"id": request_id, "result": {}})
        if active_turn:
            finish(active_turn, "interrupted")
        continue
    emit({"id": request_id, "result": result})

drain()
