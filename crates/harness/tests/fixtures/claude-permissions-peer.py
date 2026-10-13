#!/usr/bin/env python3
"""Deterministic fake Claude Code CLI for CC-03 permissions/dialogs E2E.

Frame shapes follow the SDK control schema embedded in the installed Claude
Code 2.1.286 binary (`can_use_tool`, `elicitation`, `request_user_dialog`,
`control_cancel_request`, `control_response`, `system/init|status`), kept in
`evidence/cc03-permissions/probes/schema-excerpts.txt`.

The first stdin line is the user turn `scenario:<name> [log:<path>]`. The peer
plays the scenario, records every host line, verifies the exact native
responses and ends with a `result` frame: `peer-verified:<name>` on success,
otherwise an `error_during_execution` result listing each failed check.
"""

import json
import os
import select
import sys
import time

ARGS = sys.argv[1:]
FAILS = []
RECEIVED = []
RESPONSES = {}
INTERRUPTS = []
BUF = b""


def arg_after(flag):
    for index, value in enumerate(ARGS):
        if value == flag and index + 1 < len(ARGS):
            return ARGS[index + 1]
    return None


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def read_line(timeout):
    """One stdin line, None on timeout, "" on EOF."""
    global BUF
    deadline = time.monotonic() + timeout
    while b"\n" not in BUF:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return None
        ready, _, _ = select.select([0], [], [], remaining)
        if not ready:
            return None
        chunk = os.read(0, 65536)
        if not chunk:
            if BUF:
                line, BUF = BUF, b""
                return line.decode()
            return ""
        BUF += chunk
    line, BUF = BUF.split(b"\n", 1)
    return line.decode()


def record(line):
    try:
        obj = json.loads(line)
    except ValueError:
        FAILS.append("non-JSON host line: " + line[:200])
        return
    RECEIVED.append(obj)
    if obj.get("type") == "control_response":
        response = obj.get("response", {})
        RESPONSES.setdefault(response.get("request_id"), []).append(response)
    elif obj.get("type") == "control_request" and obj.get("request", {}).get("subtype") == "interrupt":
        INTERRUPTS.append(obj)


def pump(seconds):
    deadline = time.monotonic() + seconds
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return
        line = read_line(remaining)
        if line is None or line == "":
            return
        record(line)


def wait_response(rid, timeout=8.0):
    deadline = time.monotonic() + timeout
    while not RESPONSES.get(rid):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return None
        line = read_line(remaining)
        if line is None or line == "":
            return None
        record(line)
    return RESPONSES[rid][0]


def wait_interrupt(timeout=8.0):
    deadline = time.monotonic() + timeout
    while not INTERRUPTS:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        line = read_line(remaining)
        if line is None or line == "":
            return False
        record(line)
    return True


def check(condition, message):
    if not condition:
        FAILS.append(message)


def body(response):
    return (response or {}).get("response") or {}


def control(rid, request):
    emit({"type": "control_request", "request_id": rid, "request": request})


def can_use_tool(rid, tool, tool_input, tool_use_id, **extra):
    request = {
        "subtype": "can_use_tool",
        "tool_name": tool,
        "display_name": extra.pop("display_name", tool),
        "input": tool_input,
        "tool_use_id": tool_use_id,
    }
    request.update(extra)
    control(rid, request)


def cancel(rid):
    emit({"type": "control_cancel_request", "request_id": rid})


def expect_success(rid, label, timeout=8.0):
    response = wait_response(rid, timeout)
    check(response is not None, f"{label}: no control_response for {rid}")
    if response is not None:
        check(response.get("subtype") == "success", f"{label}: subtype {response.get('subtype')}")
    return body(response)


def expect_silence(rid, label, seconds=1.0):
    pump(seconds)
    check(rid not in RESPONSES, f"{label}: host answered withdrawn/undeclared {rid}: {RESPONSES.get(rid)}")


def text_turn(session, text):
    emit({"type": "stream_event", "parent_tool_use_id": None,
          "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": text}}})
    emit({"type": "assistant", "parent_tool_use_id": None,
          "message": {"content": [{"type": "text", "text": text}]}})


def write_log(name, log_path):
    if log_path:
        with open(log_path, "w") as handle:
            json.dump({"scenario": name, "received": RECEIVED, "fails": FAILS}, handle, indent=2)


