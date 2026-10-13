#!/usr/bin/env python3
"""Offline app-server peer; capture actual adapter startup/resume/turn policy.

A turn whose prompt contains `approvals` also replays every approval request
shape Codex sends a client (codex-rs `ceea67163f`): command, file-change and
permission approvals; an MCP tool-call approval elicitation
(`_meta.codex_approval_kind: "mcp_tool_call"`, core/src/mcp_tool_call.rs);
an MCP server's own message-only approval form (Executor's
`require_approval` shape, `requestedSchema: {"type":"object","properties":{}}`);
a computer-use approval; and a data form. Under `approvalsReviewer:
"auto_review"` it then sends `item/autoApprovalReview/completed` outcomes
(app-server-protocol v2 item.rs). Each response is checked against the mode's
contract (docs/pending-tasks/permission-modes/README.md); the turn fails
listing every mismatch. No model, provider or GUI is involved.
"""
import json
from pathlib import Path
import sys

THREAD = "permission-modes-thread"
TURN = "permission-modes-turn"
PERMISSIONS = {"network": {"enabled": True}}
COMPUTER_SCHEMA = {"type": "object", "properties": {
    "approval": {"oneOf": [{"const": "once"}, {"const": "always"}]}}}
EMPTY_SCHEMA = {"type": "object", "properties": {}}


def emit(value):
    print(json.dumps(value), flush=True)


def read():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(0)
    message = json.loads(line)
    with Path("permission-modes-wire.jsonl").open("a") as receipt:
        receipt.write(json.dumps(message) + "\n")
    return message


def mode_of(params):
    if params.get("approvalPolicy") == "never":
        return "full"
    return "auto" if params.get("approvalsReviewer") == "auto_review" else "ask"


def request(rid, method, params):
    """Send one server request and wait for the host's response to it."""
    emit({"id": rid, "method": method, "params": dict(params, threadId=THREAD, turnId=TURN)})
    while True:
        message = read()
        if message.get("id") == rid and "method" not in message:
            return message.get("result", {"error": message.get("error")})
        if "id" in message and message.get("method") == "thread/goal/get":
            emit({"id": message["id"], "result": {"goal": None}})
        elif "id" in message and message.get("method"):
            emit({"id": message["id"], "error": {"code": -32601, "message": message["method"]}})


def review(status, action, rationale):
    emit({"method": "item/autoApprovalReview/completed", "params": {
        "threadId": THREAD, "turnId": TURN, "startedAtMs": 1, "completedAtMs": 2,
        "reviewId": f"review-{status}", "targetItemId": None, "decisionSource": "agent",
        "review": {"status": status, "riskLevel": "high", "userAuthorization": "low",
                   "rationale": rationale},
        "action": action}})


def approvals(mode):
    emit({"method": "turn/started", "params": {"threadId": THREAD, "turn": {"id": TURN}}})
    full = mode == "full"
    plan = [
        ("cmd", "item/commandExecution/requestApproval",
         {"itemId": "cmd-item", "command": "touch approved.txt", "cwd": "/tmp",
          "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"]},
         {"decision": "accept"} if full else {"decision": "decline"}),
        ("patch", "item/fileChange/requestApproval",
         {"itemId": "patch-item", "changes": [{"path": "/tmp/approved.txt"}]},
         {"decision": "accept"} if full else {"decision": "decline"}),
        ("perm", "item/permissions/requestApproval",
         {"itemId": "perm-item", "reason": "Fixture network access", "permissions": PERMISSIONS},
         {"permissions": PERMISSIONS, "scope": "turn"} if full
         else {"permissions": {}, "scope": "turn"}),
        ("tool", "mcpServer/elicitation/request",
         {"serverName": "executor", "mode": "form", "requestedSchema": EMPTY_SCHEMA,
          "message": 'Allow the executor MCP server to run tool "execute"?',
          "_meta": {"codex_approval_kind": "mcp_tool_call", "persist": ["session", "always"],
                    "tool_title": "execute"}},
         {"action": "accept", "content": None} if full else {"action": "decline"}),
        ("server-approval", "mcpServer/elicitation/request",
         {"serverName": "executor", "mode": "form", "requestedSchema": EMPTY_SCHEMA,
          "message": "Approve executor.coreTools.policies.list? (matched policy: executor.*)"},
         {"action": "accept", "content": {}} if full else {"action": "decline"}),
        ("computer", "mcpServer/elicitation/request",
         {"serverName": "computer-use", "mode": "form", "requestedSchema": COMPUTER_SCHEMA,
          "message": "Allow ChatGPT to use Finder?"},
         {"action": "accept", "content": {"approval": "once"}} if full
         else {"action": "decline"}),
        # A form asking for values is the user's in every mode; the test
        # declines it.
        ("form", "mcpServer/elicitation/request",
         {"serverName": "tracker", "mode": "form", "message": "Name the new project",
          "requestedSchema": {"type": "object", "required": ["name"], "properties": {
              "name": {"type": "string", "title": "Project name"}}}},
         {"action": "decline"}),
    ]
    responses, failures = {}, []
    for rid, method, params, expected in plan:
        got = request(rid, method, params)
        responses[rid] = got
        if got != expected:
            failures.append(f"{mode} {rid}: expected {expected!r}, received {got!r}")
    if mode == "auto":
        review("denied", {"type": "command", "source": "shell", "command": "rm -rf ~/Documents",
                          "cwd": "/tmp"}, "Deletes user files outside the task.")
        review("timedOut", {"type": "mcpToolCall", "server": "executor", "toolName": "execute",
                            "connectorId": None, "connectorName": None, "toolTitle": "execute"},
               None)
        # Approved and aborted reviews add nothing visible.
        review("approved", {"type": "applyPatch", "cwd": "/tmp", "files": ["/tmp/a"]}, "Fine.")
        review("aborted", {"type": "networkAccess", "target": "https://example.test",
                           "host": "example.test", "protocol": "https", "port": 443}, None)
    Path("permission-modes-approvals.json").write_text(json.dumps(
        {"mode": mode, "responses": responses, "failures": failures}, indent=2))
    return failures


def main():
    while True:
        message = read()
        method = message.get("method")
        if "id" not in message or method is None:
            continue
        if method in ("thread/start", "thread/resume"):
            emit({"id": message["id"], "result": {"thread": {"id": THREAD}}})
        elif method == "thread/goal/get":
            emit({"id": message["id"], "result": {"goal": None}})
        elif method == "turn/start":
            emit({"id": message["id"], "result": {"turn": {"id": TURN}}})
            params = message.get("params", {})
            prompt = " ".join(item.get("text", "") for item in params.get("input", [])
                              if isinstance(item, dict))
            failures = approvals(mode_of(params)) if "approvals" in prompt else []
            turn = {"id": TURN, "status": "failed" if failures else "completed"}
            if failures:
                turn["error"] = {"message": "; ".join(failures)}
            emit({"method": "turn/completed", "params": {"threadId": THREAD, "turn": turn}})
            break
        elif method == "initialize":
            emit({"id": message["id"], "result": {}})
        else:
            emit({"id": message["id"], "error": {"code": -32601, "message": method}})


main()
