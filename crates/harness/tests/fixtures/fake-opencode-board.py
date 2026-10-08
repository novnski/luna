#!/usr/bin/env python3
"""Deterministic OpenCode 2 server peer that calls LUNA board tools over MCP.

It is launched as the OpenCode adapter launches the real 2.0.22 binary
(`opencode --version`; `opencode serve --stdio --port 0` with the per-run
Basic password, the inline `OPENCODE_CONFIG_CONTENT` layer naming LUNA's
`luna` MCP server and the bearer token in `LUNA_MCP_BEARER_TOKEN`), and it
speaks the slice of the 2.x HTTP/SSE surface the adapter uses:

- readiness: the `{"url"}` stdout line, `GET /api/info`, a structural
  `/openapi.json` with every operation the adapter requires plus the LUNA
  delivery routes; it exits when its stdin (the ownership lease) closes;
- MCP: at boot it connects to the `luna` endpoint (initialize + tools/list),
  so `GET /api/mcp` reads `luna` back as connected and the endpoint has
  served its catalog, exactly what the adapter verifies before allowing a
  `luna_<tool>`;
- sessions (persisted per session under the run's cwd, so a resumed run in
  a new process finds its session), rules readback, the model, the `luna`
  instruction entry (PUT/GET/DELETE), inbox/message readbacks, interrupt;
- one turn per prompt on `/api/event`: inbox enqueued/delivered, execution
  started, one step, and for a prompt containing
  `scenario:board-plan:<label>` every call of `.luna-board-plan-<label>.json`
  as OpenCode makes it: `session.tool.input.started` (named `luna_<listed>`)
  and `session.tool.called` with the exact input on the bus, then the MCP
  `tools/call` carrying `_meta["ai.opencode/sessionID"]`, then
  `session.tool.success`/`failed`; replies go to
  `.luna-board-results-<label>.json`. Plan items may also be `{"relist":
  true}` or `{"frame": {"type", "data"}}` (a raw bus frame; `$SESSION` is
  substituted, e.g. `session.compaction.*`). Arguments may use
  `"$ref:<step>.<path>"`. Any other prompt is a plain turn that records the
  session's LUNA instruction entry in `.luna-board-turn-<task>.txt`;
- the execution then succeeds and the session reads back idle/succeeded.

Synthetic test evidence only, never real-provider proof.
"""
import base64
import json
import os
import queue
import re
import sys
import threading
import time
import urllib.error
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

VERSION = "2.0.22"

if "--version" in sys.argv[1:]:
    print(f"opencode v{VERSION}")
    sys.exit(0)
if not sys.argv[1:] or sys.argv[1] != "serve":
    sys.exit(2)

PASSWORD = os.environ.get("OPENCODE_PASSWORD") or os.environ.get("OPENCODE_SERVER_PASSWORD") or ""
CONFIG = json.loads(os.environ.get("OPENCODE_CONFIG_CONTENT") or "{}")
LUNA = (((CONFIG.get("mcp") or {}).get("servers") or {}).get("luna")) or None
MCP_URL = LUNA.get("url") if LUNA else None
TOKEN = os.environ.get("LUNA_MCP_BEARER_TOKEN", "")
TASK = os.environ.get("LUNA_THREAD_ID", "")
CWD = os.getcwd()
LOCK = threading.RLock()
SUBSCRIBERS = []
SESSIONS = {}
RUNNING = set()
MCP_STATE = {"status": "pending" if MCP_URL else None, "tools": []}
EVENT_SEQ = [0]

# ---------------------------------------------------------------------------
# The structural API contract the adapter checks at boot
# ---------------------------------------------------------------------------