def finish(name, session, log_path):
    write_log(name, log_path)
    if FAILS:
        emit({"type": "result", "subtype": "error_during_execution", "result": "peer-failed:" + name,
              "errors": FAILS, "usage": {"input_tokens": 1, "output_tokens": 1}, "session_id": session})
    else:
        emit({"type": "result", "subtype": "success", "result": "peer-verified:" + name, "errors": [],
              "usage": {"input_tokens": 1, "output_tokens": 1}, "session_id": session})


# ── scenarios ────────────────────────────────────────────────────────────

BASH_INPUT = {"command": "npm test -- --watch=false", "description": "Run the test suite"}
EDIT_INPUT = {"file_path": "/etc/hosts", "old_string": "a", "new_string": "b"}


def approval_details(session):
    can_use_tool(
        "cc03-r1", "Bash", BASH_INPUT, "toolu_cc03_bash",
        title="Claude wants to run npm test -- --watch=false",
        description="Run the test suite",
        decision_reason="\u001b[1mThis command requires approval\u001b[0m",
        decision_reason_type="other",
        permission_suggestions=[
            {"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "npm test:*"}],
             "behavior": "allow", "destination": "localSettings"},
            {"type": "setMode", "mode": "acceptEdits", "destination": "session"},
            {"type": "addRules", "rules": [{"toolName": "Bash"}], "behavior": "deny", "destination": "session"},
        ],
    )
    r1 = expect_success("cc03-r1", "allow for session")
    check(r1.get("behavior") == "allow", f"r1 behavior {r1}")
    check(r1.get("updatedInput") == BASH_INPUT, f"r1 updatedInput {r1.get('updatedInput')}")
    check(r1.get("updatedPermissions") == [
        {"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "npm test:*"}],
         "behavior": "allow", "destination": "session"}], f"r1 updatedPermissions {r1.get('updatedPermissions')}")
    check(r1.get("toolUseID") == "toolu_cc03_bash", f"r1 toolUseID {r1.get('toolUseID')}")
    check(r1.get("decisionClassification") == "user_permanent", f"r1 classification {r1}")
    can_use_tool(
        "cc03-r2", "Edit", EDIT_INPUT, "toolu_cc03_edit",
        blocked_path="/etc/hosts",
        decision_reason="Path is outside the allowed working directories",
        decision_reason_type="workingDir",
        permission_suggestions=[{"type": "addDirectories", "directories": ["/etc"], "destination": "session"}],
    )
    r2 = expect_success("cc03-r2", "cancel turn")
    check(r2.get("behavior") == "deny" and r2.get("interrupt") is True, f"r2 {r2}")
    check(r2.get("toolUseID") == "toolu_cc03_edit", f"r2 toolUseID {r2}")
    check("updatedPermissions" not in r2, f"r2 grants on deny {r2}")


def cancel_replay(session):
    request = dict(tool="Bash", tool_input={"command": "rm -rf build"}, tool_use_id="toolu_r3")
    can_use_tool("cc03-r3", **request)
    pump(0.4)
    cancel("cc03-r3")
    expect_silence("cc03-r3", "cancelled request", 1.2)
    can_use_tool("cc03-r3", **request)  # replay after the cancel
    expect_silence("cc03-r3", "replayed request", 0.8)
    can_use_tool("cc03-r4", "Bash", {"command": "echo ok"}, "toolu_r4")
    r4 = expect_success("cc03-r4", "deny after cancel")
    check(r4.get("behavior") == "deny" and "interrupt" not in r4, f"r4 {r4}")
    check(r4.get("decisionClassification") == "user_reject", f"r4 classification {r4}")
    pump(0.3)
    check("cc03-r3" not in RESPONSES, "cc03-r3 answered after all")


