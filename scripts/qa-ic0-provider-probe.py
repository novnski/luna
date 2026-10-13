#!/usr/bin/env python3
"""IC-0 wire probe: installed Codex / Claude Code / OpenCode schemas and the
MCP Apps host operations each provider actually exposes to a client.

Contract (docs/pending-tasks/interactive-chat-surfaces/ic-0/README.md, "wire"):
every provider is launched with an isolated home/config and one disposable
stdio fixture server (scripts/fixtures/ic0-mcp-app-server.py) that declares an
SEP-1865 app view. The probe then records, per provider:

- version, generated/advertised schema facts and capability words;
- what the fixture saw at `initialize` (does the client negotiate the
  `io.modelcontextprotocol/ui` extension?);
- each host operation it tries, through that provider's own live client:
  status/tool metadata, `ui://` resource read, read-only tool call and one
  mutation of the fixture's in-memory counter (checked against the fixture's
  operation log), plus refusals that must hold;
- unsupported operations, named precisely.

No model turn, login, user configuration, credential or real MCP server is
used: Codex and OpenCode get a temporary HOME/CODEX_HOME/XDG tree, Claude a
temporary CLAUDE_CONFIG_DIR, a dummy API key and a closed loopback base URL
(the method of scripts/qa-claude-launch-probe.py). The only mutation is the
fixture's own counter.

    python3 scripts/qa-ic0-provider-probe.py <evidence-dir> [--only codex,claude,opencode]

Writes <evidence-dir>/ic0-provider-probe.json, <evidence-dir>/codex-schema/
(the generated app-server schema) and <evidence-dir>/fixture-oplog-*.jsonl.
Exit 0: every EXPECTED fact held. Exit 1: drift or a blocked live probe; each
is named in `drift`. Exit 2: usage. Re-run whenever a provider CLI changes.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import platform
import queue
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURE = ROOT / "scripts/fixtures/ic0-mcp-app-server.py"
FIXTURE_NAME = "luna-ic0-fixture"
APP_URI = "ui://luna-ic0/app.html"
APP_MIME = "text/html;profile=mcp-app"
UI_EXTENSION = "io.modelcontextprotocol/ui"
HOME = str(Path.home())
TEMP_MARK = "<probe-tmp>"
MAX_TEXT = 4000

# Facts read from the installed CLIs' on-disk schemas while writing IC-0
# (Codex 0.160.0 generated schema, Claude 2.1.293 control schema, OpenCode
# 2.0.24 route table). A mismatch is drift: re-read the adapter and the
# contract before trusting a newer CLI.
EXPECTED = {
    "codex": {
        "clientMethods": ["initialize", "thread/start", "mcpServerStatus/list", "mcpServer/resource/read",
                          "mcpServer/tool/call"],
        "resourceReadParams": ["server", "uri", "threadId", "originCallId"],
        "toolCallParams": ["server", "threadId", "tool", "arguments", "_meta"],
        "mcpToolCallItemFields": ["mcpAppUi", "mcpAppResourceUri", "appContext"],
        "initializeCapabilityFields": ["experimentalApi", "extensions"],
        "dynamicToolContentTypes": ["inputText", "inputImage"],
    },
    "claude": {
        "capabilities": ["mcp_read_resource_v1", "mcp_tool_ui_meta_v1"],
        "controls": ["mcp_status", "mcp_read_resource", "mcp_call"],
    },
    "opencode": {
        # Every OpenCode route whose path mentions MCP. Anything new here
        # (a resource read or tool call for a host) changes the contract.
        "mcpRoutes": ["/api/experimental/mcp/{server}", "/api/experimental/mcp/{server}/connect",
                      "/api/experimental/mcp/{server}/disconnect", "/api/mcp", "/api/mcp/resource"],
    },
}


# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------

def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


class Redactor:
    def __init__(self, temp: Path):
        self.temp = str(temp)

    def text(self, value: str) -> str:
        value = value.replace(self.temp, TEMP_MARK).replace(HOME, "~")
        value = re.sub(r"(sk-ant-[A-Za-z0-9-]{4})[A-Za-z0-9-]+", r"\1…", value)
        return value if len(value) <= MAX_TEXT else value[:MAX_TEXT] + f"…[{len(value)} chars]"

    def value(self, value):
        if isinstance(value, str):
            return self.text(value)
        if isinstance(value, list):
            return [self.value(item) for item in value[:200]]
        if isinstance(value, dict):
            return {str(key): self.value(item) for key, item in list(value.items())[:200]}
        return value


def run(argv: list[str], env: dict | None = None, cwd: Path | None = None, timeout: int = 120) -> dict:
    started = time.time()
    try:
        done = subprocess.run(argv, env=env, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        return {"exit": done.returncode, "stdout": done.stdout.strip(), "stderr": done.stderr.strip()[-2000:],
                "seconds": round(time.time() - started, 2)}
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"exit": None, "error": str(error), "seconds": round(time.time() - started, 2)}


def closed_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def read_oplog(path: Path) -> list[dict]:
    if not path.exists():
        return []
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return rows


def increments(path: Path) -> int:
    return sum(1 for row in read_oplog(path) if row.get("method") == "tools/call"
               and row.get("tool") == "ic0_counter_increment")


def served_sha(path: Path) -> str | None:
    shas = [row.get("servedSha256") for row in read_oplog(path)
            if row.get("method") == "resources/read" and row.get("found")]
    return shas[-1] if shas else None


def negotiated(path: Path) -> dict:
    """What the fixture saw at `initialize`: the client's MCP Apps declaration."""
    for row in read_oplog(path):
        if row.get("method") == "initialize":
            capabilities = row.get("capabilities") or {}
            extensions = capabilities.get("extensions") if isinstance(capabilities, dict) else None
            return {
                "protocolVersion": row.get("protocolVersion"),
                "clientInfo": row.get("clientInfo"),
                "capabilityKeys": sorted(capabilities) if isinstance(capabilities, dict) else None,
                "extensionKeys": sorted(extensions) if isinstance(extensions, dict) else None,
                "uiExtension": extensions.get(UI_EXTENSION) if isinstance(extensions, dict) else None,
            }
    return {"initializeSeen": False}


