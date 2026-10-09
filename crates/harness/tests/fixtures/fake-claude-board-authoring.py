#!/usr/bin/env python3
"""Deterministic Claude Code 2.1.286 peer for temporary Board discussions (A6).

It behaves as the installed CLI does for one LUNA turn: the first stdin line
is the user's message, `--resume=<id>` continues a session, the inline
`--mcp-config` names the per-process `luna` endpoint (bearer token resolved
by the entry's `headersHelper`), tool uses are `mcp__luna__<tool>` frames
whose MCP `tools/call` carries `_meta["claudecode/toolUseId"]`, and every
turn starts with `system:init` (reporting the launched permission mode and
cwd) and ends with `result`.

What it does each turn comes from `.board-authoring-plan.json` in the cwd:
`{"turns": [[action, ...], ...]}`, indexed by a per-project turn counter kept
in `.board-authoring-state.json`. Actions:
- `{"read": "relative/path"}`: a built-in Read tool use of a real project
  file (the bytes and SHA-256 are recorded);
- `{"call": tool, "arguments": {...}, "as": label}`: one LUNA tool call;
- `{"pendingCall": tool, "arguments": {...}, "as": label, "delay": s}`: emit
  the tool use, write `.board-authoring-pending`, wait, then call (used to
  close a discussion during an active tool call);
- `{"say": text}`: assistant text; `{"sleep": s}`; `{"crash": true}`: exit
  without a result (a lost response).
Evidence for turn N goes to `.board-authoring-results-N.json` before the
`result` frame. Synthetic test evidence only, never real-provider proof.
"""
import hashlib
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

VERSION = "2.1.286"
ARGV = sys.argv[1:]
TOOL_PREFIX = "mcp__luna__"

if ARGV[:1] == ["--version"]:
    print(f"{VERSION} (Claude Code)")
    sys.exit(0)
if ARGV[:1] == ["agents"]:
    print("[]")
    sys.exit(0)


def flag_value(flag):
    for index, arg in enumerate(ARGV):
        if arg == flag and index + 1 < len(ARGV):
            return ARGV[index + 1]
        if arg.startswith(flag + "="):
            return arg.split("=", 1)[1]
    return None


