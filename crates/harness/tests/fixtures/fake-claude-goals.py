#!/usr/bin/env python3
"""Deterministic Claude Code 2.1.286 peer for CC-14 (goals, schedules) and
CC-15 (ReportFindings). No model, account or network.

Frame shapes:
- Local `/goal` replies, the init frame, the unrecoverable-error informational
  and the transcript `goal_status` rows match a real logged-out probe of the
  installed 2.1.286 CLI (evidence/cc14-goals-review/probes).
- The "not met" synthetic user frame, the met/impossible rows and the tool
  inputs/outputs come from the 2.1.286 bundle's own code and SDK schemas.

Like the real CLI it writes the session transcript to
$CLAUDE_CONFIG_DIR/projects/<cwd with non-alphanumerics as '-'>/<session>.jsonl.
It refuses (exit 9) any stdin line that is not a stream-json user message or
control response, so a Codex method can never pass silently. Every stdin line
is appended to $CLAUDE_CONFIG_DIR/stdin.jsonl for the tests.
"""
import hashlib
import json
import os
import re
import sys
import time
import uuid

CONFIG = os.environ.get("CLAUDE_CONFIG_DIR")
if not CONFIG:
    sys.exit(7)
CWD = os.getcwd()
ARGS = sys.argv[1:]
RESUME = next((a.split("=", 1)[1] for a in ARGS if a.startswith("--resume=")), None)
# These tests run Ask only; a bypass flag here would be a LUNA bug.
if "--dangerously-skip-permissions" in ARGS:
    sys.exit(8)


def now_iso():
    t = time.time()
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(t)) + ".%03dZ" % int((t % 1) * 1000)


def emit(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def receipt(line):
    with open(os.path.join(CONFIG, "stdin.jsonl"), "a") as out:
        out.write(line.rstrip("\n") + "\n")


def read_line():
    line = sys.stdin.readline()
    if not line:
        return None
    receipt(line)
    if "thread/goal" in line or "jsonrpc" in line:
        sys.exit(9)
    try:
        frame = json.loads(line)
    except json.JSONDecodeError:
        sys.exit(9)
    if frame.get("type") == "control_response":
        return frame
    if frame.get("type") != "user":
        sys.exit(9)
    return frame


def text_of(frame):
    content = frame.get("message", {}).get("content", "")
    if isinstance(content, list):
        return "\n".join(b.get("text", "") for b in content if b.get("type") == "text")
    return content


SESSION = RESUME or "c1a0de00-0000-4000-8000-" + hashlib.md5(CWD.encode()).hexdigest()[:12]
PROJECT_DIR = os.path.join(CONFIG, "projects", re.sub(r"[^A-Za-z0-9]", "-", CWD))
TRANSCRIPT = os.path.join(PROJECT_DIR, SESSION + ".jsonl")


def row(attachment):
    os.makedirs(PROJECT_DIR, exist_ok=True)
    with open(TRANSCRIPT, "a") as out:
        out.write(json.dumps({"type": "attachment", "attachment": attachment,
                              "uuid": str(uuid.uuid4()), "timestamp": now_iso(),
                              "sessionId": SESSION}) + "\n")


def goal_rows():
    if not os.path.exists(TRANSCRIPT):
        return []
    rows = []
    with open(TRANSCRIPT) as f:
        for line in f:
            r = json.loads(line)
            if r.get("type") == "attachment" and r["attachment"].get("type") == "goal_status":
                rows.append(r["attachment"])
    return rows


def active_goal():
    rows = goal_rows()
    if not rows:
        return None
    last = rows[-1]
    if last.get("met") or last.get("failed"):
        return None
    return last["condition"]


TOOLS_RESTRICTED = ["AskUserQuestion", "CronDelete", "CronList", "Edit", "Glob", "Grep", "Read",
                    "ReportFindings", "ScheduleWakeup", "Skill", "Task", "TaskStop", "Write"]


def init():
    emit({"type": "system", "subtype": "init", "cwd": CWD, "session_id": SESSION,
          "tools": TOOLS_RESTRICTED + ["CronCreate"], "model": "claude-haiku-4-5-20251001",
          "permissionMode": "acceptEdits", "claude_code_version": "2.1.286",
          "capabilities": ["interrupt_receipt_v1", "msg_lifecycle_v1"], "uuid": str(uuid.uuid4())})


USAGE0 = {"input_tokens": 0, "output_tokens": 0, "cache_creation_input_tokens": 0,
          "cache_read_input_tokens": 0}


def local_reply(text, args):
    emit({"type": "assistant", "message": {"id": str(uuid.uuid4()), "model": "<synthetic>",
          "role": "assistant", "stop_reason": "end_turn", "type": "message", "usage": USAGE0,
          "content": [{"type": "text", "text": text}]}, "parent_tool_use_id": None,
          "local_command_run": {"command": "goal", "args": args},
          "local_command_source": "<local-command-stdout>%s</local-command-stdout>" % text,
          "session_id": SESSION, "uuid": str(uuid.uuid4())})


def local_result(text):
    emit({"type": "result", "subtype": "success", "is_error": False, "num_turns": 0,
          "result": text, "local_command": "goal", "stop_reason": None, "session_id": SESSION,
          "usage": USAGE0, "modelUsage": {}, "uuid": str(uuid.uuid4())})


def say(text):
    emit({"type": "stream_event", "parent_tool_use_id": None,
          "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": text}}})
    emit({"type": "assistant", "parent_tool_use_id": None, "session_id": SESSION,
          "message": {"model": "claude-haiku-4-5-20251001", "role": "assistant",
                      "content": [{"type": "text", "text": text}],
                      "usage": {"input_tokens": 1200, "output_tokens": 30}}})


