#!/usr/bin/env python3
"""Deterministic Claude Code 2.1.286 peer that calls LUNA tools over MCP.

It behaves as the installed CLI does with LUNA's `--mcp-config` entry:
- reads the `luna` server from `--mcp-config`, resolves the Authorization
  header with the entry's `headersHelper` in a shell (or `${VAR}` expansion),
  never from argv;
- opens the server with the 2.1.286 startup sequence captured by the cc07
  init probe (server/discover, initialize 2025-11-25, notifications/initialized,
  GET, tools/list);
- emits stream-json frames in the 2.1.286 shapes (system:init per turn, root
  and subagent-tagged `assistant` tool_use blocks named `mcp__luna__<tool>`,
  `user` tool_result, `result`) and sends each `tools/call` with
  `_meta["claudecode/toolUseId"]` = the tool_use id, as the CLI's MCP client
  does.

Scenarios come from the first stdin user line:
- `scenario:luna-fence`: harness-level fence matrix; writes `luna-steps.json`
  into the cwd.
- `scenario:luna-plan:<label>`: engine E2E; runs the calls listed in
  `.luna-claude-plan-<label>.json` (cwd) and writes their replies to
  `.luna-claude-results-<label>.json`, then ends the turn and exits.
- anything else: one plain turn (used by child chats started by create_thread).
Synthetic test evidence only, never real-provider proof.
"""
import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
import uuid

VERSION = "2.1.286"
ARGV = sys.argv[1:]
TOOL_PREFIX = "mcp__luna__"


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
prompt = json.loads(first)["message"]["content"]
if isinstance(prompt, list):
    prompt = " ".join(block.get("text", "") for block in prompt if isinstance(block, dict))

resume = next((arg.split("=", 1)[1] for arg in ARGV if arg.startswith("--resume=")), None)
SESSION = resume or str(uuid.uuid4())
CWD = os.getcwd()

config_raw = flag_value("--mcp-config")
server = json.loads(config_raw)["mcpServers"].get("luna") if config_raw else None
URL = server.get("url") if server else None


def auth_headers():
    """The CLI's header resolution for an HTTP server entry."""
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


CLIENT_INFO = {
    "name": "claude-code",
    "title": "Claude Code",
    "version": VERSION,
    "description": "Anthropic's agentic coding tool",
    "websiteUrl": "https://claude.com/claude-code",
}


def handshake():
    auth = auth_headers()
    discover = http(
        "POST",
        {"jsonrpc": "2.0", "id": "server-discover-probe-1", "method": "server/discover", "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientInfo": CLIENT_INFO,
            "io.modelcontextprotocol/clientCapabilities": {"roots": {"listChanged": True}, "elicitation": {"form": {}, "url": {}}},
        }}},
        headers={**auth, "mcp-method": "server/discover", "mcp-protocol-version": "2026-07-28"},
    )
    init = http("POST", {"method": "initialize", "params": {
        "protocolVersion": "2025-11-25",
        "capabilities": {"roots": {"listChanged": True}, "elicitation": {}},
        "clientInfo": CLIENT_INFO,
    }, "jsonrpc": "2.0", "id": 0}, headers=auth)
    version = ((init[1] or {}).get("result") or {}).get("protocolVersion", "")
    with_version = {**auth, "mcp-protocol-version": version}
    initialized = http("POST", {"jsonrpc": "2.0", "method": "notifications/initialized"}, headers=with_version)
    sse = http("GET", headers=with_version)
    listed = http("POST", {"method": "tools/list", "jsonrpc": "2.0", "id": 1}, headers=with_version)
    return {
        "discover": [discover[0], ((discover[1] or {}).get("error") or {}).get("code")],
        "initialize": [init[0], version, ((init[1] or {}).get("result") or {}).get("serverInfo", {}).get("name")],
        "initialized": initialized[0],
        "sse": sse[0],
        "tools": sorted(tool["name"] for tool in ((listed[1] or {}).get("result") or {}).get("tools", [])),
        "readOnly": {tool["name"]: tool.get("annotations", {}).get("readOnlyHint") for tool in ((listed[1] or {}).get("result") or {}).get("tools", [])},
    }