REQUIREMENTS = [
    ("server.info", "get", "/api/info", None, []),
    ("event.subscribe", "get", "/api/event", None, []),
    ("session.create", "post", "/api/session", ["location", "permissions"], []),
    ("session.update", "patch", "/api/session/{sessionID}", ["permissions"], []),
    ("session.get", "get", "/api/session/{sessionID}", None, []),
    ("session.prompt", "post", "/api/session/{sessionID}/prompt", ["text", "files", "id", "delivery"], []),
    ("session.permission.reply", "post", "/api/session/{sessionID}/permission/{requestID}/reply", ["decision", "message"], []),
    ("session.form.reply", "post", "/api/session/{sessionID}/form/{formID}/reply", ["answer"], []),
    ("session.form.cancel", "delete", "/api/session/{sessionID}/form/{formID}", None, ["message"]),
    ("permission.request.list", "get", "/api/permission/request", None, []),
    ("form.list", "get", "/api/form", None, []),
    ("session.command", "post", "/api/session/{sessionID}/command", ["name", "text", "files"], []),
    ("session.compact", "post", "/api/session/{sessionID}/compact", [], []),
    ("session.fork", "post", "/api/session/{sessionID}/fork", ["before"], []),
    ("session.interrupt", "post", "/api/session/{sessionID}/interrupt", None, []),
    ("session.switchModel", "post", "/api/session/{sessionID}/model", ["model"], []),
    ("session.active", "get", "/api/session/active", None, []),
    ("session.message.list", "get", "/api/session/{sessionID}/message", None, ["cursor", "order", "limit"]),
    ("session.remove", "delete", "/api/session/{sessionID}", None, []),
    ("session.revert.stage", "post", "/api/session/{sessionID}/revert/stage", ["messageID", "files"], []),
    ("session.revert.commit", "post", "/api/session/{sessionID}/revert/commit", None, []),
    ("session.revert.clear", "delete", "/api/session/{sessionID}/revert", None, []),
    ("model.list", "get", "/api/model", None, []),
    ("command.list", "get", "/api/command", None, []),
    ("session.inbox.list", "get", "/api/session/{sessionID}/inbox", None, []),
    ("session.inbox.cancel", "delete", "/api/session/{sessionID}/inbox/{inboxID}", None, []),
    ("session.message.get", "get", "/api/session/{sessionID}/message/{messageID}", None, []),
    ("shell.output", "get", "/api/shell/{id}/output", None, ["cursor", "limit"]),
    ("mcp.list", "get", "/api/mcp", None, []),
]
DELIVERY_ROUTES = [
    ("put", "/api/experimental/session/{sessionID}/instructions/entries/{key}"),
    ("get", "/api/experimental/session/{sessionID}/instructions/entries"),
    ("delete", "/api/experimental/session/{sessionID}/instructions/entries/{key}"),
    ("get", "/api/mcp"),
]


def openapi():
    paths = {}
    for operation, method, path, body, query in REQUIREMENTS:
        entry = {"operationId": operation}
        if query:
            entry["parameters"] = [{"in": "query", "name": name, "schema": {"type": "string"}} for name in query]
        if body is not None:
            properties = {field: {} for field in body}
            if operation == "session.create":
                properties["permissions"] = {"type": "array", "items": {"type": "object", "properties": {
                    "action": {"type": "string"}, "resource": {"type": "string"}, "effect": {"type": "string"}}}}
            entry["requestBody"] = {"content": {"application/json": {"schema": {
                "type": "object", "properties": properties}}}}
        paths.setdefault(path, {})[method] = entry
    for method, path in DELIVERY_ROUTES:
        paths.setdefault(path, {}).setdefault(method, {})
    return {"openapi": "3.1.0", "info": {"title": "opencode", "version": "0.0.1"}, "paths": paths}


# ---------------------------------------------------------------------------
# State
# ---------------------------------------------------------------------------

def now_ms():
    return int(time.time() * 1000)


def session_file(session_id):
    return os.path.join(CWD, f".luna-opencode-board-{session_id}.json")


def save(session):
    if not TASK:
        return  # probe servers (model/command discovery) keep nothing
    temporary = session_file(session["id"]) + ".tmp"
    with open(temporary, "w") as out:
        json.dump(session, out)
    os.replace(temporary, session_file(session["id"]))


def load(session_id):
    with LOCK:
        if session_id in SESSIONS:
            return SESSIONS[session_id]
        try:
            with open(session_file(session_id)) as source:
                session = json.load(source)
        except (OSError, ValueError):
            return None
        session["resumedByProcess"] = True
        SESSIONS[session_id] = session
        return session


def public(session):
    return {key: value for key, value in session.items() if key not in ("entries", "resumedByProcess")}