def feedback(condition, reason):
    row({"type": "goal_status", "met": False, "condition": condition, "reason": reason})
    emit({"type": "user", "message": {"role": "user",
          "content": "Stop hook feedback:\n[%s]: %s" % (condition, reason)},
          "parent_tool_use_id": None, "isSynthetic": True, "session_id": SESSION,
          "uuid": str(uuid.uuid4())})


def model_result(text, turns, is_error=False):
    frame = {"type": "result", "subtype": "success", "is_error": is_error, "num_turns": turns,
             "result": text, "stop_reason": "end_turn", "session_id": SESSION,
             "usage": {"input_tokens": 2400, "output_tokens": 60}, "uuid": str(uuid.uuid4())}
    if is_error:
        frame["terminal_reason"] = "api_error"
    emit(frame)


def goal_command(args):
    args = args.strip()
    if args == "":
        current = active_goal()
        if current is None:
            text = "No goal set. Usage: `/goal <condition>`"
        else:
            checks = sum(1 for r in goal_rows() if not r.get("sentinel") and not r.get("met"))
            text = "Goal active: %s (%s)" % (current, "not yet evaluated" if checks == 0 else
                                            "%d turn%s" % (checks, "" if checks == 1 else "s"))
        local_reply(text, args)
        local_result(text)
        return
    if args.lower() in ("clear", "stop", "off", "reset", "none", "cancel"):
        current = active_goal()
        if current is None:
            text = "No goal set"
        else:
            row({"type": "goal_status", "met": True, "sentinel": True, "condition": current})
            text = "Goal cleared: " + current
        local_reply(text, args)
        local_result(text)
        return
    condition = args
    if "scenario:goal-gate" in condition:
        text = ("/goal can't run while hooks are restricted (disableAllHooks or "
                "allowManagedHooksOnly is set in settings or by policy).")
        local_reply(text, args)
        local_result(text)
        return
    # Accepted: Claude replaces any goal, replies, then runs the loop.
    row({"type": "goal_status", "met": False, "sentinel": True, "condition": condition})
    local_reply("Goal set: " + condition, args)
    emit({"type": "system", "subtype": "status", "status": "requesting", "session_id": SESSION,
          "uuid": str(uuid.uuid4())})
    if "scenario:goal-error" in condition:
        emit({"type": "assistant", "parent_tool_use_id": None, "session_id": SESSION,
              "error": "billing_error", "is_api_error_message": True,
              "message": {"model": "<synthetic>", "role": "assistant",
                          "content": [{"type": "text", "text": "Credit balance is too low"}]}})
        row({"type": "goal_status", "met": True, "sentinel": True, "condition": condition})
        emit({"type": "system", "subtype": "informational", "level": "warning",
              "content": "Goal cleared after an unrecoverable error (credit balance exhausted): "
                         "\"%s\". Run /goal again to continue." % condition,
              "session_id": SESSION, "uuid": str(uuid.uuid4())})
        model_result("Credit balance is too low", 1, is_error=True)
        return
    say("Checking the project. ")
    feedback(condition, "DONE.txt does not exist yet")
    if "scenario:goal-impossible" in condition:
        row({"type": "goal_status", "met": False, "failed": True, "condition": condition,
             "reason": "The project has no write access, so DONE.txt can never be created",
             "iterations": 2, "durationMs": 12000, "tokens": 900})
        model_result("I cannot create the file.", 2)
        return
    if "scenario:goal-stuck" in condition:
        say("Still waiting for the build. ")
        feedback(condition, "The build has not finished")
        model_result("Still waiting.", 2)
        return
    say("Created DONE.txt.")
    met = {"type": "goal_status", "met": True, "condition": condition,
           "reason": "DONE.txt exists in the project root", "iterations": 2,
           "durationMs": 41000, "tokens": 1820}
    if "scenario:goal-late" in condition:
        # Transcript writes are asynchronous: the verdict lands after `result`.
        model_result("Created DONE.txt.", 2)
        time.sleep(0.4)
        row(met)
        return
    row(met)
    model_result("Created DONE.txt.", 2)


