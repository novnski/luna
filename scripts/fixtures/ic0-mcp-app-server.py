#!/usr/bin/env python3
"""IC-0 disposable MCP Apps fixture server (stdio, newline-delimited JSON-RPC).

One process per provider launch. It declares one SEP-1865 app view
(`ui://luna-ic0/app.html`, `text/html;profile=mcp-app`) on three tools:

- `ic0_show_app`         read-only, visibility model+app, opens the view;
- `ic0_counter_read`     read-only, app-only visibility;
- `ic0_counter_increment` mutation of this process's in-memory counter only.
  Its result carries a text block, a 1x1 PNG `image` block and
  `structuredContent`, so a probe can see whether a provider's MCP client
  keeps image content.

Every request and notification is appended to the operation log given as
argv[1] (the authoritative server-side readback): the client's `initialize`
capabilities (MCP Apps negotiation is `capabilities.extensions
["io.modelcontextprotocol/ui"]`), each `resources/read` with the SHA-256 of
the exact bytes served, and each `tools/call` with the counter before/after.
Nothing here reaches a network, a file outside the log, or a real service.

    python3 -I scripts/fixtures/ic0-mcp-app-server.py <op-log.jsonl> [label]
"""
import hashlib
import json
import sys
import time

LOG = sys.argv[1] if len(sys.argv) > 1 else None
LABEL = sys.argv[2] if len(sys.argv) > 2 else "fixture"
NAME = "luna-ic0-fixture"
APP_URI = "ui://luna-ic0/app.html"
APP_MIME = "text/html;profile=mcp-app"
VERSIONS = ("2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05")
PNG_1X1 = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQ"
    "DwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
)
APP_HTML = """<!doctype html>
<meta charset="utf-8">
<title>LUNA IC-0 fixture app</title>
<style>body{font:13px system-ui;margin:12px}output{font-weight:600}</style>
<p>IC-0 fixture view. Counter: <output id="counter">?</output></p>
<button id="read">Read</button> <button id="increment">Increment</button>
<script>
// Standard MCP Apps view: JSON-RPC over postMessage to the host (parent).
let next = 1; const pending = new Map();
function request(method, params) {
  const id = next++;
  window.parent.postMessage({ jsonrpc: "2.0", id, method, params }, "*");
  return new Promise((resolve) => pending.set(id, resolve));
}
window.addEventListener("message", (event) => {
  if (event.source !== window.parent) return;
  const message = event.data;
  if (message && pending.has(message.id)) { pending.get(message.id)(message); pending.delete(message.id); }
});
async function call(tool) {
  const reply = await request("tools/call", { name: tool, arguments: {} });
  const value = reply.result && reply.result.structuredContent && reply.result.structuredContent.counter;
  document.getElementById("counter").textContent = String(value);
}
document.getElementById("read").onclick = () => call("ic0_counter_read");
document.getElementById("increment").onclick = () => call("ic0_counter_increment");
request("ui/initialize", { appInfo: { name: "luna-ic0-fixture-app", version: "1" }, appCapabilities: {} });
</script>
"""

APP_META = {"ui": {"resourceUri": APP_URI, "visibility": ["model", "app"]}, "ui/resourceUri": APP_URI}
TOOLS = [
    {
        "name": "ic0_show_app",
        "title": "Show the IC-0 fixture app",
        "description": "Read-only. Shows the IC-0 fixture MCP Apps view with the current counter.",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True},
        "_meta": APP_META,
    },
    {
        "name": "ic0_counter_read",
        "description": "Read-only. Returns the fixture counter (app-only visibility).",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True},
        "_meta": {"ui": {"resourceUri": APP_URI, "visibility": ["app"]}},
    },
    {
        "name": "ic0_counter_increment",
        "description": "Mutation of this disposable fixture process only: adds `by` (default 1) to its counter.",
        "inputSchema": {"type": "object", "properties": {"by": {"type": "integer"}}},
        "annotations": {"readOnlyHint": False, "destructiveHint": False, "idempotentHint": False},
        "_meta": {"ui": {"resourceUri": APP_URI, "visibility": ["app"]}},
    },
]

state = {"counter": 0}


