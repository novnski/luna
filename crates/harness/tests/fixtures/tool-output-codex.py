#!/usr/bin/env python3
"""Offline app-server peer for engine/tool-output integration. No model calls."""
import json
import sys
import time


def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


def note(method, **params):
    emit({"method": method, "params": {"threadId": "tool-output-thread", "turnId": "tool-output-turn", **params}})


def item(phase, value):
    note("item/" + phase, item=value)


command = {"type": "commandExecution", "id": "command", "command": "printf fixture", "status": "inProgress"}
changes = [{"path": "/fixture/a.txt", "kind": {"type": "add"}, "diff": "PATCH_HEAD\n" + "é" * 40000 + "\nPATCH_TAIL"}, {"path": "/fixture/b.txt", "kind": {"type": "update"}, "diff": "@@ -1 +1 @@\n-before\n+after\n"}]
output = "COMMAND_HEAD\n" + "é" * 50000 + "\nCOMMAND_TAIL"
started = False
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    rid = request.get("id")
    if method == "initialize":
        emit({"id": rid, "result": {"userAgent": "offline-tool-output-fixture"}})
    elif method in ("thread/start", "thread/resume"):
        emit({"id": rid, "result": {"thread": {"id": "tool-output-thread"}}})
    elif method == "thread/goal/get":
        # Installed Codex 0.159 ThreadGoalGetResponse: no goal on this thread.
        emit({"id": rid, "result": {"goal": None}})
    elif method == "turn/start":
        started = True
        emit({"id": rid, "result": {"turn": {"id": "tool-output-turn"}}})
        note("turn/started", turn={"id": "tool-output-turn"})
        item("started", command)
        item("started", {"type": "fileChange", "id": "patch", "status": "inProgress", "changes": []})
        note("item/commandExecution/outputDelta", itemId="command", delta="LIVE_BEFORE_STEER\n")
        note("item/commandExecution/terminalInteraction", itemId="command", processId="fixture-process", stdin="INPUT_BEFORE_STEER\n")
        note("item/fileChange/patchUpdated", itemId="patch", changes=changes)
    elif method == "turn/steer" and started:
        emit({"id": rid, "result": {"turnId": "tool-output-turn"}})
        client_id = request.get("params", {}).get("clientUserMessageId")
        user = {"type": "userMessage", "id": "steer-provider-user", "content": [{"type": "text", "text": "fixture steer"}]}
        if client_id:
            user["clientId"] = client_id
        item("started", user)
        item("completed", user)
        time.sleep(0.25)
        note("item/commandExecution/outputDelta", itemId="command", delta="LIVE_AFTER_STEER\n")
        changes.append({"path": "/fixture/c.txt", "kind": {"type": "add"}, "diff": "LATE_FILE_TAIL"})
        note("item/fileChange/patchUpdated", itemId="patch", changes=changes)
        note("item/fileChange/outputDelta", itemId="patch", delta="PATCH_PROGRESS\n")
        time.sleep(0.35)
        item("completed", {**command, "status": "completed", "exitCode": 0, "aggregatedOutput": output})
        item("completed", {"type": "fileChange", "id": "patch", "status": "completed", "changes": changes})
        item("completed", {"type": "fileChange", "id": "patch", "status": "completed", "changes": changes})
        item("started", {"type": "webSearch", "id": "web", "query": "fixture"})
        item("completed", {"type": "webSearch", "id": "web", "query": "fixture", "results": [{"text": "WEB_HEAD" + "w" * 70000 + "WEB_TAIL"}]})
        rich = {"type": "mcpToolCall", "id": "rich", "server": "fixture", "tool": "mixed", "arguments": {"secret": "ARGUMENT_SECRET"}, "status": "inProgress"}
        item("started", rich)
        item("completed", {**rich, "status": "failed", "result": {"content": [{"type": "text", "text": "MCP_HEAD" + "m" * 70000 + "MCP_TAIL"}, {"type": "image", "data": "SU1BR0U=", "mimeType": "image/png"}, {"type": "audio", "data": "QVVESU8=", "mimeType": "audio/wav"}, {"type": "resource", "resource": {"uri": "fixture://embedded", "text": "RESOURCE_TAIL"}}, {"type": "resource_link", "uri": "fixture://link", "name": "Fixture"}], "structuredContent": {"tail": "STRUCTURED_TAIL"}, "_meta": {"marker": "META_TAIL"}}, "error": {"message": "MCP_ERROR_TAIL"}, "mcpAppUi": {"resourceUri": "ui://fixture"}})
        for tool in ("list_threads", "read_thread", "send_message_to_thread", "wait_threads", "luna_status"):
            dynamic = {"type": "dynamicToolCall", "id": tool, "tool": tool, "arguments": {"secret": "DYNAMIC_ARGUMENT_SECRET"}, "status": "inProgress"}
            item("started", dynamic)
            item("completed", {**dynamic, "status": "failed", "success": False, "contentItems": [{"type": "inputText", "text": "DYNAMIC_RESULT_SECRET"}]})
        unknown = {"type": "futureTool", "id": "unknown", "payload": "UNKNOWN_SECRET" * 10000}
        item("started", unknown)
        item("completed", {**unknown, "status": "failed"})
        spawn = {"type": "collabAgentToolCall", "id": "spawn-child", "tool": "spawnAgent", "receiverThreadIds": ["fixture-child"], "status": "completed"}
        item("started", {**spawn, "status": "inProgress"})
        item("completed", spawn)
        child = {"threadId": "fixture-child", "turnId": "child-turn"}
        emit({"method": "turn/started", "params": {**child, "turn": {"id": "child-turn"}}})
        emit({"method": "item/started", "params": {**child, "item": {**command, "command": "child fixture"}}})
        emit({"method": "item/commandExecution/outputDelta", "params": {**child, "itemId": "command", "delta": "CHILD_LIVE\n"}})
        emit({"method": "item/completed", "params": {**child, "item": {**command, "command": "child fixture", "status": "completed", "exitCode": 0, "aggregatedOutput": "CHILD_TAIL"}}})
        emit({"method": "turn/completed", "params": {**child, "turn": {"id": "child-turn", "status": "completed"}}})
        item("completed", {"type": "agentMessage", "id": "answer", "text": "fixture complete"})
        note("turn/completed", turn={"id": "tool-output-turn", "status": "completed"})
    elif method == "turn/interrupt":
        emit({"id": rid, "result": {}})
        note("turn/completed", turn={"id": "tool-output-turn", "status": "interrupted"})
    elif rid is not None:
        emit({"id": rid, "result": {}})
