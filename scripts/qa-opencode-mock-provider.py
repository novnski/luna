#!/usr/bin/env python3
"""T2 mock OpenAI-compatible provider for OpenCode QA (OC-01).

Contract: docs/reference/qa-infrastructure.md, "OpenCode QA". Standard
library only; no model cost, deterministic replies.

  serve        GET /v1/models, POST /v1/chat/completions (streaming or not),
               GET /__mock/state, POST /__mock/script, POST /__mock/reset
  write-config writes <profile>/config/opencode/opencode.json pointing a
               custom `luna-mock` provider at a running mock

Scenario selection per request (first match wins):
  1. the last message is a tool result      -> `after-tool` text reply
  2. the request offers no tools            -> `auxiliary` text (title,
                                               summary, compaction agents);
                                               never consumes the script
  3. a [[mock:<name>]] or [[mock:<name>:<arg>]] marker in the latest user
     message (the last marker wins)
  4. the next queued script entry (--script file or POST /__mock/script)
  5. --scenario (default `text`)

Script entries are JSON objects: {"scenario": "tool-shell", "command": "..."},
{"scenario": "tool", "name": "write", "arguments": {...}},
{"scenario": "text", "text": "..."}; any scenario name below is valid.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import itertools
import json
import os
import re
import signal
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

SCENARIOS = {
    "text": "plain streamed text",
    "reasoning": "reasoning_content deltas, then text",
    "slow": "text streamed slowly (steer/stop tests)",
    "tool-shell": "one `shell` tool call (arg: the command)",
    "tool-shell-background": "one background `shell` tool call",
    "tool": "a scripted tool call: {name, arguments}",
    "subagent": "one foreground `subagent` tool call",
    "subagent-background": "one background `subagent` tool call",
    "image": "reply with the sha256/media type of every image received",
    "error-401": "HTTP 401 invalid_api_key",
    "error-429": "HTTP 429 rate_limit_exceeded, every attempt",
    "error-429-once": "HTTP 429 once per conversation, then text",
    "error-500": "HTTP 500, every attempt",
    "error-500-once": "HTTP 500 once per conversation, then text",
    "context-overflow": "HTTP 400 context_length_exceeded",
    "drop": "close the stream mid-reply without [DONE]",
}
MARKER = re.compile(r"\[\[mock:([a-z0-9-]+)(?::([^\]]*))?\]\]")
MOCK_KEY = "luna-qa-mock-key-not-secret"
PROVIDER_ID = "luna-mock"


def now_iso() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime()) + f".{int(time.time() * 1000) % 1000:03d}Z"


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def text_of(content) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(part.get("text", "") for part in content if isinstance(part, dict) and part.get("type") == "text")
    return ""


class State:
    def __init__(self, default: str, script: list[dict], log: Path | None, subagent: str, slow_ms: int):
        self.default = default
        self.queue = list(script)
        self.log = log
        self.subagent = subagent
        self.slow_ms = slow_ms
        self.lock = threading.Lock()
        self.seq = itertools.count(1)
        self.counts: dict[str, int] = {}
        self.failed_once: set[str] = set()

    def record(self, entry: dict) -> None:
        with self.lock:
            self.counts[entry["scenario"]] = self.counts.get(entry["scenario"], 0) + 1
            if self.log:
                with self.log.open("a") as handle:
                    handle.write(json.dumps(entry, sort_keys=True) + "\n")

    def pop(self) -> dict | None:
        with self.lock:
            return self.queue.pop(0) if self.queue else None


def validate_entry(entry) -> dict:
    if not isinstance(entry, dict) or entry.get("scenario") not in SCENARIOS:
        raise ValueError(f"script entry needs a known scenario: {entry!r}")
    if entry["scenario"] == "tool" and not isinstance(entry.get("name"), str):
        raise ValueError(f"`tool` entries need a name: {entry!r}")
    return entry


def images_after_last_assistant(messages: list) -> list[dict]:
    tail = []
    for message in reversed(messages):
        if message.get("role") == "assistant":
            break
        tail.append(message)
    found = []
    for message in reversed(tail):
        content = message.get("content")
        if not isinstance(content, list):
            continue
        for part in content:
            if not isinstance(part, dict) or part.get("type") != "image_url":
                continue
            url = part.get("image_url", {}).get("url", "") if isinstance(part.get("image_url"), dict) else str(part.get("image_url"))
            match = re.match(r"data:([^;,]+)?(;base64)?,(.*)", url, re.S)
            if match:
                raw = base64.b64decode(match.group(3)) if match.group(2) else match.group(3).encode()
                found.append({"mime": match.group(1) or "", "bytes": len(raw), "sha256": sha256(raw)})
            else:
                found.append({"mime": "", "url": url[:200], "sha256": None})
    return found


def tool_arguments(tool: dict, wanted: dict) -> dict:
    """Keep only properties the offered tool declares; fill required strings."""
    params = tool.get("function", {}).get("parameters", {}) or {}
    props = params.get("properties", {}) or {}
    args = {key: value for key, value in wanted.items() if key in props}
    for key in params.get("required", []) or []:
        if key not in args and props.get(key, {}).get("type") == "string":
            args[key] = f"luna-mock-{key}"
    return args


def plan(state: State, body: dict) -> dict:
    """Decide the reply for one chat completion request."""
    messages = body.get("messages") or []
    tools = {t.get("function", {}).get("name"): t for t in body.get("tools") or [] if isinstance(t, dict)}
    last = messages[-1] if messages else {}
    if last.get("role") == "tool":
        output = text_of(last.get("content"))
        return {"scenario": "after-tool", "source": "after-tool", "kind": "text",
                "text": f"MOCK_TOOL_RESULT {output[:400]}"}
    if not tools:
        return {"scenario": "auxiliary", "source": "no-tools", "kind": "text", "text": "LUNA mock title"}
    user = next((m for m in reversed(messages) if m.get("role") == "user"), {})
    markers = MARKER.findall(text_of(user.get("content")))
    entry, source = None, "default"
    if markers and markers[-1][0] in SCENARIOS:
        name, arg = markers[-1]
        entry, source = {"scenario": name, "arg": arg or None}, "marker"
    elif markers:
        return {"scenario": "unknown-marker", "source": "marker", "kind": "error", "status": 400,
                "body": {"error": {"message": f"LUNA mock: unknown scenario {markers[-1][0]!r}", "type": "invalid_request_error"}}}
    if entry is None:
        entry = state.pop()
        source = "script" if entry else "default"
    if entry is None:
        entry = {"scenario": state.default}
    name = entry["scenario"]
    conversation = sha256(json.dumps(messages[:2], sort_keys=True).encode())[:16]
    result = {"scenario": name, "source": source}
    if name in ("text", "slow"):
        return {**result, "kind": "text", "text": entry.get("text") or entry.get("arg") or "MOCK_TEXT_OK luna mock reply.",
                "slow": name == "slow"}
    if name == "reasoning":
        return {**result, "kind": "text", "reasoning": "MOCK_REASONING thinking about the request.",
                "text": entry.get("text") or "MOCK_REASONING_DONE"}
    if name == "image":
        images = images_after_last_assistant(messages)
        described = " ".join(f"[sha256={i['sha256']} mime={i['mime']} bytes={i.get('bytes')}]" for i in images)
        return {**result, "kind": "text", "text": f"MOCK_IMAGE count={len(images)} {described}".strip(), "images": images}
    if name in ("tool-shell", "tool-shell-background", "subagent", "subagent-background", "tool"):
        if name.startswith("tool-shell"):
            tool_name = next((n for n in ("shell", "bash") if n in tools), None)
            wanted = {"command": entry.get("command") or entry.get("arg") or "echo luna-mock-shell-ok",
                      "description": "Print the LUNA mock marker"}
            if name.endswith("background"):
                wanted["background"] = True
        elif name.startswith("subagent"):
            tool_name = next((n for n in ("subagent", "task") if n in tools), None)
            wanted = {"agent": entry.get("agent") or state.subagent, "subagent_type": entry.get("agent") or state.subagent,
                      "description": entry.get("description") or "LUNA mock child",
                      "prompt": entry.get("prompt") or entry.get("arg") or "[[mock:text:MOCK_CHILD_OK]] Child task from the LUNA mock."}
            if name.endswith("background"):
                wanted["background"] = True
        else:
            tool_name = entry["name"] if entry["name"] in tools else None
            wanted = entry.get("arguments") or {}
        if tool_name is None:
            return {**result, "scenario": f"{name}:unscripted-tool", "kind": "text",
                    "text": f"MOCK_TOOL_UNAVAILABLE requested={name} offered={sorted(tools)}"}
        args = wanted if name == "tool" else tool_arguments(tools[tool_name], wanted)
        return {**result, "kind": "tool", "tool": tool_name, "arguments": args}
    if name == "error-401":
        return {**result, "kind": "error", "status": 401, "body": {"error": {
            "message": "Incorrect API key provided (LUNA mock).", "type": "invalid_request_error", "code": "invalid_api_key"}}}
    if name in ("error-429", "error-429-once", "error-500", "error-500-once"):
        once = name.endswith("-once")
        key = f"{name}:{conversation}"
        with state.lock:
            already = key in state.failed_once
            state.failed_once.add(key)
        if once and already:
            return {**result, "kind": "text", "text": f"MOCK_RECOVERED after {name}"}
        if name.startswith("error-429"):
            return {**result, "kind": "error", "status": 429, "headers": {"retry-after": "1"}, "body": {"error": {
                "message": "Rate limit reached (LUNA mock).", "type": "rate_limit_error", "code": "rate_limit_exceeded"}}}
        return {**result, "kind": "error", "status": 500, "body": {"error": {
            "message": "The server had an error while processing your request (LUNA mock).", "type": "server_error"}}}
    if name == "context-overflow":
        return {**result, "kind": "error", "status": 400, "body": {"error": {
            "message": "This model's maximum context length is 128000 tokens. However, your messages resulted in "
                       "200000 tokens. Please reduce the length of the messages.",
            "type": "invalid_request_error", "param": "messages", "code": "context_length_exceeded"}}}
    if name == "drop":
        return {**result, "kind": "drop", "text": "MOCK_PARTIAL this reply is cut off"}
    raise AssertionError(name)


class Handler(BaseHTTPRequestHandler):
    server_version = "LunaMockProvider/1"
    state: State

    def log_message(self, fmt, *args):  # quiet; the JSONL log is the record
        pass

    def send_json(self, status: int, payload, headers: dict | None = None) -> None:
        data = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)

    def read_body(self) -> bytes:
        length = int(self.headers.get("content-length") or 0)
        return self.rfile.read(length) if length else b""

    def do_GET(self):
        if self.path.rstrip("/") in ("/v1/models", "/models"):
            self.send_json(200, {"object": "list", "data": [
                {"id": "mock-model", "object": "model", "created": 0, "owned_by": PROVIDER_ID}]})
        elif self.path == "/__mock/state":
            with self.state.lock:
                self.send_json(200, {"counts": dict(self.state.counts), "queue": len(self.state.queue),
                                     "default": self.state.default})
        else:
            self.send_json(404, {"error": {"message": "not found"}})

    def do_POST(self):
        raw = self.read_body()
        if self.path == "/__mock/script":
            try:
                payload = json.loads(raw or b"{}")
                entries = [validate_entry(e) for e in (payload.get("entries") if isinstance(payload, dict) and "entries" in payload else [payload])]
            except (ValueError, json.JSONDecodeError) as error:
                self.send_json(400, {"error": str(error)})
                return
            with self.state.lock:
                self.state.queue.extend(entries)
                self.send_json(200, {"queued": len(entries), "queue": len(self.state.queue)})
            return
        if self.path == "/__mock/reset":
            with self.state.lock:
                self.state.queue.clear()
                self.state.counts.clear()
                self.state.failed_once.clear()
            self.send_json(200, {"reset": True})
            return
        if self.path.rstrip("/") not in ("/v1/chat/completions", "/chat/completions"):
            self.send_json(404, {"error": {"message": f"LUNA mock does not serve {self.path}"}})
            return
        try:
            body = json.loads(raw)
        except json.JSONDecodeError:
            self.send_json(400, {"error": {"message": "invalid JSON"}})
            return
        decision = plan(self.state, body)
        seq = next(self.state.seq)
        entry = {
            "ts": now_iso(), "seq": seq, "path": self.path, "scenario": decision["scenario"],
            "source": decision["source"], "model": body.get("model"), "stream": bool(body.get("stream")),
            "toolNames": sorted(t.get("function", {}).get("name") for t in body.get("tools") or [] if isinstance(t, dict)),
            "messageCount": len(body.get("messages") or []),
            "lastRole": (body.get("messages") or [{}])[-1].get("role"),
            "reasoningEffort": body.get("reasoning_effort"),
            "authorizationPresent": bool(self.headers.get("authorization")),
            "bodySha256": sha256(raw), "response": {"kind": decision["kind"]},
        }
        if decision.get("images") is not None:
            entry["images"] = decision["images"]
        if decision["kind"] == "error":
            entry["response"]["status"] = decision["status"]
            self.state.record(entry)
            self.send_json(decision["status"], decision["body"], decision.get("headers"))
            return
        if decision["kind"] == "tool":
            entry["response"].update(tool=decision["tool"], argumentKeys=sorted(decision["arguments"]))
        else:
            entry["response"]["textSha256"] = sha256(decision.get("text", "").encode())
        self.state.record(entry)
        model = body.get("model") or "mock-model"
        prompt_tokens = max(1, len(raw) // 4)
        if body.get("stream"):
            self.stream(decision, model, seq, prompt_tokens, bool((body.get("stream_options") or {}).get("include_usage")))
        else:
            self.complete(decision, model, seq, prompt_tokens)

    def complete(self, decision: dict, model: str, seq: int, prompt_tokens: int) -> None:
        message: dict = {"role": "assistant", "content": None}
        finish = "stop"
        if decision["kind"] == "tool":
            message["tool_calls"] = [{"id": f"call_mock_{seq}", "type": "function", "function": {
                "name": decision["tool"], "arguments": json.dumps(decision["arguments"])}}]
            finish = "tool_calls"
        else:
            message["content"] = decision.get("text", "")
            if decision.get("reasoning"):
                message["reasoning_content"] = decision["reasoning"]
        completion = len(json.dumps(message)) // 4
        self.send_json(200, {"id": f"chatcmpl-mock-{seq}", "object": "chat.completion", "created": int(time.time()),
                             "model": model, "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                             "usage": {"prompt_tokens": prompt_tokens, "completion_tokens": completion,
                                       "total_tokens": prompt_tokens + completion}})

    def stream(self, decision: dict, model: str, seq: int, prompt_tokens: int, usage: bool) -> None:
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()
        created = int(time.time())

        def chunk(delta: dict, finish=None, extra: dict | None = None):
            payload = {"id": f"chatcmpl-mock-{seq}", "object": "chat.completion.chunk", "created": created,
                       "model": model, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
            if extra:
                payload.update(extra)
            self.wfile.write(b"data: " + json.dumps(payload).encode() + b"\n\n")
            self.wfile.flush()

        completion = 0
        try:
            chunk({"role": "assistant", "content": ""})
            if decision["kind"] == "tool":
                arguments = json.dumps(decision["arguments"])
                half = len(arguments) // 2
                chunk({"tool_calls": [{"index": 0, "id": f"call_mock_{seq}", "type": "function",
                                       "function": {"name": decision["tool"], "arguments": ""}}]})
                for piece in (arguments[:half], arguments[half:]):
                    chunk({"tool_calls": [{"index": 0, "function": {"arguments": piece}}]})
                completion = len(arguments) // 4 + 8
                finish = "tool_calls"
            else:
                if decision.get("reasoning"):
                    for word in decision["reasoning"].split(" "):
                        chunk({"reasoning_content": word + " "})
                words = decision.get("text", "").split(" ")
                if decision["kind"] == "drop":
                    for word in words[: max(1, len(words) // 2)]:
                        chunk({"content": word + " "})
                    self.wfile.flush()
                    self.close_connection = True
                    return
                for index, word in enumerate(words):
                    chunk({"content": word if index == len(words) - 1 else word + " "})
                    if decision.get("slow"):
                        time.sleep(self.state.slow_ms / 1000)
                completion = max(1, len(decision.get("text", "")) // 4)
                finish = "stop"
            chunk({}, finish)
            if usage:
                payload = {"id": f"chatcmpl-mock-{seq}", "object": "chat.completion.chunk", "created": created,
                           "model": model, "choices": [], "usage": {"prompt_tokens": prompt_tokens,
                                                                     "completion_tokens": completion,
                                                                     "total_tokens": prompt_tokens + completion}}
                self.wfile.write(b"data: " + json.dumps(payload).encode() + b"\n\n")
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass  # the client (OpenCode) aborted, e.g. an interrupt


def cmd_serve(args) -> int:
    if args.scenario not in SCENARIOS:
        print(f"unknown scenario {args.scenario!r}; known: {', '.join(SCENARIOS)}", file=sys.stderr)
        return 64
    script: list[dict] = []
    if args.script:
        try:
            loaded = json.loads(Path(args.script).read_text())
            script = [validate_entry(e) for e in (loaded if isinstance(loaded, list) else loaded["entries"])]
        except (ValueError, KeyError, json.JSONDecodeError) as error:
            print(f"invalid script {args.script}: {error}", file=sys.stderr)
            return 65
    log = Path(args.log) if args.log else None
    if log:
        log.parent.mkdir(parents=True, exist_ok=True)
    Handler.state = State(args.scenario, script, log, args.subagent, args.slow_ms)
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    server.daemon_threads = True
    host, port = server.server_address[:2]
    print(json.dumps({"url": f"http://{host}:{port}", "baseURL": f"http://{host}:{port}/v1", "pid": os.getpid()}), flush=True)

    def stop(*_):
        threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    if args.exit_on_stdin_eof:
        def watch():
            while sys.stdin.read(4096):
                pass
            stop()
        threading.Thread(target=watch, daemon=True).start()
    server.serve_forever(poll_interval=0.2)
    server.server_close()
    return 0


def cmd_write_config(args) -> int:
    profile = Path(args.profile).resolve()
    if not (profile / "luna-qa-opencode-profile.json").is_file():
        print(f"{profile} is not a QA profile (run scripts/qa-opencode-env.sh first)", file=sys.stderr)
        return 65
    target = profile / "config/opencode/opencode.json"
    if target.exists() and not args.force:
        existing = json.loads(target.read_text() or "{}")
        if PROVIDER_ID not in (existing.get("providers") or {}):
            print(f"{target} exists and was not written by this tool; pass --force", file=sys.stderr)
            return 65
    base = args.base_url.rstrip("/")
    if not base.endswith("/v1"):
        base += "/v1"
    model = {
        "name": "LUNA Mock Model",
        "capabilities": {"tools": True, "input": ["text", "image"], "output": ["text"]},
        "limit": {"context": 128000, "output": 8192},
        "cost": {"input": 0, "output": 0},
        "variants": [
            {"id": "low", "body": {"reasoning_effort": "low"}},
            {"id": "high", "body": {"reasoning_effort": "high"}},
        ],
    }
    config = {
        "$schema": "https://opencode.ai/config.json",
        "model": f"{PROVIDER_ID}/{args.model_id}",
        "share": "disabled",
        "update": "disable",
        "providers": {PROVIDER_ID: {
            "name": "LUNA QA Mock",
            "package": "aisdk:@ai-sdk/openai-compatible",
            "settings": {"baseURL": base, "apiKey": MOCK_KEY},
            "models": {args.model_id: model},
        }},
    }
    if args.exclusive:
        config["permissions"] = [
            {"action": "provider.use", "resource": "*", "effect": "deny"},
            {"action": "provider.use", "resource": PROVIDER_ID, "effect": "allow"},
        ]
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(config, indent=2) + "\n")
    print(json.dumps({"config": str(target), "model": config["model"], "baseURL": base, "exclusive": args.exclusive}))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    serve = sub.add_parser("serve")
    serve.add_argument("--host", default="127.0.0.1")
    serve.add_argument("--port", type=int, default=0)
    serve.add_argument("--log")
    serve.add_argument("--scenario", default="text")
    serve.add_argument("--script")
    serve.add_argument("--subagent", default="general")
    serve.add_argument("--slow-ms", type=int, default=400)
    serve.add_argument("--exit-on-stdin-eof", action="store_true")
    config = sub.add_parser("write-config")
    config.add_argument("--profile", required=True)
    config.add_argument("--base-url", required=True)
    config.add_argument("--model-id", default="mock-model")
    config.add_argument("--exclusive", action="store_true")
    config.add_argument("--force", action="store_true")
    sub.add_parser("scenarios")
    args = parser.parse_args()
    if args.command == "scenarios":
        for name, description in SCENARIOS.items():
            print(f"{name:24} {description}")
        return 0
    return cmd_serve(args) if args.command == "serve" else cmd_write_config(args)


if __name__ == "__main__":
    sys.exit(main())