def tool_turn(calls):
    """calls: list of (tool_name, id, input, result_text, structured, is_error, parent)."""
    for name, call_id, data, result_text, structured, is_error, parent in calls:
        emit({"type": "assistant", "parent_tool_use_id": parent, "session_id": SESSION,
              "message": {"model": "claude-haiku-4-5-20251001", "role": "assistant",
                          "content": [{"type": "tool_use", "id": call_id, "name": name,
                                       "input": data}]}})
        frame = {"type": "user", "parent_tool_use_id": parent, "session_id": SESSION,
                 "message": {"role": "user", "content": [{"type": "tool_result",
                             "tool_use_id": call_id, "content": result_text,
                             "is_error": is_error}]}}
        if structured is not None:
            frame["tool_use_result"] = structured
        emit(frame)


FINDINGS = [
    {"file": "src/auth.rs", "line": 42, "summary": "The token expiry check is inverted, so expired "
     "tokens are accepted", "short_summary": "Inverted expiry check",
     "failure_scenario": "A token that expired yesterday passes `is_valid()` and logs the user in",
     "category": "correctness", "verdict": "CONFIRMED"},
    {"file": "src/db.rs", "summary": "Connection is never returned to the pool on error",
     "failure_scenario": "Ten failed queries exhaust the pool and every later request hangs",
     "category": "correctness", "verdict": "PLAUSIBLE"},
]


def findings_turn(prompt):
    if os.environ.get("CLAUDE_CODE_REPORT_FINDINGS") != "1":
        model_result("ReportFindings host opt-in missing", 1, is_error=True)
        return
    if "scenario:findings-empty" in prompt:
        tool_turn([("ReportFindings", "toolu_rf_empty", {"level": "low", "findings": []},
                    "No findings reported.", {"count": 0, "level": "low", "findings": []},
                    False, None)])
        model_result("", 1)
        return
    if "scenario:findings-error" in prompt:
        tool_turn([("ReportFindings", "toolu_rf_bad", {"level": "high", "findings": "oops"},
                    "<tool_use_error>InputValidationError: findings: Expected array</tool_use_error>",
                    None, True, None)])
        model_result("", 1)
        return
    tool_turn([
        ("ReportFindings", "toolu_rf_sub", {"level": "high", "findings": FINDINGS[:1]},
         "1 finding reported.", None, False, "toolu_agent_parent"),
        ("ReportFindings", "toolu_rf1", {"level": "high", "findings": FINDINGS + [
            {"summary": "no file here", "failure_scenario": "x"}]},
         "3 findings reported.", {"count": 3, "level": "high", "findings": FINDINGS}, False, None),
        ("ReportFindings", "toolu_rf2", {"level": "high", "findings": [
            dict(FINDINGS[0], outcome="fixed")]},
         "1 finding reported.", {"count": 1, "level": "high", "findings": []}, False, None),
    ])
    model_result("", 3)