def duplicate_concurrent(session):
    request = dict(tool="Bash", tool_input={"command": "make lint"}, tool_use_id="toolu_r5")
    can_use_tool("cc03-r5", **request)
    can_use_tool("cc03-r5", **request)  # live duplicate
    can_use_tool("cc03-r6", "Bash", {"command": "make fmt"}, "toolu_r6")
    r6 = expect_success("cc03-r6", "second concurrent")
    r5 = expect_success("cc03-r5", "first concurrent")
    check(r6.get("behavior") == "allow" and r6.get("updatedInput") == {"command": "make fmt"}, f"r6 {r6}")
    check(r5.get("behavior") == "deny", f"r5 {r5}")
    pump(0.5)
    check(len(RESPONSES.get("cc03-r5", [])) == 1, f"r5 answered {len(RESPONSES.get('cc03-r5', []))} times")
    check(len(RESPONSES.get("cc03-r6", [])) == 1, f"r6 answered {len(RESPONSES.get('cc03-r6', []))} times")


def subagent_mcp(session):
    can_use_tool("cc03-r7", "Bash", {"command": "cargo check"}, "toolu_r7", agent_id="a1b2c3")
    r7 = expect_success("cc03-r7", "sub-agent allow once")
    check(r7.get("behavior") == "allow" and "updatedPermissions" not in r7, f"r7 {r7}")
    check(r7.get("decisionClassification") == "user_temporary", f"r7 classification {r7}")
    can_use_tool(
        "cc03-r8", "mcp__linear__create_issue", {"title": "Bug", "priority": 2}, "toolu_r8",
        display_name="create_issue", mcp_server={"name": "linear", "source": "user"},
    )
    r8 = expect_success("cc03-r8", "mcp deny")
    check(r8.get("behavior") == "deny" and "interrupt" not in r8, f"r8 {r8}")


def full_mode(session):
    check(arg_after("--permission-mode") == "bypassPermissions", f"Full argv {ARGS}")
    can_use_tool("cc03-r9", "ExitPlanMode", {"plan": "do it"}, "toolu_r9", requires_user_interaction=True)
    r9 = expect_success("cc03-r9", "plan exit")
    check(r9.get("behavior") == "deny" and "Plan mode" in r9.get("message", ""), f"r9 {r9}")
    can_use_tool("cc03-r10", "ShowOnboardingRolePicker", {}, "toolu_r10", requires_user_interaction=True)
    r10 = expect_success("cc03-r10", "interactive tool")
    check(r10.get("behavior") == "deny" and "interactive" in r10.get("message", ""), f"r10 {r10}")
    can_use_tool("cc03-r11", "Bash", {"command": "ls"}, "toolu_r11")
    r11 = expect_success("cc03-r11", "full stray ask")
    check(r11.get("behavior") == "allow" and "updatedPermissions" not in r11, f"r11 {r11}")
    check("decisionClassification" not in r11, f"r11 claims a user decision {r11}")
    can_use_tool(
        "cc03-r12", "Bash", {"command": "git push"}, "toolu_r12",
        matched_ask_rule={"source": "userSettings", "tool_name": "Bash", "rule_content": "git push:*"},
    )
    r12 = expect_success("cc03-r12", "ask rule in full")
    check(r12.get("behavior") == "allow", f"r12 {r12}")
    # Full asks about nothing: an MCP tool call and an MCP server's own
    # message-only approval form (Executor's `require_approval` shape).
    can_use_tool("cc03-r24", "mcp__executor__execute", EXECUTOR_INPUT, "toolu_r24",
                 display_name="execute", mcp_server={"name": "executor", "source": "user"})
    r24 = expect_success("cc03-r24", "mcp tool in full")
    check(r24.get("behavior") == "allow" and r24.get("updatedInput") == EXECUTOR_INPUT, f"r24 {r24}")
    check("updatedPermissions" not in r24 and "decisionClassification" not in r24, f"r24 grants {r24}")
    control("cc03-r25", SERVER_APPROVAL)
    r25 = expect_success("cc03-r25", "server approval in full")
    check(r25 == {"action": "accept", "content": {}}, f"r25 {r25}")


EXECUTOR_INPUT = {"code": "return await tools.executor.coreTools.policies.list({})"}
SERVER_APPROVAL = {"subtype": "elicitation", "mcp_server_name": "executor", "mode": "form",
                   "message": "Approve executor.coreTools.policies.list? (matched policy: executor.*)",
                   "requested_schema": {"type": "object", "properties": {}}}


