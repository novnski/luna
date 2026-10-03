#!/usr/bin/env python3
"""Persistent JSON-RPC peer for native fork integration tests.

Records process starts and protocol calls under the isolated CODEX_HOME. Fork
metadata survives processes so independent readback actually observes disk.
"""
import json
import os
from pathlib import Path
import sys
import time

home = Path(os.environ["CODEX_HOME"])
home.mkdir(parents=True, exist_ok=True)
with (home / "fork-processes").open("a") as f:
    f.write(f"{os.getpid()}\n")

for line in sys.stdin:
    frame = json.loads(line)
    method = frame.get("method")
    if "id" not in frame:
        continue
    params = frame.get("params", {})
    thread = params.get("threadId", "")
    with (home / "fork-trace").open("a") as f:
        f.write(json.dumps({"pid": os.getpid(), "method": method, "thread": thread}) + "\n")
    result = {}
    error = None
    if method == "initialize":
        result = {"userAgent": "fork-peer"}
    elif method == "thread/turns/list":
        if thread == "fork-source-preflight-fail":
            error = {"code": -32603, "message": "injected fork preflight failure"}
        else:
            forked = thread.startswith("forked-")
            prefix = "fork" if forked else "source"
            text = "changed" if thread == "forked-fork-source-mismatch" else "one"
            result = {"data": [{"id": f"{prefix}-turn-1", "status": "completed",
                       "itemsView": "full", "error": None,
                       "items": [{"id": f"{prefix}-item-1", "type": "agentMessage",
                                  "text": text, "payload": {"id": "semantic-id", "time": 17}}]}],
                      "nextCursor": None, "backwardsCursor": None}
    elif method == "thread/fork":
        target = f"forked-{thread}"
        (home / f"thread-{target}.json").write_text(json.dumps({"id": target, "name": "Forked"}))
        with (home / "fork-mutations").open("a") as f:
            f.write(thread + "\n")
        result = {"thread": {"id": target}}
    elif method == "thread/name/set":
        target = home / f"thread-{thread}.json"
        value = json.loads(target.read_text())
        value["name"] = params["name"]
        target.write_text(json.dumps(value))
    elif method == "thread/read":
        if thread == "forked-fork-source-timeout":
            time.sleep(30)
        target = home / f"thread-{thread}.json"
        if target.exists():
            result = {"thread": {**json.loads(target.read_text()), "cwd": "/tmp",
                                 "status": {"type": "idle"}, "ephemeral": False}}
        else:
            error = {"code": -32602, "message": "thread not found"}
    elif method == "thread/list":
        # Baseline permits this; the integration assertion forbids inventory
        # scans in the optimized fork path.
        result = {"data": [], "nextCursor": None}
    else:
        error = {"code": -32601, "message": f"unexpected method {method}"}
    print(json.dumps({"id": frame["id"], **({"error": error} if error else {"result": result})}), flush=True)
