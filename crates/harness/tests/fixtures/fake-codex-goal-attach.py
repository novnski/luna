#!/usr/bin/env python3
"""Stateful native-format peer for cold goal continuation; no model/network.

The test copies this executable to a disposable directory. Metadata clients
can change/read a goal but cannot run it. Only an owned, instructed
thread/resume starts the existing goal. Counters and creation time survive.
"""
import json
import pathlib
import sys
import time

base = pathlib.Path(__file__).resolve().parent
state_path = base / "goal-state.json"
trace_path = base / "goal-trace.jsonl"
name_path = base / "thread-name.txt"
thread = "cold-goal-thread"
owned = False
active_turn = None


def state():
    return json.loads(state_path.read_text()) if state_path.exists() else {"goal": None}


def save(value):
    state_path.write_text(json.dumps(value))


def emit(value):
    print(json.dumps(value), flush=True)


def notify(method, params):
    emit({"method": method, "params": params})


for raw in sys.stdin:
    request = json.loads(raw)
    method, params = request.get("method"), request.get("params") or {}
    with trace_path.open("a") as log:
        log.write(json.dumps({"method": method, "params": params, "owned": owned}) + "\n")
    if "id" not in request:
        continue
    result = {}
    after = None
    if method == "initialize":
        result = {"userAgent": "stateful-goal-fixture"}
    elif method == "account/read":
        result = {"account": {"type": "apiKey"}, "requiresOpenaiAuth": False}
    elif method == "model/list":
        result = {"data": [], "nextCursor": None}
    elif method in ("thread/start", "thread/resume", "thread/read"):
        result = {"thread": {"id": thread, "name": name_path.read_text() if name_path.exists() else "[LUNA] New task", "cwd": params.get("cwd", str(base)),
                              "path": str(base / "rollout.jsonl"), "modelProvider": "openai",
                              "createdAt": 100, "updatedAt": 200, "status": {"type": "idle"}, "turns": []}}
        if method in ("thread/start", "thread/resume"):
            owned = "LUNA" in str(params.get("developerInstructions", ""))
            goal = state().get("goal")
            if method == "thread/resume" and owned and goal and goal["status"] == "active":
                if state().get("rejectAttach"):
                    emit({"id": request["id"], "error": {"code": -32000, "message": "fixture refused owned attachment"}})
                    continue
                active_turn = "resumed-owned-goal"
                if not state().get("noTurnOnAttach"):
                    after = "resume"
    elif method == "thread/list":
        result = {"data": [], "nextCursor": None}
    elif method == "thread/name/set":
        name_path.write_text(params.get("name", "[LUNA] New task"))
    elif method == "thread/turns/list":
        result = {"data": [], "nextCursor": None}
    elif method == "thread/goal/get":
        result = {"goal": state().get("goal")}
    elif method == "thread/goal/set":
        current = state()
        goal = current.get("goal")
        if not goal:
            goal = {"threadId": thread, "objective": params.get("objective", "saved objective"),
                    "status": "paused", "tokenBudget": 9000, "tokensUsed": 321,
                    "timeUsedSeconds": 17, "createdAt": 100, "updatedAt": 200}
        for field in ("objective", "status", "tokenBudget"):
            if field in params:
                goal[field] = params[field]
        current["goal"] = goal
        save(current)
        if not owned and current.get("gateMetadataSet"):
            (base / "metadata-set-waiting").touch()
            deadline = time.monotonic() + 10
            while not (base / "metadata-set-release").exists():
                if time.monotonic() > deadline:
                    raise RuntimeError("fixture metadata set was not released")
                time.sleep(0.01)
        result = {"goal": goal}
        after = "goal"
    elif method == "thread/goal/clear":
        save({"goal": None})
        after = "clear"
    elif method == "turn/start":
        active_turn = "initial-human-turn"
        result = {"turn": {"id": active_turn, "status": "inProgress", "items": []}}
        after = "human"
    elif method == "turn/interrupt":
        after = "interrupt"
    elif method == "turn/steer":
        result = {"turnId": active_turn}
        after = "steer"
    emit({"id": request["id"], "result": result})
    if after in ("resume", "human"):
        notify("turn/started", {"threadId": thread, "turn": {"id": active_turn, "status": "inProgress"}})
        text = "OWNED_GOAL_CONTINUATION" if after == "resume" else "INITIAL_GOAL_RUNTIME"
        notify("item/agentMessage/delta", {"threadId": thread, "turnId": active_turn,
                                          "itemId": "answer-" + active_turn, "delta": text})
        if after == "human":
            notify("turn/completed", {"threadId": thread, "turn": {"id": active_turn, "status": "completed"}})
        elif state().get("completeOnAttach"):
            current = state()
            current["goal"]["status"] = "complete"
            save(current)
            notify("thread/goal/updated", {"threadId": thread, "turnId": active_turn, "goal": current["goal"]})
            notify("turn/completed", {"threadId": thread, "turn": {"id": active_turn, "status": "completed"}})
    elif after == "interrupt":
        notify("turn/completed", {"threadId": thread, "turn": {"id": active_turn or "resumed-owned-goal", "status": "interrupted"}})
    elif after == "steer":
        client_id = params.get("clientUserMessageId")
        notify("item/started", {"threadId": thread, "turnId": active_turn,
                                "item": {"id": "confirmed-steer", "type": "userMessage", "clientId": client_id, "content": []}})
        notify("item/agentMessage/delta", {"threadId": thread, "turnId": active_turn,
                                          "itemId": "steered-answer", "delta": "STEERED_OWNED_GOAL"})
    elif after == "goal":
        notify("thread/goal/updated", {"threadId": thread, "turnId": None, "goal": result["goal"]})
    elif after == "clear":
        notify("thread/goal/cleared", {"threadId": thread})