def emit(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


first = sys.stdin.readline()
if not first:
    sys.exit(1)
try:
    frame = json.loads(first)
except ValueError:
    sys.exit(1)
if frame.get("type") != "user":
    # A discovery probe (initialize control request), not a turn.
    sys.exit(1)
message = frame["message"]["content"]
# What the turn carried besides text (image blocks from attachments).
CONTENT_TYPES = (
    [block.get("type") for block in message if isinstance(block, dict)]
    if isinstance(message, list)
    else ["text"]
)
if isinstance(message, list):
    message = " ".join(block.get("text", "") for block in message if isinstance(block, dict))

RESUME = flag_value("--resume")
SESSION = RESUME or str(uuid.uuid4())
CWD = os.getcwd()
MODE = flag_value("--permission-mode") or "default"
config_raw = flag_value("--mcp-config")
server = json.loads(config_raw)["mcpServers"].get("luna") if config_raw else None
URL = server.get("url") if server else None
MODEL = "claude-haiku-4-5-20251001"


def auth_headers():
    if server is None:
        return {}
    if "headersHelper" in server:
        out = subprocess.run(
            server["headersHelper"], shell=True, capture_output=True, text=True, timeout=10
        )
        return json.loads(out.stdout)
    return {
        key: re.sub(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}", lambda m: os.environ.get(m.group(1), ""), value)
        for key, value in (server.get("headers") or {}).items()
    }


def http(method, body=None, headers=None, timeout=120):
    if URL is None:
        return 0, {"transportError": "no luna endpoint attached"}
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(URL, data=data, method=method)
    request.add_header("Accept", "application/json, text/event-stream" if data else "text/event-stream")
    request.add_header("User-Agent", f"claude-code/{VERSION} (sdk-cli)")
    if data is not None:
        request.add_header("Content-Type", "application/json")
    for key, value in (auth_headers() if headers is None else headers).items():
        request.add_header(key, value)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            text = response.read().decode()
            return response.status, json.loads(text) if text else None
    except urllib.error.HTTPError as error:
        return error.code, None
    except Exception as error:  # refused / reset
        return 0, {"transportError": str(error)}


CLIENT_INFO = {"name": "claude-code", "title": "Claude Code", "version": VERSION}
HEADERS = {}


def handshake():
    auth = auth_headers()
    init = http("POST", {"method": "initialize", "params": {
        "protocolVersion": "2025-11-25",
        "capabilities": {"roots": {"listChanged": True}, "elicitation": {}},
        "clientInfo": CLIENT_INFO,
    }, "jsonrpc": "2.0", "id": 0}, headers=auth)
    version = ((init[1] or {}).get("result") or {}).get("protocolVersion", "")
    HEADERS.update({**auth, "mcp-protocol-version": version})
    http("POST", {"jsonrpc": "2.0", "method": "notifications/initialized"}, headers=HEADERS)
    listed = http("POST", {"method": "tools/list", "jsonrpc": "2.0", "id": 1}, headers=HEADERS)
    tools = ((listed[1] or {}).get("result") or {}).get("tools", [])
    return sorted(tool["name"] for tool in tools)


NEXT_RPC_ID = [10]


def call(tool, arguments, tool_use_id):
    NEXT_RPC_ID[0] += 1
    params = {"name": tool, "arguments": arguments, "_meta": {"claudecode/toolUseId": tool_use_id}}
    status, reply = http(
        "POST", {"method": "tools/call", "params": params, "jsonrpc": "2.0", "id": NEXT_RPC_ID[0]},
        headers=HEADERS,
    )
    result = (reply or {}).get("result") or {}
    text = "".join(item.get("text", "") for item in result.get("content", []))
    outcome = {"status": status, "isError": result.get("isError"), "text": text}
    if reply and "transportError" in reply:
        outcome["transportError"] = reply["transportError"]
    try:
        outcome["json"] = json.loads(text)
    except (ValueError, TypeError):
        pass
    return outcome


def assistant(content):
    emit({
        "type": "assistant",
        "message": {
            "model": MODEL, "id": "msg_" + uuid.uuid4().hex[:24], "type": "message", "role": "assistant",
            "content": content, "stop_reason": None, "stop_sequence": None,
            "usage": {"input_tokens": 1, "output_tokens": 1},
        },
        "parent_tool_use_id": None, "session_id": SESSION, "uuid": str(uuid.uuid4()),
    })


def tool_use(tool_use_id, name, arguments):
    assistant([{"type": "tool_use", "id": tool_use_id, "name": name, "input": arguments,
                "caller": {"type": "direct"}}])


def tool_result(tool_use_id, text, is_error=False):
    content = [{"type": "text", "text": text}]
    emit({
        "type": "user",
        "message": {"role": "user", "content": [{"tool_use_id": tool_use_id, "type": "tool_result",
                                                 "content": content, "is_error": bool(is_error)}]},
        "parent_tool_use_id": None, "session_id": SESSION, "uuid": str(uuid.uuid4()),
        "tool_use_result": content,
    })


def result_frame(text):
    emit({
        "type": "result", "subtype": "success", "is_error": False, "duration_ms": 10, "num_turns": 1,
        "result": text, "stop_reason": "end_turn", "session_id": SESSION, "total_cost_usd": 0,
        "usage": {"input_tokens": 1, "output_tokens": 1}, "modelUsage": {}, "permission_denials": [],
        "uuid": str(uuid.uuid4()),
    })


def write_json(name, value):
    path = os.path.join(CWD, name)
    with open(path + ".tmp", "w") as out:
        json.dump(value, out, indent=1)
    os.replace(path + ".tmp", path)


def next_turn():
    path = os.path.join(CWD, ".board-authoring-state.json")
    try:
        with open(path) as source:
            state = json.load(source)
    except (OSError, ValueError):
        state = {"turns": 0}
    turn = state["turns"]
    state["turns"] = turn + 1
    write_json(".board-authoring-state.json", state)
    return turn


TURN = next_turn()
try:
    with open(os.path.join(CWD, ".board-authoring-plan.json")) as source:
        PLAN = json.load(source).get("turns", [])
except (OSError, ValueError):
    PLAN = []
ACTIONS = PLAN[TURN] if TURN < len(PLAN) else [{"say": "Noted."}]

evidence = {
    "turn": TURN, "session": SESSION, "resume": RESUME, "cwd": CWD, "permissionMode": MODE,
    "model": flag_value("--model"), "prompt": message, "systemPrompt": flag_value("--append-system-prompt"),
    "mcpUrl": URL, "pid": os.getpid(), "env": {
        key: os.environ.get(key) for key in ("LUNA_THREAD_ID", "LUNA_PROJECT_ID")
    },
    "steps": {},
    "contentTypes": CONTENT_TYPES,
}
evidence["tools"] = handshake() if URL else []
emit({
    "type": "system", "subtype": "init", "cwd": CWD, "session_id": SESSION,
    "tools": ["Read", "Grep"] + [TOOL_PREFIX + name for name in evidence["tools"]],
    "mcp_servers": [{"name": "luna", "status": "connected"}] if URL else [],
    "model": MODEL, "permissionMode": MODE, "apiKeySource": "none",
    "claude_code_version": VERSION, "output_style": "default", "uuid": str(uuid.uuid4()),
})

final = "Done."
for index, action in enumerate(ACTIONS):
    tool_use_id = f"toolu_{TURN}_{index}"
    if "read" in action:
        path = os.path.join(CWD, action["read"])
        tool_use(tool_use_id, "Read", {"file_path": path})
        with open(path, "rb") as source:
            data = source.read()
        tool_result(tool_use_id, data.decode(errors="replace"))
        evidence["steps"][action.get("as", f"read{index}")] = {
            "path": path, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(),
        }
    elif "call" in action:
        tool_use(tool_use_id, TOOL_PREFIX + action["call"], action.get("arguments", {}))
        outcome = call(action["call"], action.get("arguments", {}), tool_use_id)
        tool_result(tool_use_id, outcome["text"], outcome.get("isError"))
        evidence["steps"][action.get("as", f"call{index}")] = outcome
    elif "pendingCall" in action:
        tool_use(tool_use_id, TOOL_PREFIX + action["pendingCall"], action.get("arguments", {}))
        write_json(".board-authoring-pending", {"pid": os.getpid(), "mcpUrl": URL, "turn": TURN})
        time.sleep(action.get("delay", 3))
        outcome = call(action["pendingCall"], action.get("arguments", {}), tool_use_id)
        evidence["steps"][action.get("as", f"pending{index}")] = outcome
        write_json(f".board-authoring-late-{TURN}.json", outcome)
        tool_result(tool_use_id, outcome["text"], outcome.get("isError"))
    elif "say" in action:
        final = action["say"]
        # As the CLI streams it (--include-partial-messages): the text
        # arrives as content_block_delta frames, then the message snapshot.
        # LUNA builds assistant text from the deltas only.
        emit({
            "type": "stream_event", "parent_tool_use_id": None, "session_id": SESSION,
            "uuid": str(uuid.uuid4()),
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": final}},
        })
        assistant([{"type": "text", "text": final}])
    elif "sleep" in action:
        time.sleep(action["sleep"])
    elif action.get("crash"):
        write_json(f".board-authoring-results-{TURN}.json", evidence)
        sys.exit(3)

write_json(f".board-authoring-results-{TURN}.json", evidence)
result_frame(final)
