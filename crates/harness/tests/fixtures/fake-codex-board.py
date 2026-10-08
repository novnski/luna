#!/usr/bin/env python3
"""Deterministic Codex app-server peer that calls LUNA board tools over MCP.

It behaves as the installed Codex 0.159 app-server does with LUNA's
per-process `luna` server (`-c mcp_servers.luna.url=...`, bearer token in
`LUNA_MCP_BEARER_TOKEN`), speaking the same JSON-RPC 2.0 frames as
`fake-codex.sh`:

- answers `initialize`, `thread/start` / `thread/resume` (recording the LUNA
  `developerInstructions`), `thread/turns/list`, `thread/goal/get`,
  `thread/name/set`, `config/read` and `model/list` (one default model, for
  child-model catalogs);
- on `turn/start`, announces the turn, then, for a prompt containing
  `scenario:board-plan:<label>`, connects to the endpoint (initialize,
  tools/list) and runs every call of `.luna-board-plan-<label>.json` (cwd)
  with Codex's `_meta` (`threadId`, `callId`, `x-codex-turn-metadata.turn_id`),
  writing replies to `.luna-board-results-<label>.json`. Plan items may be
  `{"tool", "arguments"}` (arguments may use `"$ref:<step>.<path>"` to reuse a
  field of an earlier JSON reply), `{"relist": true}` (rediscover the
  catalog) or `{"frame": {...}}` or `{"frames": [...]}` (raw notifications; `$THREAD`/`$TURN` are
  substituted, e.g. a mid-turn `contextCompaction` item);
- any other prompt is one plain turn (child chats, workers started by board
  tools); it records its LUNA instructions in `.luna-board-turn-<task>.txt`.

The process exits after its single turn, like `fake-codex.sh`; the next
dispatch resumes the same native thread in a new process with a new endpoint.
Synthetic test evidence only, never real-provider proof.
"""
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

ARGV = sys.argv[1:]
CWD = os.getcwd()
TOKEN = os.environ.get("LUNA_MCP_BEARER_TOKEN", "")
TASK = os.environ.get("LUNA_THREAD_ID", "")
MCP_URL = None
for arg in ARGV:
    if arg.startswith("mcp_servers.luna.url="):
        MCP_URL = arg.split("=", 1)[1].strip('"')

MODELS = {
    "data": [{
        "id": "gpt-board-fixture", "model": "gpt-board-fixture", "displayName": "Board Fixture",
        "description": "Synthetic model for the board tools E2E.", "hidden": False,
        "supportedReasoningEfforts": [{"reasoningEffort": "low"}, {"reasoningEffort": "high"}],
        "additionalSpeedTiers": [], "serviceTiers": [], "defaultServiceTier": None, "isDefault": True,
    }],
    "nextCursor": None,
}


def emit(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def respond(request_id, result):
    emit({"id": request_id, "result": result})


def fail(request_id, message):
    emit({"id": request_id, "error": {"code": -32601, "message": message}})


def mcp(body):
    request = urllib.request.Request(MCP_URL, data=json.dumps(body).encode(), method="POST")
    request.add_header("Authorization", f"Bearer {TOKEN}")
    request.add_header("Content-Type", "application/json")
    request.add_header("Accept", "application/json, text/event-stream")
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            text = response.read().decode()
            return response.status, json.loads(text) if text else None
    except urllib.error.HTTPError as error:
        return error.code, None
    except Exception as error:  # refused / reset
        return 0, {"transportError": str(error)}


def list_tools():
    mcp({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "fake-codex-board", "version": "0"}}})
    _, listed = mcp({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})
    tools = ((listed or {}).get("result") or {}).get("tools", [])
    return {
        "tools": sorted(tool["name"] for tool in tools),
        "readOnly": {tool["name"]: tool.get("annotations", {}).get("readOnlyHint") for tool in tools},
        "schemas": {tool["name"]: tool.get("inputSchema") for tool in tools},
    }


NEXT_ID = [10]


def call(tool, arguments, thread, turn, call_id):
    NEXT_ID[0] += 1
    status, reply = mcp({"jsonrpc": "2.0", "id": NEXT_ID[0], "method": "tools/call", "params": {
        "name": tool, "arguments": arguments,
        "_meta": {"threadId": thread, "callId": call_id, "x-codex-turn-metadata": {"turn_id": turn}},
    }})
    result = (reply or {}).get("result") or {}
    text = "".join(item.get("text", "") for item in result.get("content", []))
    return {"status": status, "isError": result.get("isError"), "text": text}