def content_types(result) -> list[str]:
    if not isinstance(result, dict):
        return []
    content = result.get("content")
    if not isinstance(content, list):
        return []
    return [item.get("type") for item in content if isinstance(item, dict)]


def resource_text(contents) -> bytes | None:
    if not isinstance(contents, list) or not contents or not isinstance(contents[0], dict):
        return None
    first = contents[0]
    if isinstance(first.get("text"), str):
        return first["text"].encode()
    if isinstance(first.get("blob"), str):
        try:
            return base64.b64decode(first["blob"])
        except ValueError:
            return None
    return None


def json_or_none(body: bytes):
    try:
        return json.loads(body)
    except (ValueError, TypeError):
        return None


def fixture_command(oplog: Path, label: str) -> list[str]:
    return [sys.executable, "-I", str(FIXTURE), str(oplog), label]


class LineProcess:
    """A child speaking newline-delimited JSON on stdio, read on a thread."""

    def __init__(self, argv: list[str], env: dict, cwd: Path):
        self.proc = subprocess.Popen(argv, env=env, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, text=True, bufsize=1)
        self.frames: queue.Queue = queue.Queue()
        self.backlog: list[dict] = []
        self.stderr_tail: list[str] = []
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._drain, daemon=True).start()

    def _read(self):
        for line in self.proc.stdout:
            try:
                frame = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(frame, dict):
                self.frames.put(frame)
        self.frames.put(None)

    def _drain(self):
        for line in self.proc.stderr:
            self.stderr_tail = (self.stderr_tail + [line.rstrip()])[-20:]

    def send(self, frame: dict):
        # A child that already exited answers nothing; the wait reports it.
        try:
            self.proc.stdin.write(json.dumps(frame) + "\n")
            self.proc.stdin.flush()
        except (OSError, ValueError):
            pass

    def wait_for(self, matches, timeout: float, on_other=None):
        """Next frame satisfying `matches`; earlier backlog first."""
        for index, frame in enumerate(self.backlog):
            if matches(frame):
                return self.backlog.pop(index)
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                frame = self.frames.get(timeout=max(0.05, deadline - time.time()))
            except queue.Empty:
                break
            if frame is None:
                self.frames.put(None)  # keep end-of-stream visible to later waits
                return None
            if matches(frame):
                return frame
            if on_other and on_other(frame):
                continue
            self.backlog = (self.backlog + [frame])[-500:]
        return None

    def close(self, grace: float = 5.0):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()


def clean_env(drop_prefixes: tuple[str, ...]) -> dict:
    return {key: value for key, value in os.environ.items() if not key.startswith(drop_prefixes)}


# ---------------------------------------------------------------------------
# Codex: generated app-server schema + live app-server client
# ---------------------------------------------------------------------------

