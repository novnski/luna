#!/usr/bin/env python3
"""Offline app-server peer; capture actual adapter startup/resume/turn policy."""
import json
from pathlib import Path
import sys

thread_id = "permission-modes-thread"
for line in sys.stdin:
    message = json.loads(line)
    with Path("permission-modes-wire.jsonl").open("a") as receipt:
        receipt.write(json.dumps(message) + "\n")
    method = message.get("method")
    if "id" not in message:
        continue
    result = {}
    if method in ("thread/start", "thread/resume"):
        result = {"thread": {"id": thread_id}}
    elif method == "thread/goal/get":
        result = {"goal": None}
    elif method == "turn/start":
        result = {"turn": {"id": "permission-modes-turn"}}
    elif method != "initialize":
        print(json.dumps({"id": message["id"], "error": {"code": -32601, "message": method}}), flush=True)
        continue
    print(json.dumps({"id": message["id"], "result": result}), flush=True)
    if method == "turn/start":
        print(json.dumps({"method": "turn/completed", "params": {
            "threadId": thread_id, "turn": {"id": "permission-modes-turn", "status": "completed"}
        }}), flush=True)
        break
