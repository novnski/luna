#!/usr/bin/env python3
"""Deterministic fake Claude Code CLI for the CC-11 MCP control plane.

Frame shapes mirror isolated live captures from CLI 2.1.286 (initialize,
mcp_status, mcp_toggle, mcp_read_resource and their error wordings; see the
cc11 evidence `probes/run-*/`). Behaviour it reproduces:

- `--restricted` loads only `dynamic`-scope servers (P2).
- `mcp_toggle` persists per cwd in the shared state file (P3).
- `mcp_read_resource` is `ui://`-only and strips `com.anthropic/` meta (P4).
- servers marked `slowPolls: N` report `pending` for the first N polls.

Scenario selection never uses the environment: the cwd holds
`.fake-claude-mcp.json` = {"state": "/abs/state.json", "log": "/abs/log.jsonl"}.

Guard rails (each makes the process exit non-zero and logs why):
- any permission-bypass flag;
- a `user` message on a control-only session.
Run mode: when the FIRST stdin line is a user message, emit the state's
`sessionInit` frame and a success result (session evidence for the run path).
"""
import json
import os
import sys

BYPASS = {"--dangerously-skip-permissions", "--allow-dangerously-skip-permissions"}


def load_pointer():
    try:
        with open(os.path.join(os.getcwd(), ".fake-claude-mcp.json")) as handle:
            return json.load(handle)
    except OSError:
        return {}


POINTER = load_pointer()
STATE_PATH = POINTER.get("state")
LOG_PATH = POINTER.get("log")
ARGV = sys.argv[1:]
RESTRICTED = "--restricted" in ARGV


def log(entry):
    if not LOG_PATH:
        return
    entry = dict(entry, argv=ARGV, cwd=os.getcwd(), pid=os.getpid())
    with open(LOG_PATH, "a") as handle:
        handle.write(json.dumps(entry) + "\n")


def load_state():
    with open(STATE_PATH) as handle:
        return json.load(handle)


def save_state(state):
    tmp = STATE_PATH + ".tmp"
    with open(tmp, "w") as handle:
        json.dump(state, handle, indent=2)
    os.replace(tmp, STATE_PATH)