def auto_mode(session):
    """Claude 2.1.293 auto mode escalates to the host what its classifier does
    not decide (`decision_reason_type` and `classifier_approvable` in the
    control schema). LUNA Auto has no person to ask: each is denied with
    Claude's reason, never classified as a user decision, never interrupting
    the turn. A question is content and still reaches the user."""
    check(arg_after("--permission-mode") == "auto", f"Auto argv {ARGS}")
    escalations = [
        ("cc03-a1", "Bash", {"command": "rm -rf ~/Library/Caches"}, "toolu_a1",
         {"decision_reason": "Recursive delete in the home directory", "decision_reason_type": "safetyCheck",
          "classifier_approvable": False,
          "permission_suggestions": [{"type": "addRules", "rules": [{"toolName": "Bash"}],
                                      "behavior": "allow", "destination": "localSettings"}]},
         ["safetyCheck", "Recursive delete in the home directory", "manual approval"]),
        ("cc03-a2", "mcp__executor__execute", EXECUTOR_INPUT, "toolu_a2",
         {"display_name": "execute", "mcp_server": {"name": "executor", "source": "user"},
          "decision_reason": "Auto mode classifier transcript too long", "decision_reason_type": "other"},
         ["execute", "transcript too long"]),
        ("cc03-a3", "Edit", EDIT_INPUT, "toolu_a3",
         {"blocked_path": "/etc/hosts", "decision_reason_type": "workingDir",
          "permission_suggestions": [{"type": "addDirectories", "directories": ["/etc"],
                                      "destination": "session"}]},
         ["workingDir", "/etc/hosts"]),
        ("cc03-a4", "Bash", {"command": "git push --force"}, "toolu_a4",
         {"decision_reason_type": "classifier", "decision_reason": "Auto mode denial limit reached"},
         ["classifier", "denial limit"]),
    ]
    for rid, tool, tool_input, tool_use_id, extra, reasons in escalations:
        can_use_tool(rid, tool, tool_input, tool_use_id, **extra)
        response = expect_success(rid, f"auto escalation {tool}")
        message = response.get("message", "")
        check(response.get("behavior") == "deny", f"{rid} not denied: {response}")
        check(response.get("toolUseID") == tool_use_id, f"{rid} toolUseID {response}")
        for key in ("interrupt", "decisionClassification", "updatedPermissions", "updatedInput"):
            check(key not in response, f"{rid} carries {key}: {response}")
        check("Auto" in message and "Full access" in message, f"{rid} message {message!r}")
        for reason in reasons:
            check(reason in message, f"{rid} message lacks {reason!r}: {message!r}")
    single = {"questions": [{"question": "Proceed?", "header": "Go", "options": [
        {"label": "Yes", "description": ""}, {"label": "No", "description": ""}], "multiSelect": False}]}
    can_use_tool("cc03-a5", "AskUserQuestion", single, "toolu_a5", requires_user_interaction=True)
    a5 = expect_success("cc03-a5", "question in auto")
    check(a5.get("behavior") == "deny" and "dismissed" in a5.get("message", ""), f"a5 {a5}")
    control("cc03-a6", dict(SERVER_APPROVAL))
    a6 = expect_success("cc03-a6", "server approval in auto")
    check(a6 == {"action": "decline"}, f"a6 {a6}")


QUESTIONS = {"questions": [
    {"question": "Which database?", "header": "DB", "multiSelect": False,
     "options": [{"label": "Postgres", "description": "Relational, robust"},
                 {"label": "SQLite", "description": "Embedded file"}]},
    {"question": "Which features?", "header": "Features", "multiSelect": True,
     "options": [{"label": "Auth", "description": "Login"}, {"label": "Search", "description": "Full text"}]},
]}


def ask_user(session):
    can_use_tool("cc03-r13", "AskUserQuestion", QUESTIONS, "toolu_q1", requires_user_interaction=True)
    r13 = expect_success("cc03-r13", "answered questions")
    check(r13.get("behavior") == "allow", f"r13 {r13}")
    answers = (r13.get("updatedInput") or {}).get("answers")
    check(answers == {"Which database?": "SQLite", "Which features?": ["Auth", "Search"]}, f"r13 answers {answers}")
    check((r13.get("updatedInput") or {}).get("questions") == QUESTIONS["questions"], "r13 dropped questions")
    check(r13.get("toolUseID") == "toolu_q1", f"r13 toolUseID {r13}")
    single = {"questions": [{"question": "Proceed?", "header": "Go", "options": [
        {"label": "Yes", "description": ""}, {"label": "No", "description": ""}], "multiSelect": False}]}
    can_use_tool("cc03-r14", "AskUserQuestion", single, "toolu_q2", requires_user_interaction=True)
    r14 = expect_success("cc03-r14", "dismissed question")
    check(r14.get("behavior") == "deny" and "dismissed" in r14.get("message", ""), f"r14 {r14}")


