#!/usr/bin/env python3
"""Protocol peer for the MCP startup isolation subprocess E2E.

It records only the launch policy, never arbitrary argv or headers. A separate
real Codex probe with a marker MCP proves the CLI's override merge behavior.
"""

import json
import os
from pathlib import Path
import sys


IS_CLAUDE = "claude" in Path(sys.argv[0]).name
ARGS = sys.argv[1:]
HOME = Path(os.environ["CLAUDE_CONFIG_DIR" if IS_CLAUDE else "CODEX_HOME"])
LOG = HOME / "mcp-isolation-launches.jsonl"


def value(flag):
    try:
        return ARGS[ARGS.index(flag) + 1]
    except (ValueError, IndexError):
        return None


def launch_report():
    report = {"provider": "claude" if IS_CLAUDE else "codex", "qa": os.environ.get("LUNA_QA_MCP_ISOLATION") == "1"}
    if IS_CLAUDE:
        report["strict"] = "--strict-mcp-config" in ARGS
        raw = value("--mcp-config")
        report["mcp_servers"] = sorted(json.loads(raw).get("mcpServers", {})) if raw else None
    else:
        report["disabled"] = sorted(arg for arg in ARGS if arg.startswith("mcp_servers.") and arg.endswith(".enabled=false"))
        report["luna_enabled"] = "mcp_servers.luna.enabled=true" in ARGS
    with LOG.open("a") as output:
        output.write(json.dumps(report) + "\n")


def emit(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


launch_report()
for line in sys.stdin:
    try:
        request = json.loads(line)
    except ValueError:
        continue
    if IS_CLAUDE:
        if request.get("type") == "control_request":
            ident = request.get("request_id")
            subtype = request.get("request", {}).get("subtype")
            if subtype == "initialize":
                result = {"models": [{"value": "claude-sonnet-5", "displayName": "Sonnet 5"}], "commands": []}
            elif subtype == "mcp_status":
                result = {"mcpServers": [], "done": True}
            else:
                result = {}
            emit({"type": "control_response", "response": {"request_id": ident, "subtype": "success", "response": result}})
        elif request.get("type") == "user":
            emit({"type": "system", "subtype": "init", "session_id": "isolation-claude", "model": "claude-sonnet-5", "tools": []})
            emit({"type": "result", "subtype": "success", "session_id": "isolation-claude", "result": "ok", "is_error": False})
            break
    else:
        method = request.get("method")
        if "id" not in request:
            continue
        ident = request["id"]
        if method == "config/read":
            result = {"config": {"mcp_servers": {"marker": {"enabled": True}}}}
        elif method == "model/list":
            result = {"data": [], "nextCursor": None}
        elif method == "skills/list":
            result = {"data": []}
        elif method in ("thread/start", "thread/resume"):
            result = {"thread": {"id": "isolation-codex"}}
        elif method == "turn/start":
            result = {"turn": {"id": "isolation-turn"}}
            emit({"id": ident, "result": result})
            emit({"method": "turn/completed", "params": {"threadId": "isolation-codex", "turn": {"id": "isolation-turn", "status": "completed"}}})
            break
        elif method == "thread/goal/get":
            result = {"goal": None}
        else:
            result = {}
        emit({"id": ident, "result": result})