def emit(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def success(request_id, response=None):
    body = {"subtype": "success", "request_id": request_id}
    if response is not None:
        body["response"] = response
    emit({"type": "control_response", "response": body})


def error(request_id, message):
    emit({"type": "control_response", "response": {"subtype": "error", "request_id": request_id, "error": message}})


def bypass_requested():
    if any(arg in BYPASS for arg in ARGV):
        return True
    for index, arg in enumerate(ARGV[:-1]):
        if arg == "--permission-mode" and ARGV[index + 1] == "bypassPermissions":
            return True
    return False


POLLS = {}


def visible_rows(state):
    cwd = os.getcwd()
    disabled = set(state.get("disabled", {}).get(cwd, []))
    rows = []
    for server in state.get("servers", []):
        if RESTRICTED and server.get("scope") != "dynamic":
            continue
        name = server["name"]
        row = {k: v for k, v in server.items() if k not in ("slowPolls", "resources")}
        if name in disabled:
            row = {k: row[k] for k in ("name", "config", "scope", "source") if k in row}
            row["status"] = "disabled"
        else:
            slow = server.get("slowPolls", 0)
            seen = POLLS.get(name, 0)
            POLLS[name] = seen + 1
            if slow < 0 or seen < slow:
                row = {k: row[k] for k in ("name", "config", "scope", "source") if k in row}
                row["status"] = "pending"
        rows.append(row)
    return rows


def find(state, name):
    for server in state.get("servers", []):
        if server["name"] == name and not (RESTRICTED and server.get("scope") != "dynamic"):
            return server
    return None


def initialize_response():
    return {
        "commands": [],
        "agents": [],
        "output_style": "default",
        "available_output_styles": ["default"],
        "models": [],
        "account": {"tokenSource": "none", "apiProvider": "firstParty"},
        "pid": os.getpid(),
        "capabilities": ["ui_surface_v1"],
        "current_permission_mode": "default",
        "analytics_disabled": True,
    }


def handle(state, frame):
    request_id = frame.get("request_id", "")
    request = frame.get("request") or {}
    subtype = request.get("subtype")
    log({"event": "control_request", "subtype": subtype, "request": request})
    if subtype in state.get("unsupported", []):
        error(request_id, f"Unsupported control request subtype: {subtype}")
        return
    if subtype == "initialize":
        success(request_id, initialize_response())
    elif subtype == "mcp_status":
        success(request_id, {"mcpServers": visible_rows(state)})
    elif subtype == "mcp_toggle":
        name = request.get("serverName")
        if find(state, name) is None:
            error(request_id, f"Server not found: {name}")
            return
        cwd = os.getcwd()
        disabled = state.setdefault("disabled", {}).setdefault(cwd, [])
        if request.get("enabled") is False and name not in disabled:
            disabled.append(name)
        if request.get("enabled") is True and name in disabled:
            disabled.remove(name)
        save_state(state)
        if "mcp_toggle" in state.get("dieAfterWrite", []):
            log({"event": "died_after_write", "subtype": subtype})
            sys.exit(9)
        if "mcp_toggle" in state.get("ignoreWrite", []):
            # Accept without effect: the readback must catch the mismatch.
            if request.get("enabled") is False:
                disabled.remove(name)
            save_state(state)
        success(request_id)
    elif subtype == "mcp_read_resource":
        name = request.get("serverName")
        uri = request.get("uri")
        if not isinstance(name, str):
            error(request_id, "mcp_read_resource: serverName must be a string")
            return
        if not isinstance(uri, str):
            error(request_id, "mcp_read_resource: uri must be a string")
            return
        if not uri.startswith("ui://"):
            error(request_id, "mcp_read_resource: uri must use the ui:// scheme")
            return
        server = find(state, name)
        if server is None:
            error(request_id, f"Server not found: {name}")
            return
        if name in state.get("disabled", {}).get(os.getcwd(), []):
            error(request_id, f"MCP server {name} is disabled — enable it (mcp_toggle) before reading")
            return
        if server.get("status") != "connected":
            error(request_id, f"MCP server {name} is not connected (status: {server.get('status')})")
            return
        resource = (server.get("resources") or {}).get(uri)
        if resource is None:
            error(request_id, f'mcp_read_resource: resources/read on "{name}" failed (Resource not found)')
            return
        contents = []
        for item in resource:
            item = dict(item)
            meta = item.get("_meta")
            if isinstance(meta, dict):
                item["_meta"] = {k: v for k, v in meta.items() if not k.startswith("com.anthropic/")}
            contents.append(item)
        success(request_id, {"contents": contents})
    else:
        error(request_id, f"Unsupported control request subtype: {subtype}")


def run_mode(state, first):
    log({"event": "user_message", "mode": "run"})
    init = dict(state.get("sessionInit") or {})
    init.setdefault("type", "system")
    init.setdefault("subtype", "init")
    init.setdefault("cwd", os.getcwd())
    emit(init)
    session = init.get("session_id", "sess-fake-mcp")
    emit({"type": "assistant", "message": {"id": "msg-1", "role": "assistant", "content": [{"type": "text", "text": "ok"}]}, "session_id": session})
    emit({"type": "result", "subtype": "success", "result": "ok", "errors": [], "usage": {"input_tokens": 1, "output_tokens": 1}, "session_id": session})
    # Keep stdin open semantics: exit on EOF.
    for _ in sys.stdin:
        pass


def main():
    if bypass_requested():
        log({"event": "bypass_flag_rejected"})
        sys.exit(3)
    if not STATE_PATH:
        sys.exit(2)
    state = load_state()
    first = True
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            frame = json.loads(raw)
        except json.JSONDecodeError:
            continue
        kind = frame.get("type")
        if kind == "user":
            if first:
                run_mode(state, frame)
                return
            log({"event": "user_message_on_control_session"})
            sys.exit(4)
        first = False
        if kind == "control_request":
            state = load_state()
            handle(state, frame)
    log({"event": "stdin_eof"})


if __name__ == "__main__":
    main()
