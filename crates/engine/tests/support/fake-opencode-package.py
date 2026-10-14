#!/usr/bin/env python3
"""Scripted OpenCode 2 server for the CORE-11 suites (test bytes only).

It is launched exactly as LUNA's OpenCode adapter launches the real 2.0.x
binary (`opencode --version`; `opencode serve --stdio --port 0` with the
per-run Basic password), whether that adapter runs inside the engine or in
the first-party package's worker. It speaks the slice of the 2.x HTTP/SSE
surface the adapter uses: the `{"url"}` readiness line and the stdin lease,
`/api/info`, the structural `/openapi.json`, sessions with their rules read
back, the model, provider and command catalogs, the global `/api/event`
bus, prompts (with `delivery: "steer"`), interrupt, permission asks and
replies, and MCP status (it connects to LUNA's `luna` endpoint when the
run attaches one).

Turns are scripted by markers in the prompt:

  (default)           "PONG <n>", n counting this session's turns
  scenario:slow       "tick <n>" every 150 ms until interrupted; a steer is
                      enqueued and delivered at the next tick and answered
                      with "steered: <text>"
  scenario:fail       the execution fails with a provider error
  scenario:late       text frames after the execution's terminal frame
  scenario:malformed  a frame that is not JSON and an unknown event type
  scenario:reconnect  the bus is cut mid-turn; the turn resumes once the
                      adapter has subscribed again
  scenario:ask        a permission ask for `echo fixture`; the turn goes on
                      once it is answered
  scenario:large      one 100 KB text item (larger than a protocol frame)

It records and obeys files only in a directory holding
`.fake-opencode-checkout` (a chat's checkout), so discovery servers started
in $HOME leave nothing behind:

  .fake-opencode-hold-boot   wait 30 s before readiness
  .fake-opencode-drop-rules  the session readback omits the applied rules

Evidence is appended as JSON lines under `<checkout>/.fake-opencode/`:
`servers` (pid, parent pid), `prompts` (session, id, text, delivery, model),
`replies` (permission answers) and `interrupts`. Sessions persist there, so
a later server (a later turn's worker) resumes them.
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
CWD = os.getcwd()
RECORD = os.path.exists(os.path.join(CWD, ".fake-opencode-checkout"))
STATE = os.path.join(CWD, ".fake-opencode")
LOCK = threading.RLock()
SUBSCRIBERS = []
SUBSCRIBED = threading.Condition(LOCK)
SESSIONS = {}
RUNNING = set()
INTERRUPTED = set()
STEERS = {}
ASKS = {}
REPLIED = {}
MCP_STATE = {"status": "pending" if MCP_URL else None}
EVENT_SEQ = [0]
GENERATION = [0]

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
# State and evidence
# ---------------------------------------------------------------------------

def now_ms():
    return int(time.time() * 1000)


def marker(name):
    return RECORD and os.path.exists(os.path.join(CWD, name))


def record(kind, value):
    if not RECORD:
        return
    os.makedirs(STATE, exist_ok=True)
    with LOCK:
        with open(os.path.join(STATE, f"{kind}.jsonl"), "a") as out:
            out.write(json.dumps(value) + "\n")


def session_file(session_id):
    return os.path.join(STATE, f"session-{session_id}.json")


def save(session):
    if not RECORD:
        return
    os.makedirs(STATE, exist_ok=True)
    temporary = session_file(session["id"]) + ".tmp"
    with open(temporary, "w") as out:
        json.dump(session, out)
    os.replace(temporary, session_file(session["id"]))


def load(session_id):
    with LOCK:
        if session_id in SESSIONS:
            return SESSIONS[session_id]
        if not RECORD or not re.fullmatch(r"ses_[0-9a-f]+", session_id or ""):
            return None
        try:
            with open(session_file(session_id)) as source:
                session = json.load(source)
        except (OSError, ValueError):
            return None
        SESSIONS[session_id] = session
        return session


def public(session):
    view = {key: value for key, value in session.items() if key not in ("entries", "turns")}
    if marker(".fake-opencode-drop-rules"):
        view["permissions"] = []
    return view


def broadcast(kind, data):
    with LOCK:
        EVENT_SEQ[0] += 1
        frame = {"id": f"evt_{os.getpid()}_{EVENT_SEQ[0]}", "type": kind, "data": data}
        for subscriber in list(SUBSCRIBERS):
            subscriber.put(json.dumps(frame))


def raw_frame(text):
    with LOCK:
        for subscriber in list(SUBSCRIBERS):
            subscriber.put(text)


def emit(session_id, kind, **data):
    broadcast(kind, {"sessionID": session_id, **data})


def drop_bus_and_wait():
    """Cut every subscription, then wait for the adapter to subscribe again."""
    with LOCK:
        generation = GENERATION[0]
        for subscriber in list(SUBSCRIBERS):
            subscriber.put(None)
        deadline = time.time() + 10
        while GENERATION[0] == generation and time.time() < deadline:
            SUBSCRIBED.wait(timeout=0.2)


# ---------------------------------------------------------------------------
# LUNA's `luna` MCP endpoint, as OpenCode's MCP client
# ---------------------------------------------------------------------------

def mcp(body):
    request = urllib.request.Request(MCP_URL, data=json.dumps(body).encode(), method="POST")
    request.add_header("Authorization", f"Bearer {TOKEN}")
    request.add_header("Content-Type", "application/json")
    request.add_header("Accept", "application/json, text/event-stream")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            text = response.read().decode()
            return response.status, json.loads(text) if text else None
    except urllib.error.HTTPError as error:
        return error.code, None
    except Exception as error:  # refused / reset
        return 0, {"transportError": str(error)}


def connect_mcp():
    for _ in range(200):
        status, _ = mcp({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "opencode", "version": VERSION}}})
        if status == 200:
            mcp({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})
            MCP_STATE["status"] = "connected"
            return
        time.sleep(0.05)
    MCP_STATE["status"] = "failed"


# ---------------------------------------------------------------------------
# Turns
# ---------------------------------------------------------------------------

def open_turn(session, prompt_id):
    session_id = session["id"]
    time.sleep(0.2)
    emit(session_id, "session.inbox.enqueued", inboxID=prompt_id, item={"type": "user", "delivery": "queue"})
    with LOCK:
        RUNNING.add(session_id)
        INTERRUPTED.discard(session_id)
    emit(session_id, "session.execution.started")
    emit(session_id, "session.inbox.delivered", inboxID=prompt_id, item={"type": "user", "delivery": "queue"})
    message = f"msg_{uuid.uuid4().hex}"
    emit(session_id, "session.step.started", assistantMessageID=message)
    return message


def say(session_id, message, ordinal, text):
    emit(session_id, "session.text.started", assistantMessageID=message, ordinal=ordinal, text="")
    emit(session_id, "session.text.delta", assistantMessageID=message, ordinal=ordinal, delta=text)
    emit(session_id, "session.text.ended", assistantMessageID=message, ordinal=ordinal, text=text)


def settle(session, outcome):
    with LOCK:
        RUNNING.discard(session["id"])
        session.setdefault("time", {})["idle"] = now_ms()
        session["outcome"] = outcome
        save(session)


def slow_turn(session, message):
    session_id = session["id"]
    ordinal = 0
    for tick in range(1, 401):
        with LOCK:
            interrupted = session_id in INTERRUPTED
            steers = STEERS.pop(session_id, [])
        if interrupted:
            emit(session_id, "session.step.ended", assistantMessageID=message, finish="stop")
            settle(session, "interrupted")
            emit(session_id, "session.execution.interrupted", reason="user")
            return
        for steer_id, text in steers:
            emit(session_id, "session.inbox.delivered", inboxID=steer_id, item={"type": "user", "delivery": "steer"})
            say(session_id, message, ordinal, f"steered: {text}\n")
            ordinal += 1
        say(session_id, message, ordinal, f"tick {tick}\n")
        ordinal += 1
        time.sleep(0.15)
    emit(session_id, "session.step.ended", assistantMessageID=message, finish="stop")
    settle(session, "succeeded")
    emit(session_id, "session.execution.succeeded")


def ask_turn(session, message):
    session_id = session["id"]
    request_id = f"per_{uuid.uuid4().hex[:12]}"
    ask = {
        "id": request_id,
        "sessionID": session_id,
        "action": "bash",
        "resources": ["echo fixture"],
        "metadata": {"command": "echo fixture", "description": "Fixture command"},
    }
    with LOCK:
        ASKS[request_id] = ask
    broadcast("permission.asked", ask)
    deadline = time.time() + 60
    while time.time() < deadline:
        with LOCK:
            decision = REPLIED.pop(request_id, None)
            interrupted = session_id in INTERRUPTED
        if decision is not None:
            broadcast("permission.replied", {"sessionID": session_id, "requestID": request_id, "reply": decision})
            say(session_id, message, 0, f"permission answered: {decision}")
            emit(session_id, "session.step.ended", assistantMessageID=message, finish="stop")
            settle(session, "succeeded")
            emit(session_id, "session.execution.succeeded")
            return
        if interrupted:
            with LOCK:
                ASKS.pop(request_id, None)
            emit(session_id, "session.step.ended", assistantMessageID=message, finish="stop")
            settle(session, "interrupted")
            emit(session_id, "session.execution.interrupted", reason="user")
            return
        time.sleep(0.05)


def run_turn(session, prompt_id, text):
    session_id = session["id"]
    message = open_turn(session, prompt_id)
    try:
        if "scenario:slow" in text:
            return slow_turn(session, message)
        if "scenario:ask" in text:
            return ask_turn(session, message)
        if "scenario:fail" in text:
            say(session_id, message, 0, "starting")
            settle(session, "failed")
            emit(session_id, "session.execution.failed",
                 error={"type": "ProviderAuthError", "message": "fake provider refused the request"})
            return
        if "scenario:malformed" in text:
            raw_frame("{not json")
            broadcast("session.unknown.thing", {"sessionID": session_id, "detail": "ignored"})
        if "scenario:reconnect" in text:
            say(session_id, message, 0, "before the cut")
            drop_bus_and_wait()
            time.sleep(0.3)
            say(session_id, message, 1, "after the cut")
        elif "scenario:large" in text:
            say(session_id, message, 0, "LARGE " + "0123456789" * 10000)
        else:
            with LOCK:
                session["turns"] = session.get("turns", 0) + 1
                turns = session["turns"]
            say(session_id, message, 0, f"PONG {turns}")
        emit(session_id, "session.step.ended", assistantMessageID=message, finish="stop")
        settle(session, "succeeded")
        emit(session_id, "session.execution.succeeded")
        if "scenario:late" in text:
            say(session_id, message, 5, "late text")
            time.sleep(0.2)
            say(session_id, message, 6, "later text")
    except Exception as error:  # surface fixture faults in the turn
        sys.stderr.write(f"fake-opencode-package: {error!r}\n")


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
            return self.reply(200, {"data": [
                {"providerID": "fake", "id": "package-model", "name": "Package fixture", "enabled": True,
                 "limit": {"context": 200000, "output": 32000}, "variants": [{"id": "low"}, {"id": "high"}]},
                {"providerID": "fake", "id": "second-model", "name": "Second fixture", "enabled": True,
                 "limit": {"context": 100000, "output": 16000}},
            ]})
        if method == "GET" and path == "/api/provider":
            return self.reply(200, {"data": [{"id": "fake", "name": "Fake provider"}]})
        if method == "GET" and path == "/api/command":
            return self.reply(200, {"data": [{"name": "init", "description": "Initialize"},
                                             {"name": "review", "description": "Review"}]})
        if method == "GET" and path == "/api/skill":
            return self.reply(200, {"data": [{"name": "fixture-skill", "description": "Fixture skill"}]})
        if method == "GET" and path == "/api/mcp":
            rows = []
            if MCP_STATE["status"]:
                rows.append({"name": "luna", "status": {"status": MCP_STATE["status"]}})
            return self.reply(200, {"data": rows})
        if method == "GET" and path == "/api/permission/request":
            with LOCK:
                return self.reply(200, {"data": list(ASKS.values())})
        if method == "GET" and path == "/api/form":
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
                text = body.get("text") or ""
                delivery = body.get("delivery") or "queue"
                record("prompts", {"session": session["id"], "id": prompt_id, "text": text,
                                   "delivery": delivery, "model": session.get("model")})
                with LOCK:
                    running = session["id"] in RUNNING
                    if delivery == "steer" and running:
                        STEERS.setdefault(session["id"], []).append((prompt_id, text))
                if delivery == "steer" and running:
                    emit(session["id"], "session.inbox.enqueued", inboxID=prompt_id,
                         item={"type": "user", "delivery": "steer"})
                else:
                    threading.Thread(target=run_turn, args=(session, prompt_id, text), daemon=True).start()
                return self.reply(200, {"data": {"id": prompt_id, "time": {"created": now_ms()}}})
            if rest == ["interrupt"] and method == "POST":
                with LOCK:
                    running = session["id"] in RUNNING
                    if running:
                        INTERRUPTED.add(session["id"])
                record("interrupts", {"session": session["id"], "running": running})
                return self.reply(200, {"data": {"interrupted": running}})
            if len(rest) == 3 and rest[0] == "permission" and rest[2] == "reply" and method == "POST":
                body = self.body()
                decision = body.get("decision")
                record("replies", {"session": session["id"], "request": rest[1], "decision": decision})
                with LOCK:
                    if ASKS.pop(rest[1], None) is None:
                        return self.reply(404, {"name": "NotFoundError", "data": {"message": "no such request"}})
                    REPLIED[rest[1]] = decision
                return self.reply(200, {"data": True})
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
            GENERATION[0] += 1
            SUBSCRIBED.notify_all()
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
                except queue.Empty:
                    self.wfile.write(b": heartbeat\n\n")
                    self.wfile.flush()
                    continue
                if frame is None:
                    break
                self.wfile.write(("data: " + frame + "\n\n").encode())
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
    record("servers", {"pid": os.getpid(), "ppid": os.getppid()})
    if marker(".fake-opencode-hold-boot"):
        time.sleep(30)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    threading.Thread(target=watch_lease, daemon=True).start()
    if MCP_URL:
        threading.Thread(target=connect_mcp, daemon=True).start()
    sys.stdout.write(json.dumps({"url": f"http://127.0.0.1:{server.server_address[1]}"}) + "\n")
    sys.stdout.flush()
    server.serve_forever()


main()