def codex_schema_facts(schema_dir: Path) -> dict:
    def load(name):
        path = schema_dir / name
        return json.loads(path.read_text()) if path.exists() else {}

    def methods(document):
        found = []
        for variant in document.get("oneOf", []):
            found += variant.get("properties", {}).get("method", {}).get("enum", [])
        return sorted(set(found))

    definitions: dict = {}
    for path in sorted(schema_dir.rglob("*.json")):
        try:
            document = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        if isinstance(document, dict):
            for key, value in (document.get("definitions") or {}).items():
                definitions.setdefault(key, value)
            title = document.get("title")
            if title and title not in definitions:
                definitions[title] = document

    def properties(name):
        return sorted((definitions.get(name) or {}).get("properties", {}).keys())

    tool_item = []
    for variant in (definitions.get("ThreadItem") or {}).get("oneOf", []):
        if variant.get("properties", {}).get("type", {}).get("enum") == ["mcpToolCall"]:
            tool_item = sorted(variant["properties"].keys())
    dynamic_types = []
    for variant in (definitions.get("DynamicToolCallOutputContentItem") or {}).get("oneOf", []):
        dynamic_types += variant.get("properties", {}).get("type", {}).get("enum", [])
    client = methods(load("ClientRequest.json"))
    server_requests = methods(load("ServerRequest.json"))
    notifications = methods(load("ServerNotification.json"))
    files = sorted(str(path.relative_to(schema_dir)) for path in schema_dir.rglob("*.json"))
    return {
        "fileCount": len(files),
        "sha256": {name: sha256((schema_dir / name).read_bytes()) for name in files
                   if name in ("ClientRequest.json", "ServerRequest.json", "ServerNotification.json",
                               "codex_app_server_protocol.v2.schemas.json")},
        "clientMethodCount": len(client),
        "serverRequestCount": len(server_requests),
        "serverNotificationCount": len(notifications),
        "mcpOrAppClientMethods": [m for m in client if re.search(r"mcp|app|resource", m, re.I)],
        "mcpServerRequests": [m for m in server_requests if "mcp" in m.lower()],
        "mcpNotifications": [m for m in notifications if "mcp" in m.lower() or m.startswith("app/")],
        "clientMethods": client,
        "resourceReadParams": properties("McpResourceReadParams"),
        "resourceReadResponse": properties("McpResourceReadResponse"),
        "toolCallParams": properties("McpServerToolCallParams"),
        "toolCallResponse": properties("McpServerToolCallResponse"),
        "statusParams": properties("ListMcpServerStatusParams"),
        "statusRow": properties("McpServerStatus"),
        "mcpToolCallItemFields": tool_item,
        "mcpAppUi": properties("McpAppUi"),
        "initializeCapabilityFields": properties("InitializeCapabilities"),
        "dynamicToolContentTypes": sorted(set(dynamic_types)),
    }


class JsonRpc:
    def __init__(self, process: LineProcess):
        self.process = process
        self.next_id = 0

    def _server_request(self, frame):
        # The fixture never elicits; refuse any server→client request.
        if "method" in frame and "id" in frame:
            self.process.send({"jsonrpc": "2.0", "id": frame["id"],
                               "error": {"code": -32601, "message": "IC-0 probe answers no server requests"}})
            return True
        return False

    def call(self, method: str, params: dict, timeout: float = 30) -> dict:
        self.next_id += 1
        request_id = self.next_id
        self.process.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        frame = self.process.wait_for(lambda f: f.get("id") == request_id and "method" not in f, timeout,
                                      on_other=self._server_request)
        if frame is None:
            return {"error": {"message": f"{method}: no response within {timeout}s"}}
        return {"result": frame["result"]} if "result" in frame else {"error": frame.get("error")}

    def notify(self, method: str, params: dict | None = None):
        frame = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            frame["params"] = params
        self.process.send(frame)


