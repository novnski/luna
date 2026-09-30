#!/usr/bin/env python3
"""Deterministic review protocol peer; QA evidence, never real-provider proof."""
import json
import os
import sys
from pathlib import Path

if "--version" in sys.argv:
    print("codex-cli 0.159.0")
    raise SystemExit(0)

thread_id = f"native-review-{os.getpid()}"
cwd = None
active_turn = None
counter = 0
title_only = False
state_file = Path(os.environ.get("CODEX_HOME", ".")) / "native-review-fixture-threads.json"
state_file.parent.mkdir(parents=True, exist_ok=True)
threads = json.loads(state_file.read_text()) if state_file.exists() else {}

def load_threads():
    # Several fixture processes (runs, title/metadata clients) share this state.
    return json.loads(state_file.read_text()) if state_file.exists() else {}

def save_threads(value):
    state_file.write_text(json.dumps(value))

def emit(value):
    print(json.dumps(value), flush=True)

def notify(method, params):
    emit({"method": method, "params": params})

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    params = request.get("params", {})
    request_id = request.get("id")
    if method in ("thread/start", "thread/resume"):
        cwd = Path(params["cwd"])
        thread_id = params.get("threadId", thread_id)
        title_only = params.get("ephemeral", False)
        if not title_only:
            threads = load_threads()
            # Resume keeps the persisted native name LUNA reconciles against.
            entry = threads.get(thread_id, {"id": thread_id, "turns": []})
            entry.update({"cwd": str(cwd), "status": {"type": "idle"}})
            threads[thread_id] = entry
            save_threads(threads)
    if cwd and request_id is not None and not title_only:
        with (cwd / ".native-review-protocol.jsonl").open("a") as capture:
            capture.write(json.dumps({"method": method, "params": params}) + "\n")
    if request_id is None:
        continue
    result = {}
    if method == "initialize":
        result = {"userAgent": "luna-native-review-fixture"}
    elif method in ("thread/start", "thread/resume", "thread/read"):
        requested_id = params.get("threadId", thread_id)
        threads = load_threads() or threads
        result = {"thread": threads.get(requested_id, {"id": requested_id, "turns": [], "cwd": str(cwd), "status": {"type": "idle"}})}
    elif method == "thread/name/set":
        threads = load_threads()
        named = params["threadId"]
        threads.setdefault(named, {"id": named, "turns": [], "status": {"type": "idle"}})["name"] = params.get("name")
        save_threads(threads)
    elif method == "thread/goal/get":
        # Installed Codex 0.159 ThreadGoalGetResponse: no goal on this thread.
        result = {"goal": None}
    elif method == "model/list":
        result = {"data": [], "nextCursor": None}
    elif method == "thread/list":
        result = {"data": [], "nextCursor": None}
    elif method == "account/read":
        result = {"account": None, "requiresOpenaiAuth": False}
    elif method == "account/rateLimits/read":
        result = {"rateLimits": None, "rateLimitsByLimitId": {}}
    elif method == "review/start":
        target = params["target"]
        if "[reject]" in target.get("instructions", ""):
            emit({"id": request_id, "error": {"code": -32602, "message": "review fixture rejects this target"}})
            continue
        counter += 1
        active_turn = f"review-turn-{os.getpid()}-{counter}"
        returned_thread = "unexpected-foreign-thread" if "[foreign]" in target.get("instructions", "") else thread_id
        emit({"id": request_id, "result": {"turn": {"id": active_turn, "status": "inProgress", "items": []}, "reviewThreadId": returned_thread}})
        if returned_thread != thread_id:
            continue
        notify("turn/started", {"threadId": thread_id, "turn": {"id": active_turn, "status": "inProgress"}})
        entry = {"id": active_turn + "-entry", "type": "enteredReviewMode", "review": target["type"]}
        for phase in ("started", "completed"):
            notify("item/" + phase, {"threadId": thread_id, "turnId": active_turn, "item": entry})
        if "[hold]" in target.get("instructions", ""):
            continue
        final = {"id": active_turn + "-exit", "type": "exitedReviewMode", "review": f"[P1] Fixture finding for {target['type']}\n\nInspect fixture.js:1. Deterministic protocol evidence only."}
        if "[missing]" not in target.get("instructions", ""):
            for phase in ("started", "completed", "completed"):
                notify("item/" + phase, {"threadId": thread_id, "turnId": active_turn, "item": final})
        notify("turn/completed", {"threadId": thread_id, "turn": {"id": active_turn, "status": "completed"}})
        continue
    elif method == "turn/start" and title_only:
        turn = {"id": "title-turn", "status": "completed"}
        emit({"id": request_id, "result": {"turn": turn}})
        notify("item/agentMessage/delta", {"threadId": thread_id, "itemId": "title", "delta": "Native review QA"})
        notify("turn/completed", {"threadId": thread_id, "turn": turn})
        continue
    elif method == "turn/start":
        emit({"id": request_id, "error": {"code": -32601, "message": "review QA must use review/start"}})
        continue
    elif method == "turn/interrupt" and active_turn:
        emit({"id": request_id, "result": {}})
        notify("turn/completed", {"threadId": thread_id, "turn": {"id": active_turn, "status": "interrupted"}})
        continue
    emit({"id": request_id, "result": result})