def broadcast(kind, data):
    with LOCK:
        EVENT_SEQ[0] += 1
        frame = {"id": f"evt_{os.getpid()}_{EVENT_SEQ[0]}", "type": kind, "data": data}
        for subscriber in list(SUBSCRIBERS):
            subscriber.put(frame)


def emit(session_id, kind, **data):
    broadcast(kind, {"sessionID": session_id, **data})


# ---------------------------------------------------------------------------
# The luna MCP endpoint, as OpenCode's MCP client
# ---------------------------------------------------------------------------

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
        "clientInfo": {"name": "opencode", "version": VERSION}}})
    _, listed = mcp({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})
    tools = ((listed or {}).get("result") or {}).get("tools", [])
    return {
        "listed": sorted(tool["name"] for tool in tools),
        # Canonical engine names (OpenCode lists `luna_status` as `status`).
        "tools": sorted(canonical(tool["name"]) for tool in tools),
        "readOnly": {canonical(tool["name"]): tool.get("annotations", {}).get("readOnlyHint") for tool in tools},
        "schemas": {canonical(tool["name"]): tool.get("inputSchema") for tool in tools},
    }


def canonical(listed):
    return "luna_status" if listed == "status" else listed


def listed_name(tool):
    names = MCP_STATE["tools"]
    if tool in names:
        return tool
    if tool.startswith("luna_") and tool[len("luna_"):] in names:
        return tool[len("luna_"):]
    return tool


def connect_mcp():
    for _ in range(200):
        status, reply = mcp({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "opencode", "version": VERSION}}})
        if status == 200:
            _, listed = mcp({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})
            MCP_STATE["tools"] = [tool["name"] for tool in ((listed or {}).get("result") or {}).get("tools", [])]
            MCP_STATE["status"] = "connected"
            return
        time.sleep(0.05)
    MCP_STATE["status"] = "failed"


NEXT_ID = [10]


def call(listed, arguments, session_id):
    NEXT_ID[0] += 1
    status, reply = mcp({"jsonrpc": "2.0", "id": NEXT_ID[0], "method": "tools/call", "params": {
        "name": listed, "arguments": arguments, "_meta": {"ai.opencode/sessionID": session_id}}})
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


def substitute(value, session_id):
    if isinstance(value, str):
        return value.replace("$SESSION", session_id)
    if isinstance(value, list):
        return [substitute(entry, session_id) for entry in value]
    if isinstance(value, dict):
        return {key: substitute(entry, session_id) for key, entry in value.items()}
    return value


# ---------------------------------------------------------------------------
# One turn
# ---------------------------------------------------------------------------

def run_plan(label, session, message):
    session_id = session["id"]
    with open(os.path.join(CWD, f".luna-board-plan-{label}.json")) as source:
        steps = json.load(source)
    results = {
        "attachment": {
            "url": MCP_URL, "session": session_id, "resumed": bool(session.get("resumedByProcess")),
            "tokenInEnv": bool(TOKEN), "tokenInConfig": bool(TOKEN) and TOKEN in json.dumps(CONFIG),
        },
        "instructions": (session.get("entries") or {}).get("luna"),
        "handshake": list_tools(),
    }
    for index, item in enumerate(steps):
        if "frame" in item or "frames" in item:
            frames = [substitute(frame, session_id) for frame in item.get("frames") or [item["frame"]]]
            for frame in frames:
                broadcast(frame["type"], frame.get("data") or {})
            time.sleep(0.2)
            results[str(index)] = {"frames": [frame["type"] for frame in frames]}
            continue
        if item.get("relist"):
            results[str(index)] = list_tools()
            continue
        arguments = resolve(item.get("arguments", {}), results)
        listed = listed_name(item["tool"])
        call_id = f"call_{label}_{index}"
        emit(session_id, "session.tool.input.started", assistantMessageID=message, id=call_id, name=f"luna_{listed}")
        emit(session_id, "session.tool.called", assistantMessageID=message, id=call_id, input=arguments)
        time.sleep(0.05)
        outcome = call(listed, arguments, session_id)
        results[str(index)] = outcome
        content = [{"type": "text", "text": outcome["text"]}]
        if outcome["isError"]:
            emit(session_id, "session.tool.failed", assistantMessageID=message, id=call_id,
                 error={"message": outcome["text"]})
        else:
            emit(session_id, "session.tool.success", assistantMessageID=message, id=call_id, content=content)
    temporary = os.path.join(CWD, f".luna-board-results-{label}.json.tmp")
    with open(temporary, "w") as out:
        json.dump(results, out, indent=1)
    os.replace(temporary, os.path.join(CWD, f".luna-board-results-{label}.json"))