def probe_codex(work: Path, out: Path, redact: Redactor) -> dict:
    exe = shutil.which("codex")
    if not exe:
        return {"installed": False}
    home = work / "codex-home"
    user_home = work / "codex-user-home"
    project = work / "codex-project"
    for path in (home, user_home, project):
        path.mkdir()
    oplog = out / "fixture-oplog-codex.jsonl"
    oplog.unlink(missing_ok=True)
    argv = fixture_command(oplog, "codex")
    (home / "config.toml").write_text(
        f"[mcp_servers.{FIXTURE_NAME}]\n"
        f"command = {json.dumps(argv[0])}\n"
        f"args = {json.dumps(argv[1:])}\n"
        "startup_timeout_sec = 20\n"
    )
    env = clean_env(("CODEX", "OPENAI"))
    env.update({"CODEX_HOME": str(home), "HOME": str(user_home), "RUST_LOG": "error"})
    record = {"installed": True, "executable": redact.text(exe),
              "version": run([exe, "--version"], env=env).get("stdout")}
    schema_dir = out / "codex-schema"
    shutil.rmtree(schema_dir, ignore_errors=True)
    generate = run([exe, "app-server", "generate-json-schema", "--out", str(schema_dir)], env=env)
    record["schemaCommand"] = {"exit": generate.get("exit"), "stderr": redact.text(generate.get("stderr", ""))}
    record["schema"] = codex_schema_facts(schema_dir) if schema_dir.exists() else {}

    live: dict = {}
    process = LineProcess([exe, "app-server"], env, project)
    rpc = JsonRpc(process)
    try:
        init_params = {"clientInfo": {"name": "luna-ic0-probe", "title": "LUNA IC-0 probe", "version": "0"},
                       "capabilities": {"experimentalApi": True,
                                        "extensions": {UI_EXTENSION: {"mimeTypes": [APP_MIME]}}}}
        init = rpc.call("initialize", init_params)
        live["initializeWithUiExtension"] = {"ok": "result" in init, "error": init.get("error"),
                                             "resultKeys": sorted((init.get("result") or {}).keys())}
        if "error" in init:
            init_params["capabilities"].pop("extensions")
            init = rpc.call("initialize", init_params)
            live["initializeWithoutExtension"] = {"ok": "result" in init, "error": init.get("error")}
        rpc.notify("initialized")
        start = rpc.call("thread/start", {"cwd": str(project), "ephemeral": True, "approvalPolicy": "never",
                                          "sandbox": "read-only"}, timeout=60)
        thread_id = ((start.get("result") or {}).get("thread") or {}).get("id")
        live["threadStart"] = {"ok": bool(thread_id), "error": start.get("error")}
        scope = {"threadId": thread_id} if thread_id else {}
        live["threadless"] = not thread_id
        row = None
        deadline = time.time() + 45
        while time.time() < deadline:
            status = rpc.call("mcpServerStatus/list", {**scope, "serverName": FIXTURE_NAME, "detail": "full"})
            rows = (status.get("result") or {}).get("data") or []
            row = next((r for r in rows if isinstance(r, dict) and r.get("name") == FIXTURE_NAME), None)
            live["status"] = {"error": status.get("error"), "rowSeen": row is not None}
            if row and row.get("tools"):
                break
            time.sleep(1)
        if row:
            tools = row.get("tools")
            tool_rows = list(tools.values()) if isinstance(tools, dict) else (tools or [])
            live["status"].update({
                "rowKeys": sorted(row.keys()),
                "runtimeStatus": row.get("runtimeStatus"),
                "authStatus": row.get("authStatus"),
                "toolNames": sorted(t.get("name") for t in tool_rows if isinstance(t, dict)),
                "toolUiMeta": {t.get("name"): (t.get("_meta") or {}).get("ui") for t in tool_rows
                               if isinstance(t, dict)},
                "resources": [r.get("uri") for r in row.get("resources") or [] if isinstance(r, dict)],
            })
        read = rpc.call("mcpServer/resource/read", {**scope, "server": FIXTURE_NAME, "uri": APP_URI})
        contents = (read.get("result") or {}).get("contents")
        body = resource_text(contents)
        live["resourceRead"] = {
            "ok": body is not None, "error": read.get("error"),
            "mimeType": (contents[0].get("mimeType") if contents else None),
            "metaKeys": sorted(((contents[0].get("_meta") or {}) if contents else {}).keys()),
            "sha256": sha256(body) if body is not None else None, "servedSha256": served_sha(oplog),
        }
        missing = rpc.call("mcpServer/resource/read", {**scope, "server": FIXTURE_NAME,
                                                       "uri": "ui://luna-ic0/missing.html"})
        live["resourceReadUndeclared"] = {"refused": "error" in missing, "error": missing.get("error")}
        if thread_id:
            read_call = rpc.call("mcpServer/tool/call", {"server": FIXTURE_NAME, "threadId": thread_id,
                                                         "tool": "ic0_counter_read", "arguments": {}})
            live["toolCallReadOnly"] = {"ok": "result" in read_call, "error": read_call.get("error"),
                                        "contentTypes": content_types(read_call.get("result")),
                                        "structured": (read_call.get("result") or {}).get("structuredContent")}
            before = increments(oplog)
            mutation = rpc.call("mcpServer/tool/call", {"server": FIXTURE_NAME, "threadId": thread_id,
                                                        "tool": "ic0_counter_increment", "arguments": {"by": 1}})
            time.sleep(0.3)
            live["toolCallMutation"] = {"ok": "result" in mutation, "error": mutation.get("error"),
                                        "contentTypes": content_types(mutation.get("result")),
                                        "fixtureIncrements": increments(oplog) - before}
            foreign = rpc.call("mcpServer/tool/call", {"server": FIXTURE_NAME, "threadId": str(uuid.uuid4()),
                                                       "tool": "ic0_counter_read", "arguments": {}})
            live["toolCallUnknownThread"] = {"refused": "error" in foreign, "error": foreign.get("error")}
    finally:
        process.close()
        live["stderrTail"] = [redact.text(line) for line in process.stderr_tail[-5:]]
    live["fixtureNegotiation"] = negotiated(oplog)
    record["live"] = live
    return record


# ---------------------------------------------------------------------------
# Claude Code: control channel of a run-shaped stream-json process
# ---------------------------------------------------------------------------

PROFILE = (ROOT / "crates/harness/src/claude/profile.rs").read_text()


def const_list(name: str) -> list[str]:
    body = PROFILE.split(f"pub const {name}: &[&str] = &[", 1)[1].split("];", 1)[0]
    return [part.strip().strip('"') for part in body.split(",") if part.strip().startswith('"')]


CLAUDE_BASE = [
    "--print", "--input-format", "stream-json", "--output-format", "stream-json",
    "--verbose", "--include-partial-messages", "--thinking-display", "summarized",
    "--replay-user-messages", "--permission-prompt-tool", "stdio",
]


