#!/usr/bin/env python3
"""Offline Codex app-server peer: a preview server left running past its turn.

Frames follow a real Codex 0.162.1 app-server driven through a mock Responses
API (issue #22). The agent starts `npm run preview` with `exec_command`; the
command yields while still running, so its `commandExecution` item (with a
`processId`) stays `inProgress` through `turn/completed`. When the process
exits, possibly turns later, Codex sends `item/completed` with the exit code
under the ORIGINAL `turnId`. Ending the app-server ends the process, so this
peer records a SIGTERM in `peer-terminated`.

The test creates `stop-preview` in the working directory to make the preview
process exit (SIGTERM, exit code 143).
"""
import json
import os
import signal
import sys
import threading
import time

THREAD = "background-thread"
TURN = "background-turn"
lock = threading.Lock()


def emit(value):
    with lock:
        print(json.dumps(value), flush=True)


def item(phase, value):
    emit({"method": "item/" + phase, "params": {"threadId": THREAD, "turnId": TURN, "item": value}})


def terminated(_signum, _frame):
    with open("peer-terminated", "w") as marker:
        marker.write("SIGTERM\n")
    os._exit(0)


signal.signal(signal.SIGTERM, terminated)

preview = {
    "type": "commandExecution",
    "id": "preview",
    "command": "/bin/bash -lc 'npm run preview -- --port 5174'",
    "cwd": os.getcwd(),
    "processId": "41001",
    "source": "unifiedExecStartup",
    "status": "inProgress",
    "commandActions": [{"type": "unknown", "command": "npm run preview -- --port 5174"}],
    "aggregatedOutput": None,
    "exitCode": None,
    "durationMs": None,
}


def preview_process():
    while not os.path.exists("stop-preview"):
        time.sleep(0.02)
    item("completed", {**preview, "status": "failed", "exitCode": 143, "durationMs": 1624,
                       "aggregatedOutput": "Local: http://127.0.0.1:5174/\n"})


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    rid = request.get("id")
    if method == "initialize":
        emit({"id": rid, "result": {"userAgent": "offline-background-terminal-fixture"}})
    elif method in ("thread/start", "thread/resume"):
        emit({"id": rid, "result": {"thread": {"id": THREAD}}})
    elif method == "thread/goal/get":
        emit({"id": rid, "result": {"goal": None}})
    elif method == "turn/start":
        emit({"id": rid, "result": {"turn": {"id": TURN}}})
        emit({"method": "turn/started", "params": {"threadId": THREAD, "turn": {"id": TURN}}})
        item("started", preview)
        emit({"method": "item/commandExecution/outputDelta",
              "params": {"threadId": THREAD, "turnId": TURN, "itemId": "preview",
                         "delta": "Local: http://127.0.0.1:5174/\n"}})
        threading.Thread(target=preview_process, daemon=True).start()
        item("completed", {"type": "agentMessage", "id": "answer",
                           "text": "Preview running: http://127.0.0.1:5174/"})
        emit({"method": "turn/completed",
              "params": {"threadId": THREAD, "turn": {"id": TURN, "status": "completed"}}})
    elif method == "turn/interrupt":
        emit({"id": rid, "result": {}})
    elif rid is not None:
        emit({"id": rid, "result": {}})