def bounded(value, limit=2000):
    text = json.dumps(value, sort_keys=True, default=str)
    return value if len(text) <= limit else {"truncatedJson": text[:limit]}


def log(entry):
    if not LOG:
        return
    entry = dict(entry, t=round(time.time(), 3), label=LABEL)
    with open(LOG, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(entry, sort_keys=True) + "\n")


def reply(message_id, result=None, error=None):
    frame = {"jsonrpc": "2.0", "id": message_id}
    if error is not None:
        frame["error"] = error
    else:
        frame["result"] = result
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def call_tool(params):
    name = params.get("name")
    before = state["counter"]
    if name == "ic0_show_app":
        result = {
            "content": [{"type": "text", "text": f"IC-0 app ready; counter={before}"}],
            "structuredContent": {"counter": before},
            "_meta": {"ui": {"resourceUri": APP_URI}},
        }
    elif name == "ic0_counter_read":
        result = {"content": [{"type": "text", "text": f"counter={before}"}], "structuredContent": {"counter": before}}
    elif name == "ic0_counter_increment":
        by = (params.get("arguments") or {}).get("by", 1)
        if not isinstance(by, int) or isinstance(by, bool) or not 1 <= by <= 100:
            return None, {"code": -32602, "message": "`by` must be an integer from 1 to 100"}
        state["counter"] = before + by
        result = {
            "content": [
                {"type": "text", "text": f"counter={state['counter']}"},
                {"type": "image", "data": PNG_1X1, "mimeType": "image/png"},
            ],
            "structuredContent": {"counter": state["counter"]},
        }
    else:
        return None, {"code": -32602, "message": f"Unknown tool: {name}"}
    log({"method": "tools/call", "tool": name, "counterBefore": before, "counterAfter": state["counter"],
         "meta": bounded(params.get("_meta"), 600)})
    return result, None


def handle(message):
    method = message.get("method")
    message_id = message.get("id")
    params = message.get("params") or {}
    if message_id is None:
        log({"method": method, "notification": True})
        return
    if method == "initialize":
        requested = params.get("protocolVersion")
        log({"method": method, "protocolVersion": requested, "clientInfo": bounded(params.get("clientInfo"), 400),
             "capabilities": bounded(params.get("capabilities"), 2000)})
        reply(message_id, {
            "protocolVersion": requested if requested in VERSIONS else "2025-06-18",
            "capabilities": {"tools": {"listChanged": False}, "resources": {"listChanged": False, "subscribe": False}},
            "serverInfo": {"name": NAME, "version": "1"},
        })
    elif method == "ping":
        reply(message_id, {})
    elif method == "tools/list":
        log({"method": method})
        reply(message_id, {"tools": TOOLS})
    elif method == "resources/list":
        log({"method": method})
        reply(message_id, {"resources": [{"uri": APP_URI, "name": "ic0-app", "mimeType": APP_MIME}]})
    elif method == "resources/templates/list":
        reply(message_id, {"resourceTemplates": []})
    elif method == "prompts/list":
        reply(message_id, {"prompts": []})
    elif method == "resources/read":
        uri = params.get("uri")
        found = uri == APP_URI
        log({"method": method, "uri": uri, "found": found,
             "servedSha256": hashlib.sha256(APP_HTML.encode()).hexdigest() if found else None})
        if not found:
            reply(message_id, error={"code": -32002, "message": "Resource not found"})
            return
        reply(message_id, {"contents": [{
            "uri": APP_URI, "mimeType": APP_MIME, "text": APP_HTML,
            "_meta": {"ui": {"csp": {"connectDomains": [], "resourceDomains": []}, "prefersBorder": True}},
        }]})
    elif method == "tools/call":
        result, error = call_tool(params)
        reply(message_id, result, error)
    else:
        log({"method": method, "unsupported": True})
        reply(message_id, error={"code": -32601, "message": f"Method not found: {method}"})


def main():
    log({"method": "<start>", "argv": sys.argv[2:]})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            log({"method": "<invalid-json>"})
            continue
        if isinstance(message, dict) and "method" in message:
            handle(message)
    log({"method": "<eof>"})


if __name__ == "__main__":
    main()