def claude_modes() -> dict:
    grant = ",".join(const_list("ASK_AUTO_TOOL_GRANT"))
    return {
        "ask": ["--restricted", "--permission-mode", "acceptEdits", "--tools", grant],
        "full": ["--permission-mode", "bypassPermissions", "--dangerously-skip-permissions"],
    }


def claude_control(process: LineProcess, counter: list[int], request: dict, timeout: float = 30) -> dict:
    counter[0] += 1
    request_id = f"ic0-{counter[0]}"
    process.send({"type": "control_request", "request_id": request_id, "request": request})
    frame = process.wait_for(lambda f: f.get("type") == "control_response"
                             and (f.get("response") or {}).get("request_id") == request_id, timeout)
    if frame is None:
        return {"error": f"{request.get('subtype')}: no control_response within {timeout}s"}
    response = frame.get("response") or {}
    if response.get("subtype") == "error":
        return {"error": response.get("error")}
    return {"result": response.get("response")}


def probe_claude_mode(exe: str, work: Path, out: Path, mode: str, flags: list[str], redact: Redactor) -> dict:
    home = work / f"claude-home-{mode}"
    project = work / f"claude-project-{mode}"
    home.mkdir()
    project.mkdir()
    (home / ".claude.json").write_text(json.dumps({"hasCompletedOnboarding": True}))
    oplog = out / f"fixture-oplog-claude-{mode}.jsonl"
    oplog.unlink(missing_ok=True)
    argv = fixture_command(oplog, f"claude-{mode}")
    config = json.dumps({"mcpServers": {FIXTURE_NAME: {"type": "stdio", "command": argv[0], "args": argv[1:]}}})
    env = clean_env(("CLAUDE", "ANTHROPIC"))
    env.update({
        "CLAUDE_CONFIG_DIR": str(home), "ANTHROPIC_API_KEY": "sk-ant-ic0-probe-not-a-key",
        "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{closed_port()}", "DISABLE_AUTOUPDATER": "1",
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "ENABLE_CLAUDEAI_MCP_SERVERS": "false",
    })
    result: dict = {"mode": mode}
    counter = [0]
    process = LineProcess([exe, *CLAUDE_BASE, *flags, "--mcp-config", config], env, project)
    try:
        init = claude_control(process, counter, {"subtype": "initialize"})
        result["initialize"] = {"ok": "result" in init, "error": init.get("error"),
                                "resultKeys": sorted((init.get("result") or {}).keys())}
        row = None
        deadline = time.time() + 40
        while time.time() < deadline:
            status = claude_control(process, counter, {"subtype": "mcp_status"})
            servers = (status.get("result") or {}).get("mcpServers") or []
            row = next((s for s in servers if isinstance(s, dict) and s.get("name") == FIXTURE_NAME), None)
            result["status"] = {"error": status.get("error"), "rowSeen": row is not None,
                                "status": (row or {}).get("status")}
            if row and row.get("status") not in ("pending", None):
                break
            time.sleep(1)
        if row:
            tools = [t for t in row.get("tools") or [] if isinstance(t, dict)]
            result["status"].update({
                "rowKeys": sorted(row.keys()), "toolNames": sorted(t.get("name") for t in tools),
                "toolUiMeta": {t.get("name"): (t.get("_meta") or {}).get("ui") for t in tools},
                "toolAnnotations": {t.get("name"): t.get("annotations") for t in tools},
            })
        read = claude_control(process, counter, {"subtype": "mcp_read_resource", "serverName": FIXTURE_NAME,
                                                 "uri": APP_URI})
        payload = read.get("result") or {}
        contents = payload.get("contents") if isinstance(payload, dict) else None
        body = resource_text(contents)
        result["resourceRead"] = {
            "ok": body is not None, "error": read.get("error"), "payloadKeys": sorted(payload.keys())
            if isinstance(payload, dict) else None,
            "mimeType": contents[0].get("mimeType") if contents else None,
            "sha256": sha256(body) if body is not None else None, "servedSha256": served_sha(oplog),
        }
        non_ui = claude_control(process, counter, {"subtype": "mcp_read_resource", "serverName": FIXTURE_NAME,
                                                   "uri": "https://example.invalid/not-ui"})
        result["resourceReadNonUi"] = {"refused": "error" in non_ui, "error": non_ui.get("error")}
        read_call = claude_control(process, counter, {"subtype": "mcp_call",
                                                      "tool": f"mcp__{FIXTURE_NAME}__ic0_counter_read",
                                                      "arguments": {}})
        result["toolCallReadOnly"] = {"ok": "result" in read_call, "error": read_call.get("error"),
                                      "resultKeys": sorted((read_call.get("result") or {}).keys())
                                      if isinstance(read_call.get("result"), dict) else None,
                                      "contentTypes": content_types(read_call.get("result"))}
        before = increments(oplog)
        mutation = claude_control(process, counter, {"subtype": "mcp_call",
                                                     "tool": f"mcp__{FIXTURE_NAME}__ic0_counter_increment",
                                                     "arguments": {"by": 1}})
        time.sleep(0.3)
        result["toolCallMutation"] = {"ok": "result" in mutation, "error": mutation.get("error"),
                                      "contentTypes": content_types(mutation.get("result")),
                                      "result": redact.value(mutation.get("result")),
                                      "fixtureIncrements": increments(oplog) - before}
        unknown = claude_control(process, counter, {"subtype": "mcp_call", "tool": "mcp__ic0_absent__tool",
                                                    "arguments": {}})
        result["toolCallUnknownServer"] = {"refused": "error" in unknown, "error": unknown.get("error")}
        # The run's system/init carries the capability words; it is emitted
        # on the first user line. The closed base URL means no model answers.
        process.send({"type": "user", "message": {"role": "user", "content": "ic0 probe"}})
        frame = process.wait_for(lambda f: f.get("type") == "system" and f.get("subtype") == "init", 60)
        if frame:
            result["init"] = {
                "claudeCodeVersion": frame.get("claude_code_version"),
                "capabilities": sorted(frame.get("capabilities") or []),
                "fixtureTools": sorted(t for t in frame.get("tools") or [] if t.startswith(f"mcp__{FIXTURE_NAME}__")),
                "mcpServers": sorted(({"name": s.get("name"), "status": s.get("status")}
                                      for s in frame.get("mcp_servers") or [] if isinstance(s, dict)),
                                     key=lambda s: s["name"] or ""),
            }
        else:
            result["init"] = {"error": "no system/init frame within 60s"}
    finally:
        process.close(grace=2)
        result["stderrTail"] = [redact.text(line) for line in process.stderr_tail[-5:]]
    result["fixtureNegotiation"] = negotiated(oplog)
    return result