NEXT_RPC_ID = [10]


def call(tool, arguments, tool_use_id, meta=True, headers=None):
    NEXT_RPC_ID[0] += 1
    params = {"name": tool, "arguments": arguments}
    if meta:
        params["_meta"] = {"claudecode/toolUseId": tool_use_id}
    status, reply = http(
        "POST", {"method": "tools/call", "params": params, "jsonrpc": "2.0", "id": NEXT_RPC_ID[0]}, headers=headers
    )
    result = (reply or {}).get("result") or {}
    text = "".join(item.get("text", "") for item in result.get("content", []))
    return {"status": status, "isError": result.get("isError"), "text": text}


def init_frame():
    emit({
        "type": "system", "subtype": "init", "cwd": CWD, "session_id": SESSION,
        "tools": ["Read", "Edit"] + [TOOL_PREFIX + name for name in sorted(LISTED_TOOLS)],
        "mcp_servers": [{"name": "luna", "status": "connected"}] if server else [],
        "model": "claude-haiku-4-5-20251001", "permissionMode": "acceptEdits",
        "apiKeySource": "none", "claude_code_version": VERSION, "output_style": "default",
        "uuid": str(uuid.uuid4()),
    })


def tool_use(tool_use_id, name, arguments, parent=None):
    emit({
        "type": "assistant",
        "message": {
            "model": "claude-haiku-4-5-20251001", "id": "msg_" + uuid.uuid4().hex[:24], "type": "message",
            "role": "assistant",
            "content": [{"type": "tool_use", "id": tool_use_id, "name": name, "input": arguments, "caller": {"type": "direct"}}],
            "stop_reason": None, "stop_sequence": None,
            "usage": {"input_tokens": 1, "output_tokens": 1}, "context_management": None,
        },
        "parent_tool_use_id": parent, "session_id": SESSION, "uuid": str(uuid.uuid4()),
    })


def tool_result(tool_use_id, outcome, parent=None):
    content = [{"type": "text", "text": outcome["text"]}]
    emit({
        "type": "user",
        "message": {"role": "user", "content": [{"tool_use_id": tool_use_id, "type": "tool_result", "content": content, "is_error": bool(outcome["isError"])}]},
        "parent_tool_use_id": parent, "session_id": SESSION, "uuid": str(uuid.uuid4()),
        "tool_use_result": content,
    })


def assistant_text(text):
    emit({
        "type": "assistant",
        "message": {"model": "claude-haiku-4-5-20251001", "id": "msg_" + uuid.uuid4().hex[:24], "type": "message",
                    "role": "assistant", "content": [{"type": "text", "text": text}], "stop_reason": None,
                    "stop_sequence": None, "usage": {"input_tokens": 1, "output_tokens": 1}},
        "parent_tool_use_id": None, "session_id": SESSION, "uuid": str(uuid.uuid4()),
    })


def result_frame(text="DONE"):
    emit({
        "type": "result", "subtype": "success", "is_error": False, "duration_ms": 10, "num_turns": 1,
        "result": text, "stop_reason": "end_turn", "session_id": SESSION, "total_cost_usd": 0,
        "usage": {"input_tokens": 1, "output_tokens": 1}, "modelUsage": {}, "permission_denials": [],
        "uuid": str(uuid.uuid4()),
    })


LISTED_TOOLS = []


def attachment_evidence():
    """What LUNA put on the command line and in the environment."""
    token = os.environ.get("LUNA_MCP_BEARER_TOKEN", "")
    return {
        "servers": sorted(json.loads(config_raw)["mcpServers"]) if config_raw else [],
        "url": URL,
        "allowedTools": flag_value("--allowedTools"),
        "strict": "--strict-mcp-config" in ARGV,
        "headersHelper": bool(server and "headersHelper" in server),
        "tokenInEnv": bool(token),
        "tokenInArgv": bool(token) and any(token in arg for arg in ARGV),
        "resume": resume,
        "session": SESSION,
    }