FORM_SCHEMA = {"type": "object", "required": ["name"], "properties": {
    "name": {"type": "string", "title": "Project name", "minLength": 1},
    "visibility": {"type": "string", "enum": ["public", "private"]},
}}
SECRET_URL = "https://auth.example.com/oauth/start?state=SECRET123"


def elicitation(session):
    control("cc03-e1", {"subtype": "elicitation", "mcp_server_name": "tracker",
                        "message": "Configure the new project", "mode": "form", "requested_schema": FORM_SCHEMA})
    e1 = expect_success("cc03-e1", "form accept")
    check(e1 == {"action": "accept", "content": {"name": "luna", "visibility": "private"}}, f"e1 {e1}")
    control("cc03-e2", {"subtype": "elicitation", "mcp_server_name": "tracker", "mode": "url",
                        "message": "Sign in to tracker at " + SECRET_URL, "url": SECRET_URL,
                        "elicitation_id": "el-1"})
    e2 = expect_success("cc03-e2", "url decline")
    check(e2 == {"action": "decline"}, f"e2 {e2}")
    control("cc03-e3", {"subtype": "elicitation", "mcp_server_name": "tracker", "mode": "form",
                        "message": "Log in", "requested_schema": {"type": "object", "properties": {
                            "password": {"type": "string", "title": "Password"}}}})
    e3 = expect_success("cc03-e3", "sensitive form")
    check(e3 == {"action": "decline"}, f"e3 {e3}")
    control("cc03-e4", {"subtype": "elicitation", "mcp_server_name": "tracker", "mode": "form",
                        "message": "Pick a label", "requested_schema": {"type": "object", "properties": {
                            "label": {"type": "string"}}}})
    pump(0.5)
    cancel("cc03-e4")
    expect_silence("cc03-e4", "cancelled elicitation", 1.2)


def unknown_controls(session):
    control("cc03-u1", {"subtype": "luna_future_thing"})
    u1 = wait_response("cc03-u1")
    check(u1 == {"subtype": "error", "request_id": "cc03-u1",
                 "error": "Unsupported control request subtype: luna_future_thing"}, f"u1 {u1}")
    control("cc03-u2", {"subtype": "request_user_dialog", "dialog_kind": "refusal_fallback_prompt",
                        "payload": {"text": "x"}, "tool_use_id": "toolu_u2"})
    expect_silence("cc03-u2", "undeclared dialog", 1.0)
    emit({"type": "control_request", "request": {"subtype": "can_use_tool", "tool_name": "Bash",
                                                 "input": {"command": "id"}, "tool_use_id": "t"}})
    control("cc03-u4", {"subtype": "can_use_tool", "input": {}, "tool_use_id": "toolu_u4"})
    u4 = wait_response("cc03-u4")
    check((u4 or {}).get("subtype") == "error", f"u4 {u4}")
    pump(0.3)
    check(set(RESPONSES) == {"cc03-u1", "cc03-u4"}, f"unexpected responses {sorted(map(str, RESPONSES))}")


def interrupt(session):
    can_use_tool("cc03-r23", "Bash", {"command": "sleep 100"}, "toolu_r23")
    check(wait_interrupt(), "no interrupt control_request after Stop")
    pump(0.3)
    check("cc03-r23" not in RESPONSES, f"pending request answered after Stop: {RESPONSES.get('cc03-r23')}")
    cancel("cc03-r23")
    expect_silence("cc03-r23", "after native cancel", 0.5)