def probe_claude(work: Path, out: Path, redact: Redactor) -> dict:
    exe = shutil.which("claude")
    if not exe:
        return {"installed": False}
    record = {"installed": True, "executable": redact.text(exe),
              "version": run([exe, "--version"]).get("stdout"), "modes": []}
    for mode, flags in claude_modes().items():
        record["modes"].append(probe_claude_mode(exe, work, out, mode, flags, redact))
    return record


# ---------------------------------------------------------------------------
# OpenCode: route table and MCP inventory of an isolated `opencode serve`
# ---------------------------------------------------------------------------

def http(base: str, auth: str, path: str, directory: str | None = None, timeout: float = 10):
    request = urllib.request.Request(base + path, headers={"Authorization": auth, "Accept": "application/json"})
    if directory:
        request.add_header("x-opencode-directory", urllib.parse.quote(directory, safe="/-_.~"))
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()
    except (urllib.error.URLError, OSError) as error:
        return None, str(error).encode()


def probe_opencode(work: Path, out: Path, redact: Redactor) -> dict:
    exe = shutil.which("opencode") or (str(Path.home() / ".opencode/bin/opencode")
                                       if (Path.home() / ".opencode/bin/opencode").exists() else None)
    if not exe:
        return {"installed": False}
    root = work / "opencode"
    project = root / "project"
    for name in ("home", "config", "data", "cache", "state", "project"):
        (root / name).mkdir(parents=True)
    oplog = out / "fixture-oplog-opencode.jsonl"
    oplog.unlink(missing_ok=True)
    password = uuid.uuid4().hex
    env = clean_env(("OPENCODE", "XDG_"))
    env.update({
        "HOME": str(root / "home"), "XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"),
        "XDG_CACHE_HOME": str(root / "cache"), "XDG_STATE_HOME": str(root / "state"),
        "OPENCODE_PASSWORD": password, "OPENCODE_SERVER_PASSWORD": password, "OPENCODE_CLIENT": "luna",
        "OPENCODE_DISABLE_EMBEDDED_WEB_UI": "1", "OPENCODE_DISABLE_MODELS_FETCH": "1",
        "OPENCODE_DISABLE_PROJECT_CONFIG": "1",
        "OPENCODE_CONFIG_CONTENT": json.dumps({"mcp": {"servers": {FIXTURE_NAME: {
            "type": "local", "command": fixture_command(oplog, "opencode"), "codemode": False}}}}),
    })
    record = {"installed": True, "executable": redact.text(exe),
              "version": run([exe, "--version"], env=env).get("stdout")}
    auth = "Basic " + base64.b64encode(f"opencode:{password}".encode()).decode()
    proc = subprocess.Popen([exe, "serve", "--stdio", "--port", "0"], env=env, cwd=project, stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    urls: queue.Queue = queue.Queue()

    def read_stdout():
        for line in proc.stdout:
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict) and isinstance(value.get("url"), str):
                urls.put(value["url"].rstrip("/"))

    threading.Thread(target=read_stdout, daemon=True).start()
    live: dict = {}
    try:
        try:
            base = urls.get(timeout=90)
        except queue.Empty:
            live["error"] = "opencode serve printed no url within 90s"
            return {**record, "live": live}
        deadline = time.time() + 60
        info = (None, b"")
        while time.time() < deadline:
            info = http(base, auth, "/api/info")
            if info[0] == 200:
                break
            time.sleep(0.5)
        info_body = json_or_none(info[1]) if info[0] == 200 else None
        live["info"] = {"status": info[0],
                        "version": info_body.get("version") if isinstance(info_body, dict) else None}
        status, body = http(base, auth, "/openapi.json")
        routes: list[str] = []
        document = json_or_none(body) if status == 200 else None
        if isinstance(document, dict):
            for path, operations in (document.get("paths") or {}).items():
                path = re.sub(r":([A-Za-z_][A-Za-z0-9_]*)", r"{\1}", path)
                for method in operations if isinstance(operations, dict) else {}:
                    if "mcp" in path.lower() and method in ("get", "post", "put", "patch", "delete"):
                        routes.append(f"{method.upper()} {path}")
        live["openapi"] = {"status": status, "mcpRoutes": sorted(routes),
                           "sha256": sha256(body) if status == 200 else None}
        row = None
        deadline = time.time() + 45
        while time.time() < deadline:
            status, body = http(base, auth, "/api/mcp", str(project))
            text = body.decode(errors="replace")
            payload = json_or_none(body)
            rows = payload.get("data") if isinstance(payload, dict) else payload
            row = next((r for r in rows or [] if isinstance(r, dict) and r.get("name") == FIXTURE_NAME), None)
            state = ((row or {}).get("status") or {}).get("status") if isinstance((row or {}).get("status"), dict) \
                else (row or {}).get("status")
            live["mcpList"] = {"status": status, "rowSeen": row is not None, "serverStatus": state,
                               "mentionsUiMeta": ("resourceUri" in text) or ("ui://" in text)}
            if state == "connected":
                break
            time.sleep(1)
        if row:
            live["mcpList"]["rowKeys"] = sorted(row.keys())
        status, body = http(base, auth, "/api/mcp/resource", str(project))
        text = body.decode(errors="replace")
        live["resourceCatalog"] = {"status": status, "listsAppResource": APP_URI in text,
                                   "body": redact.text(text[:1500])}
    finally:
        try:
            proc.stdin.close()
        except OSError:
            pass
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
    live["fixtureNegotiation"] = negotiated(oplog)
    live["fixtureResourceReads"] = sum(1 for r in read_oplog(oplog) if r.get("method") == "resources/read")
    live["fixtureToolCalls"] = sum(1 for r in read_oplog(oplog) if r.get("method") == "tools/call")
    record["live"] = live
    return record