def resolve(value, results):
    if isinstance(value, str) and value.startswith("$ref:"):
        index, _, path = value[len("$ref:"):].partition(".")
        try:
            node = json.loads(results[index]["text"])
        except (KeyError, TypeError, ValueError):
            return None
        for part in filter(None, path.split(".")):
            if isinstance(node, list):
                node = node[int(part)] if part.isdigit() and int(part) < len(node) else None
            elif isinstance(node, dict):
                node = node.get(part)
            else:
                return None
        return node
    if isinstance(value, list):
        return [resolve(entry, results) for entry in value]
    if isinstance(value, dict):
        return {key: resolve(entry, results) for key, entry in value.items()}
    return value


def substitute(value, thread, turn):
    if isinstance(value, str):
        return value.replace("$THREAD", thread).replace("$TURN", turn)
    if isinstance(value, list):
        return [substitute(entry, thread, turn) for entry in value]
    if isinstance(value, dict):
        return {key: substitute(entry, thread, turn) for key, entry in value.items()}
    return value


def run_plan(label, thread, turn, thread_request, resumed):
    with open(os.path.join(CWD, f".luna-board-plan-{label}.json")) as source:
        steps = json.load(source)
    results = {
        "attachment": {
            "url": MCP_URL, "thread": thread, "resumed": resumed,
            "tokenInEnv": bool(TOKEN), "tokenInArgv": bool(TOKEN) and any(TOKEN in arg for arg in ARGV),
        },
        "instructions": (thread_request or {}).get("developerInstructions"),
        "handshake": list_tools(),
    }
    for index, item in enumerate(steps):
        if "frame" in item or "frames" in item:
            frames = item.get("frames") or [item["frame"]]
            for frame in frames:
                emit(substitute(frame, thread, turn))
            time.sleep(0.2)  # let the adapter read the frames before the next call
            results[str(index)] = {"frames": [frame.get("method") for frame in frames]}
            continue
        if item.get("relist"):
            results[str(index)] = list_tools()
            continue
        arguments = resolve(item.get("arguments", {}), results)
        results[str(index)] = call(item["tool"], arguments, thread, turn, f"call-{label}-{index}")
    temporary = os.path.join(CWD, f".luna-board-results-{label}.json.tmp")
    with open(temporary, "w") as out:
        json.dump(results, out, indent=1)
    os.replace(temporary, os.path.join(CWD, f".luna-board-results-{label}.json"))


def main():
    thread = None
    thread_request = None
    resumed = False
    while True:
        line = sys.stdin.readline()
        if not line:
            return
        message = json.loads(line)
        method = message.get("method")
        request_id = message.get("id")
        params = message.get("params") or {}
        if request_id is None:
            continue  # notifications (initialized) need no reply
        if method == "initialize":
            respond(request_id, {"userAgent": "fake-codex-board"})
        elif method == "thread/start":
            thread = f"th-{TASK or 'ephemeral'}"
            thread_request = params
            respond(request_id, {"thread": {"id": thread}})
        elif method == "thread/resume":
            thread = params.get("threadId") or f"th-{TASK}"
            thread_request = params
            resumed = True
            respond(request_id, {"thread": {"id": thread}})
        elif method == "thread/turns/list":
            respond(request_id, {"data": [], "nextCursor": None, "backwardsCursor": None})
        elif method == "thread/goal/get":
            respond(request_id, {"goal": None})
        elif method in ("thread/name/set", "thread/goal/clear", "turn/interrupt"):
            respond(request_id, {})
        elif method == "config/read":
            respond(request_id, {"config": {"mcp_servers": {}}})
        elif method == "model/list":
            respond(request_id, MODELS)
        elif method == "turn/start":
            label = re.search(r"scenario:board-plan:([A-Za-z0-9_-]+)", json.dumps(params.get("input")))
            turn = f"t-{TASK or 'ephemeral'}-{int(time.time() * 1000)}"
            respond(request_id, {"turn": {"id": turn}})
            emit({"method": "turn/started", "params": {"threadId": thread, "turn": {"id": turn}}})
            # HTTP and stdout are separate channels: let the adapter read the
            # turn start before the first tools/call reaches its fence.
            time.sleep(0.3)
            if label and MCP_URL:
                run_plan(label.group(1), thread, turn, thread_request, resumed)
            elif TASK and not (thread_request or {}).get("ephemeral"):
                with open(os.path.join(CWD, f".luna-board-turn-{TASK}.txt"), "w") as out:
                    out.write((thread_request or {}).get("developerInstructions") or "")
            emit({"method": "item/completed", "params": {"threadId": thread, "turnId": turn, "item": {
                "type": "agentMessage", "id": f"{turn}-msg", "text": "Board fixture turn finished."}}})
            emit({"method": "turn/completed", "params": {"threadId": thread, "turn": {"id": turn, "status": "completed"}}})
            time.sleep(0.2)
            return
        else:
            fail(request_id, f"fake-codex-board: unsupported {method}")


main()