def engine_approval(session):
    can_use_tool(
        "cc03-g1", "Bash", BASH_INPUT, "toolu_g1", description="Run the test suite",
        decision_reason="This command requires approval", decision_reason_type="other",
        permission_suggestions=[{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "npm test:*"}],
                                 "behavior": "allow", "destination": "localSettings"}],
    )
    g1 = expect_success("cc03-g1", "engine allow for session", timeout=20)
    check(g1.get("behavior") == "allow", f"g1 {g1}")
    check(g1.get("updatedPermissions") == [
        {"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "npm test:*"}],
         "behavior": "allow", "destination": "session"}], f"g1 grants {g1}")


def engine_cancel(session):
    can_use_tool("cc03-g2", "Bash", {"command": "rm -rf target"}, "toolu_g2")
    pump(1.0)
    cancel("cc03-g2")
    expect_silence("cc03-g2", "engine cancelled request", 2.0)


SCENARIOS = {
    "approval-details": approval_details,
    "cancel-replay": cancel_replay,
    "duplicate-concurrent": duplicate_concurrent,
    "subagent-mcp": subagent_mcp,
    "full-mode": full_mode,
    "auto-mode": auto_mode,
    "ask-user": ask_user,
    "elicitation": elicitation,
    "unknown-controls": unknown_controls,
    "interrupt": interrupt,
    "engine-approval": engine_approval,
    "engine-cancel": engine_cancel,
}


def main():
    first = read_line(10)
    if not first:
        return 1
    frame = json.loads(first)
    if frame.get("type") == "control_request":
        # Discovery probe: answer initialize, then wait for stdin to close.
        emit({"type": "control_response", "response": {
            "subtype": "success", "request_id": frame.get("request_id"),
            "response": {"commands": [], "models": [], "current_permission_mode": arg_after("--permission-mode")}}})
        while read_line(30) not in ("", None):
            pass
        return 0
    content = frame.get("message", {}).get("content", "")
    if isinstance(content, list):
        content = " ".join(block.get("text", "") for block in content if isinstance(block, dict))
    if (arg_after("--system-prompt") or "").startswith("You generate session titles"):
        emit({"type": "result", "subtype": "success", "result": "Permission Peer", "errors": [],
              "usage": {"input_tokens": 1, "output_tokens": 1}, "session_id": "sess-title"})
        return 0
    words = content.split()
    name = next((w.split(":", 1)[1] for w in words if w.startswith("scenario:")), "")
    log_path = next((w.split(":", 1)[1] for w in words if w.startswith("log:")), None)
    mode = next((w.split(":", 1)[1] for w in words if w.startswith("report-mode:")), None)
    session = "sess-cc03-" + name
    emit({"type": "system", "subtype": "init", "cwd": "/tmp/cc03-peer", "session_id": session,
          "tools": ["Bash", "Edit", "AskUserQuestion"], "mcp_servers": [], "model": "claude-haiku-4-5-20251001",
          "permissionMode": mode or arg_after("--permission-mode") or "default", "slash_commands": [],
          "output_style": "default", "skills": [], "plugins": [], "claude_code_version": "2.1.286",
          "apiKeySource": "none", "uuid": "00000000-0000-4000-8000-000000000001"})
    if name == "mode-weaker":
        check(wait_interrupt(), "weaker native mode did not stop the turn")
        emit({"type": "result", "subtype": "error_during_execution", "result": None, "errors": [],
              "usage": {"input_tokens": 1, "output_tokens": 1}, "session_id": session})
        return 0
    if name == "mode-stricter":
        emit({"type": "system", "subtype": "status", "status": None, "permissionMode": "default",
              "uuid": "00000000-0000-4000-8000-000000000002", "session_id": session})
        pump(0.8)
        check(not INTERRUPTS, "a stricter native mode stopped the turn")
        finish(name, session, log_path)
        return 0
    scenario = SCENARIOS.get(name)
    if scenario is None:
        FAILS.append("unknown scenario " + repr(name))
    else:
        scenario(session)
    if name == "interrupt":
        write_log(name, log_path)
        emit({"type": "result", "subtype": "error_during_execution", "result": None, "errors": FAILS,
              "usage": {"input_tokens": 1, "output_tokens": 1}, "session_id": session})
        return 0
    if name.startswith("engine-"):
        text_turn(session, ("peer-verified:" if not FAILS else "peer-failed:") + name)
    finish(name, session, log_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