# ---------------------------------------------------------------------------
# Expectations
# ---------------------------------------------------------------------------

def check(rows: list, provider: str, fact: str, expected, observed, ok: bool):
    rows.append({"provider": provider, "fact": fact, "expected": expected, "observed": observed,
                 "status": "match" if ok else "drift"})


def evaluate(providers: dict) -> list:
    rows: list = []
    codex = providers.get("codex")
    if codex is not None:
        if not codex.get("installed"):
            check(rows, "codex", "installed", True, False, False)
        else:
            schema = codex.get("schema") or {}
            want = EXPECTED["codex"]
            for fact, key in (("clientMethods", "clientMethods"), ("resourceReadParams", "resourceReadParams"),
                              ("toolCallParams", "toolCallParams"), ("mcpToolCallItemFields", "mcpToolCallItemFields"),
                              ("initializeCapabilityFields", "initializeCapabilityFields"),
                              ("dynamicToolContentTypes", "dynamicToolContentTypes")):
                missing = sorted(set(want[fact]) - set(schema.get(key) or []))
                check(rows, "codex", f"schema.{fact}", want[fact], {"missing": missing}, not missing)
            live = codex.get("live") or {}
            check(rows, "codex", "live.threadStart", True, live.get("threadStart"),
                  bool((live.get("threadStart") or {}).get("ok")))
            read = live.get("resourceRead") or {}
            check(rows, "codex", "live.resourceRead byte-identical", True,
                  {"sha256": read.get("sha256"), "served": read.get("servedSha256"), "error": read.get("error")},
                  bool(read.get("ok")) and read.get("sha256") == read.get("servedSha256"))
            check(rows, "codex", "live.resourceReadUndeclared refused", True, live.get("resourceReadUndeclared"),
                  bool((live.get("resourceReadUndeclared") or {}).get("refused")))
            check(rows, "codex", "live.toolCallReadOnly", True, live.get("toolCallReadOnly"),
                  bool((live.get("toolCallReadOnly") or {}).get("ok")))
            mutation = live.get("toolCallMutation") or {}
            check(rows, "codex", "live.toolCallMutation applied once", 1, mutation.get("fixtureIncrements"),
                  mutation.get("ok") is True and mutation.get("fixtureIncrements") == 1)
            check(rows, "codex", "live.toolCallUnknownThread refused", True, live.get("toolCallUnknownThread"),
                  bool((live.get("toolCallUnknownThread") or {}).get("refused")))
    claude = providers.get("claude")
    if claude is not None:
        if not claude.get("installed"):
            check(rows, "claude", "installed", True, False, False)
        for mode in claude.get("modes") or []:
            name = mode["mode"]
            capabilities = (mode.get("init") or {}).get("capabilities") or []
            missing = sorted(set(EXPECTED["claude"]["capabilities"]) - set(capabilities))
            check(rows, "claude", f"{name}.init.capabilities", EXPECTED["claude"]["capabilities"],
                  {"missing": missing, "observed": capabilities}, not missing and bool(capabilities))
            check(rows, "claude", f"{name}.mcp_status fixture row", True, mode.get("status"),
                  bool((mode.get("status") or {}).get("rowSeen")))
            read = mode.get("resourceRead") or {}
            check(rows, "claude", f"{name}.mcp_read_resource byte-identical", True,
                  {"sha256": read.get("sha256"), "served": read.get("servedSha256"), "error": read.get("error")},
                  bool(read.get("ok")) and read.get("sha256") == read.get("servedSha256"))
            check(rows, "claude", f"{name}.mcp_read_resource non-ui refused", True, mode.get("resourceReadNonUi"),
                  bool((mode.get("resourceReadNonUi") or {}).get("refused")))
            check(rows, "claude", f"{name}.mcp_call read-only", True, mode.get("toolCallReadOnly"),
                  bool((mode.get("toolCallReadOnly") or {}).get("ok")))
            mutation = mode.get("toolCallMutation") or {}
            check(rows, "claude", f"{name}.mcp_call mutation applied once", 1, mutation.get("fixtureIncrements"),
                  mutation.get("ok") is True and mutation.get("fixtureIncrements") == 1)
    opencode = providers.get("opencode")
    if opencode is not None:
        if not opencode.get("installed"):
            check(rows, "opencode", "installed", True, False, False)
        else:
            live = opencode.get("live") or {}
            routes = (live.get("openapi") or {}).get("mcpRoutes") or []
            paths = sorted({route.split(" ", 1)[1] for route in routes})
            host_ops = [route for route in routes if re.search(r"(read|call|invoke|tool)", route.split(" ", 1)[1], re.I)]
            check(rows, "opencode", "openapi.mcpRoutes", EXPECTED["opencode"]["mcpRoutes"], paths,
                  paths == EXPECTED["opencode"]["mcpRoutes"])
            check(rows, "opencode", "no host resource-read/tool-call route", [], host_ops, not host_ops)
            check(rows, "opencode", "fixture connected", "connected", (live.get("mcpList") or {}).get("serverStatus"),
                  (live.get("mcpList") or {}).get("serverStatus") == "connected")
            check(rows, "opencode", "probe made no fixture tool call", 0, live.get("fixtureToolCalls"),
                  live.get("fixtureToolCalls") == 0)
    return rows