def fence_matrix():
    steps = {"attachment": attachment_evidence()}
    hs = handshake()
    LISTED_TOOLS.extend(hs["tools"])
    steps["handshake"] = hs
    steps["wrongToken"] = call("luna_status", {}, "toolu_wrong", headers={"Authorization": "Bearer not-the-token"})["status"]

    init_frame()  # turn 1
    tool_use("toolu_A", TOOL_PREFIX + "luna_status", {})
    steps["root"] = call("luna_status", {}, "toolu_A")
    steps["replay"] = call("luna_status", {}, "toolu_A")
    steps["missingMeta"] = call("luna_status", {}, "", meta=False)
    tool_use("toolu_B", TOOL_PREFIX + "fork_thread", {})
    steps["unattached"] = call("fork_thread", {}, "toolu_B")
    tool_use("toolu_C", TOOL_PREFIX + "luna_status", {})
    steps["nameMismatch"] = call("list_threads", {}, "toolu_C")
    tool_use("toolu_S", TOOL_PREFIX + "luna_status", {}, parent="toolu_agent")
    started = time.monotonic()
    steps["subagent"] = call("luna_status", {}, "toolu_S")
    steps["subagent"]["seconds"] = round(time.monotonic() - started, 2)

    # The CLI's HTTP request can reach LUNA before its stdout frame is read.
    raced = {}

    def race():
        raced.update(call("luna_status", {}, "toolu_R"))

    worker = threading.Thread(target=race)
    worker.start()
    time.sleep(0.4)
    tool_use("toolu_R", TOOL_PREFIX + "luna_status", {})
    worker.join(30)
    steps["raced"] = raced

    started = time.monotonic()
    steps["forged"] = call("luna_status", {}, "toolu_forged")
    steps["forged"]["seconds"] = round(time.monotonic() - started, 2)

    # A policy prompt for an attached LUNA tool is answered without a card.
    emit({"type": "control_request", "request_id": "perm-luna", "request": {
        "subtype": "can_use_tool", "tool_name": TOOL_PREFIX + "luna_status", "input": {},
        "tool_use_id": "toolu_P"}})
    steps["preapproved"] = json.loads(sys.stdin.readline())

    tool_use("toolu_L", TOOL_PREFIX + "luna_status", {})
    tool_result("toolu_A", steps["root"])
    result_frame()  # turn 1 ends
    time.sleep(1.0)  # the CLI never calls after `result`; let LUNA read it first
    started = time.monotonic()
    steps["afterResult"] = call("luna_status", {}, "toolu_L")
    steps["afterResult"]["seconds"] = round(time.monotonic() - started, 2)

    init_frame()  # turn 2 (a native wake turn re-emits init)
    tool_use("toolu_W", TOOL_PREFIX + "luna_status", {})
    steps["wake"] = call("luna_status", {}, "toolu_W")
    tool_result("toolu_W", steps["wake"])
    result_frame()
    with open(os.path.join(CWD, "luna-steps.json"), "w") as out:
        json.dump(steps, out, indent=1)


def plan(label):
    plan_path = os.path.join(CWD, f".luna-claude-plan-{label}.json")
    with open(plan_path) as source:
        calls = json.load(source)
    results = {"attachment": attachment_evidence()}
    hs = handshake()
    LISTED_TOOLS.extend(hs["tools"])
    results["handshake"] = hs
    init_frame()
    for index, item in enumerate(calls):
        tool_use_id = f"toolu_{label}_{index}"
        tool_use(tool_use_id, TOOL_PREFIX + item["tool"], item["arguments"])
        outcome = call(item["tool"], item["arguments"], tool_use_id)
        results[str(index)] = outcome
        tool_result(tool_use_id, outcome)
    assistant_text("Plan finished.")
    result_frame("Plan finished.")
    with open(os.path.join(CWD, f".luna-claude-results-{label}.json.tmp"), "w") as out:
        json.dump(results, out, indent=1)
    os.replace(
        os.path.join(CWD, f".luna-claude-results-{label}.json.tmp"),
        os.path.join(CWD, f".luna-claude-results-{label}.json"),
    )


if "scenario:luna-fence" in prompt:
    fence_matrix()
elif "scenario:luna-plan:" in prompt:
    plan(prompt.split("scenario:luna-plan:", 1)[1].split()[0])
else:
    init_frame()
    assistant_text("ok")
    result_frame("ok")
