#!/usr/bin/env python3
"""Deterministic Claude Code 2.1.286 stream-json peer for CC-04 input tests.

Never a model. It appends argv and every stdin frame to
`<cwd>/claude-input-peer.jsonl` (one object per line: pid, kind, argv|frame)
and answers with the frame shapes the installed 2.1.286 binary emits:
`system/init` (cwd, session_id, tools, mcp_servers, model, permissionMode,
slash_commands, apiKeySource, claude_code_version, output_style, agents,
skills, plugins, uuid), `stream_event` text deltas, `assistant` messages and
`result` frames (duration_ms, duration_api_ms, is_error, num_turns, result,
stop_reason, session_id, total_cost_usd, usage, modelUsage,
permission_denials, uuid).

It records only in directories that opt in with a `.cc04-peer` marker, so a
discovery probe started from the package directory leaves no files behind.
A user line whose text contains `cc04:hold` opens a turn that stays open
until the next user line (a steer) arrives; every other user line is one
complete turn. Like the real CLI the process stays warm until stdin closes.
An `interrupt` control request is acknowledged and ends the process (the
real CLI settles the turn; ending is enough for input-delivery tests).
"""
import base64
import hashlib
import json
import os
import sys
import uuid

LOG = os.path.join(os.getcwd(), "claude-input-peer.jsonl")
LOGGING = os.path.exists(os.path.join(os.getcwd(), ".cc04-peer"))
ARGS = sys.argv[1:]
RESUME = next((a.split("=", 1)[1] for a in ARGS if a.startswith("--resume=")), None)
SESSION = RESUME or "cc04-session-%d" % os.getpid()


def record(kind, **payload):
    if not LOGGING:
        return
    with open(LOG, "a", encoding="utf-8") as log:
        log.write(json.dumps({"pid": os.getpid(), "kind": kind, **payload}) + "\n")


def emit(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def init_frame():
    return {
        "type": "system",
        "subtype": "init",
        "cwd": os.getcwd(),
        "session_id": SESSION,
        "tools": ["Bash", "Read", "Skill"],
        "mcp_servers": [],
        "model": "claude-haiku-4-5-20251001",
        "permissionMode": "default",
        "slash_commands": ["compact", "fixture-skill"],
        "apiKeySource": "none",
        "claude_code_version": "2.1.286",
        "output_style": "default",
        "agents": [],
        "skills": ["fixture-skill"],
        "plugins": [],
        "uuid": str(uuid.uuid4()),
    }


def describe(frame):
    content = frame.get("message", {}).get("content")
    if isinstance(content, str):
        return "text-only:" + content[:40]
    parts = []
    for block in content or []:
        if block.get("type") == "image":
            data = base64.b64decode(block["source"]["data"])
            parts.append("image:%s:%s" % (block["source"]["media_type"], hashlib.sha256(data).hexdigest()[:12]))
        else:
            parts.append("text:" + block.get("text", "")[:40])
    return " | ".join(parts)


def text_of(frame):
    content = frame.get("message", {}).get("content")
    if isinstance(content, str):
        return content
    return "\n".join(b.get("text", "") for b in content or [] if b.get("type") == "text")


def say(text):
    emit({
        "type": "stream_event",
        "parent_tool_use_id": None,
        "session_id": SESSION,
        "uuid": str(uuid.uuid4()),
        "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}},
    })
    emit({
        "type": "assistant",
        "parent_tool_use_id": None,
        "session_id": SESSION,
        "uuid": str(uuid.uuid4()),
        "message": {
            "id": "msg_" + uuid.uuid4().hex[:12],
            "type": "message",
            "role": "assistant",
            "model": "claude-haiku-4-5-20251001",
            "content": [{"type": "text", "text": text}],
            "stop_reason": None,
            "usage": {"input_tokens": 1, "output_tokens": 1},
        },
    })


def result(text, turns):
    emit({
        "type": "result",
        "subtype": "success",
        "is_error": False,
        "duration_ms": 5,
        "duration_api_ms": 0,
        "num_turns": turns,
        "result": text,
        "stop_reason": "end_turn",
        "session_id": SESSION,
        "total_cost_usd": 0,
        "usage": {"input_tokens": 1, "output_tokens": 1},
        "modelUsage": {},
        "permission_denials": [],
        "uuid": str(uuid.uuid4()),
    })


def main():
    record("argv", argv=ARGS)
    initialized = False
    holding = False
    turns = 0
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        frame = json.loads(raw)
        record("frame", frame=frame)
        if frame.get("type") == "control_request":
            request = frame.get("request", {})
            subtype = request.get("subtype")
            if subtype == "initialize":
                emit({"type": "control_response", "response": {
                    "subtype": "success", "request_id": frame["request_id"],
                    "response": {"commands": [{"name": "fixture-skill", "description": "Fixture skill", "argumentHint": ""}],
                                 "models": [{"value": "default", "resolvedModel": "claude-haiku-4-5-20251001"}],
                                 "output_style": "default"}}})
            elif subtype == "interrupt":
                emit({"type": "control_response", "response": {"subtype": "success", "request_id": frame["request_id"], "response": {}}})
                return
            continue
        if frame.get("type") != "user":
            continue
        if not initialized:
            emit(init_frame())
            initialized = True
        turns += 1
        summary = describe(frame)
        if holding:
            say("steer received " + summary)
            result("steered", turns)
            holding = False
            continue
        if "cc04:hold" in text_of(frame):
            say("holding " + summary)
            holding = True
            continue
        say("received " + summary)
        result("received " + summary, turns)


if __name__ == "__main__":
    main()