def main() -> int:
    args = sys.argv[1:]
    if not args or args[0].startswith("-") or ("--only" in args and args.index("--only") + 1 >= len(args)):
        print("usage: qa-ic0-provider-probe.py <evidence-dir> [--only codex,claude,opencode]", file=sys.stderr)
        return 2
    out = Path(args[0]).resolve()
    only = {"codex", "claude", "opencode"}
    if "--only" in args:
        only = set(args[args.index("--only") + 1].split(",")) & only
    out.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="ic0-provider-probe-"))
    redact = Redactor(work)
    started = time.time()
    providers: dict = {}
    try:
        for name, probe in (("codex", probe_codex), ("claude", probe_claude), ("opencode", probe_opencode)):
            if name not in only:
                continue
            began = time.time()
            try:
                providers[name] = redact.value(probe(work, out, redact))
            except Exception as error:  # a crashed probe is recorded, never hidden
                providers[name] = {"installed": True, "probeError": redact.text(f"{type(error).__name__}: {error}")}
            providers[name]["seconds"] = round(time.time() - began, 1)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    rows = evaluate(providers)
    drift = [f"{row['provider']}: {row['fact']}" for row in rows if row["status"] != "match"]
    sha = run(["git", "-C", str(ROOT), "rev-parse", "HEAD"]).get("stdout")
    record = {
        "evidenceKind": "ic0-provider-probe",
        "repoSha": sha,
        "capturedAt": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "platform": {"system": platform.system(), "release": platform.release(), "machine": platform.machine(),
                     "macVersion": platform.mac_ver()[0] or None, "python": platform.python_version()},
        "fixture": {"path": str(FIXTURE.relative_to(ROOT)), "sha256": sha256(FIXTURE.read_bytes()),
                    "server": FIXTURE_NAME, "appUri": APP_URI},
        "expected": EXPECTED,
        "providers": providers,
        "expectations": rows,
        "drift": drift,
        "seconds": round(time.time() - started, 1),
    }
    (out / "ic0-provider-probe.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"passed": not drift, "providers": sorted(providers), "drift": drift,
                      "evidence": str(out / "ic0-provider-probe.json")}))
    return 0 if not drift else 1


if __name__ == "__main__":
    sys.exit(main())