def run_turn(session, prompt_id, text):
    session_id = session["id"]
    time.sleep(0.2)
    emit(session_id, "session.inbox.enqueued", inboxID=prompt_id, item={"type": "user", "delivery": "queue"})
    with LOCK:
        RUNNING.add(session_id)
    emit(session_id, "session.execution.started")
    emit(session_id, "session.inbox.delivered", inboxID=prompt_id, item={"type": "user", "delivery": "queue"})
    message = f"msg_{uuid.uuid4().hex}"
    emit(session_id, "session.step.started", assistantMessageID=message)
    label = re.search(r"scenario:board-plan:([A-Za-z0-9_-]+)", text or "")
    try:
        if label and MCP_URL:
            run_plan(label.group(1), session, message)
        elif TASK:
            with open(os.path.join(CWD, f".luna-board-turn-{TASK}.txt"), "w") as out:
                out.write((session.get("entries") or {}).get("luna") or "")
    except Exception as error:  # surface fixture faults in the turn
        sys.stderr.write(f"fake-opencode-board: {error!r}\n")
    emit(session_id, "session.text.started", assistantMessageID=message, ordinal=0, text="")
    emit(session_id, "session.text.ended", assistantMessageID=message, ordinal=0, text="Board fixture turn finished.")
    emit(session_id, "session.step.ended", assistantMessageID=message, finish="stop")
    with LOCK:
        RUNNING.discard(session_id)
        session.setdefault("time", {})["idle"] = now_ms()
        session["outcome"] = "succeeded"
        save(session)
    emit(session_id, "session.execution.succeeded")