def schedules_turn():
    now_ms = int(time.time() * 1000)
    tool_turn([
        ("CronCreate", "toolu_c1", {"cron": "*/5 * * * *", "prompt": "check the deploy",
                                     "recurring": True},
         "Scheduled recurring job a1b2c3d4 (every 5 minutes). Session-only (not written to disk, "
         "dies when Claude exits). Auto-expires after 7 days. Use CronDelete to cancel sooner.",
         {"id": "a1b2c3d4", "humanSchedule": "every 5 minutes", "recurring": True,
          "durable": False}, False, None),
        ("CronCreate", "toolu_c2", {"cron": "30 15 1 10 *", "prompt": "push the release branch",
                                     "recurring": False},
         "Scheduled one-shot task e5f6a7b8 (at 15:30 on Oct 1). Session-only (not written to "
         "disk, dies when Claude exits). It will fire once then auto-delete.",
         None, False, None),
        ("CronCreate", "toolu_c_sub", {"cron": "0 * * * *", "prompt": "subagent job"},
         "Scheduled recurring job 99999999 (every hour). Session-only.", {"id": "99999999",
          "humanSchedule": "every hour", "recurring": True}, False, "toolu_agent_parent"),
        ("CronCreate", "toolu_c_bad", {"cron": "61 * * * *", "prompt": "bad"},
         "<tool_use_error>Invalid cron expression '61 * * * *'.</tool_use_error>", None, True,
         None),
        ("ScheduleWakeup", "toolu_w1", {"delaySeconds": 600, "reason": "CI takes about ten "
                                         "minutes", "prompt": "/loop check CI", "noop": False},
         "Next wakeup scheduled.", {"scheduledFor": now_ms + 600000, "clampedDelaySeconds": 600,
                                    "wasClamped": False}, False, None),
        ("CronDelete", "toolu_d1", {"id": "a1b2c3d4"}, "Cancelled job a1b2c3d4.",
         {"id": "a1b2c3d4"}, False, None),
        ("CronList", "toolu_l1", {},
         "e5f6a7b8 — at 15:30 on Oct 1 (one-shot) [session-only]: push the release branch",
         {"jobs": [{"id": "e5f6a7b8", "cron": "30 15 1 10 *", "humanSchedule": "at 15:30 on Oct 1",
                    "prompt": "push the release branch", "durable": False}]}, False, None),
        ("ScheduleWakeup", "toolu_w2", {"stop": True},
         "Loop stopped — cancelled 1 pending wakeup(s); no further dynamic-loop wakeups "
         "scheduled.", {"scheduledFor": 0, "clampedDelaySeconds": 0, "wasClamped": False,
                        "stopped": True, "cancelledWakeups": 1}, False, None),
    ])
    model_result("Scheduled.", 1)


def handle(frame):
    text = text_of(frame).strip()
    if text.startswith("/goal"):
        goal_command(text[len("/goal"):])
    elif text.startswith("/code-review") or "scenario:findings" in text:
        findings_turn(text)
    elif "scenario:schedules" in text:
        schedules_turn()
    elif "scenario:slow" in text:
        say("thinking ")
        time.sleep(3)
        model_result("slow done", 1)
    else:
        say("ok")
        model_result("ok", 1)


with open(os.path.join(CONFIG, "starts.jsonl"), "a") as starts:
    starts.write(json.dumps({"argv": ARGS, "resume": RESUME, "pid": os.getpid()}) + "\n")
first = read_line()
if first is None:
    sys.exit(1)
if RESUME and not os.path.exists(TRANSCRIPT) and "missing" in RESUME:
    sys.exit(1)
init()
handle(first)
while True:
    frame = read_line()
    if frame is None:
        break
    if frame.get("type") == "control_response":
        continue
    init()
    handle(frame)