# ---------------------------------------------------------------------------
# HTTP
# ---------------------------------------------------------------------------

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def authorized(self):
        expected = "Basic " + base64.b64encode(f"opencode:{PASSWORD}".encode()).decode()
        return not PASSWORD or self.headers.get("Authorization") == expected

    def body(self):
        length = int(self.headers.get("Content-Length") or 0)
        if not length:
            return {}
        try:
            return json.loads(self.rfile.read(length).decode() or "{}")
        except ValueError:
            return {}

    def reply(self, status, value=None):
        data = b"" if value is None else json.dumps(value).encode()
        self.send_response(status)
        if value is not None:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        if data:
            self.wfile.write(data)

    def not_found(self):
        self.reply(404, {"name": "NotFoundError", "data": {"message": f"no route {self.command} {self.path}"}})

    def route(self):
        if not self.authorized():
            return self.reply(401, {"name": "Unauthorized"})
        path = urlparse(self.path).path.rstrip("/")
        method = self.command
        parts = [part for part in path.split("/") if part]
        if method == "GET" and path == "/api/info":
            return self.reply(200, {"version": VERSION, "pid": os.getpid(), "urls": [], "paths": {}})
        if method == "GET" and path == "/openapi.json":
            return self.reply(200, openapi())
        if method == "GET" and path == "/api/event":
            return self.stream()
        if method == "GET" and path == "/api/agent":
            return self.reply(200, {"data": [{"id": "build", "permissions": []}]})
        if method == "GET" and path == "/api/model":
            return self.reply(200, {"data": [{"providerID": "fake", "id": "board", "name": "Board fixture",
                                              "enabled": True, "limit": {"context": 200000, "output": 32000}}]})
        if method == "GET" and path == "/api/provider":
            return self.reply(200, {"data": [{"id": "fake", "name": "Fake provider"}]})
        if method == "GET" and path == "/api/command":
            return self.reply(200, {"data": [{"name": "init", "description": "Initialize"},
                                             {"name": "review", "description": "Review"}]})
        if method == "GET" and path == "/api/skill":
            return self.reply(200, {"data": [{"name": "opencode", "description": "OpenCode"}]})
        if method == "GET" and path == "/api/mcp":
            rows = []
            if MCP_STATE["status"]:
                rows.append({"name": "luna", "status": {"status": MCP_STATE["status"]}})
            return self.reply(200, {"data": rows})
        if method == "GET" and path in ("/api/permission/request", "/api/form"):
            return self.reply(200, {"data": []})
        if method == "GET" and path == "/api/session/active":
            with LOCK:
                return self.reply(200, {"data": {session: {"type": "busy"} for session in RUNNING}})
        if method == "POST" and path == "/api/session":
            body = self.body()
            session_id = f"ses_{uuid.uuid4().hex}"
            session = {
                "id": session_id, "agent": "build", "permissions": body.get("permissions"),
                "location": body.get("location"), "time": {"created": now_ms()}, "entries": {},
            }
            with LOCK:
                SESSIONS[session_id] = session
                save(session)
            return self.reply(200, {"data": public(session)})
        # /api/experimental/session/{id}/instructions/entries[/{key}]
        if len(parts) >= 6 and parts[:3] == ["api", "experimental", "session"] and parts[4:6] == ["instructions", "entries"]:
            session = load(parts[3])
            if session is None:
                return self.not_found()
            entries = session.setdefault("entries", {})
            if method == "GET" and len(parts) == 6:
                return self.reply(200, {"data": [{"key": key, "value": value} for key, value in entries.items()]})
            if len(parts) == 7 and method == "PUT":
                entries[parts[6]] = self.body().get("value")
                save(session)
                return self.reply(204)
            if len(parts) == 7 and method == "DELETE":
                entries.pop(parts[6], None)
                save(session)
                return self.reply(204)
            return self.not_found()
        if len(parts) >= 3 and parts[:2] == ["api", "session"]:
            session = load(parts[2])
            if session is None:
                return self.not_found()
            rest = parts[3:]
            if not rest and method == "GET":
                return self.reply(200, {"data": public(session)})
            if not rest and method == "PATCH":
                body = self.body()
                if "permissions" in body:
                    session["permissions"] = body["permissions"]
                save(session)
                return self.reply(200, {"data": public(session)})
            if rest == ["model"] and method == "POST":
                session["model"] = self.body().get("model")
                save(session)
                return self.reply(200, {"data": public(session)})
            if rest == ["prompt"] and method == "POST":
                body = self.body()
                prompt_id = body.get("id") or f"msg_{uuid.uuid4().hex}"
                threading.Thread(target=run_turn, args=(session, prompt_id, body.get("text")), daemon=True).start()
                return self.reply(200, {"data": {"id": prompt_id, "time": {"created": now_ms()}}})
            if rest == ["interrupt"] and method == "POST":
                with LOCK:
                    running = session["id"] in RUNNING
                return self.reply(200, {"data": {"interrupted": running}})
            if rest == ["inbox"] and method == "GET":
                return self.reply(200, {"data": []})
            if rest == ["message"] and method == "GET":
                return self.reply(200, {"data": []})
            return self.not_found()
        return self.not_found()

    def stream(self):
        subscriber = queue.Queue()
        with LOCK:
            SUBSCRIBERS.append(subscriber)
        try:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            self.wfile.write(("data: " + json.dumps({"id": "evt_connected", "type": "server.connected", "data": {}}) + "\n\n").encode())
            self.wfile.flush()
            while True:
                try:
                    frame = subscriber.get(timeout=10)
                    self.wfile.write(("data: " + json.dumps(frame) + "\n\n").encode())
                except queue.Empty:
                    self.wfile.write(b": heartbeat\n\n")
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            with LOCK:
                if subscriber in SUBSCRIBERS:
                    SUBSCRIBERS.remove(subscriber)

    def do_GET(self):
        self.route()

    def do_POST(self):
        self.route()

    def do_PATCH(self):
        self.route()

    def do_PUT(self):
        self.route()

    def do_DELETE(self):
        self.route()


def watch_lease():
    # The adapter holds stdin as the ownership lease: EOF ends the server.
    while sys.stdin.readline():
        pass
    os._exit(0)


def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    threading.Thread(target=watch_lease, daemon=True).start()
    if MCP_URL:
        threading.Thread(target=connect_mcp, daemon=True).start()
    sys.stdout.write(json.dumps({"url": f"http://127.0.0.1:{server.server_address[1]}"}) + "\n")
    sys.stdout.flush()
    server.serve_forever()


main()
