#!/usr/bin/env python3
"""OpenCode T2 rig: real `opencode` v2 binary + mock provider, isolated (OC-01).

Contract: docs/reference/qa-infrastructure.md, "OpenCode QA". Harness-level
only: no LUNA app, adapter or engine is involved, so this evidence is tier T2
for the OpenCode server and the mock, never LUNA behaviour or model output.

  scripts/qa-opencode-t2.py [--evidence-root DIR] [--scenario NAME]
      [--binary PATH] [--turn SCENARIO ...] [--exclusive] [--keep-work]

Steps: isolated profile + disposable fixture under the system temp dir (never
inside $HOME), mock provider, opencode.json, `opencode serve --stdio --port 0`
with OPENCODE_PASSWORD and stdin as the lease, GET /api/info (503 retried),
/api/model (must list luna-mock/mock-model), /openapi.json fingerprint,
/api/mcp, optional session turns driven through /api/event, then shutdown by
closing stdin (SIGTERM only after the grace period). Writes manifest.json,
commands.jsonl, http.jsonl, native-frames.jsonl, native-readback/, result.md,
secret-scan.json, SHA256SUMS and pass.json (only when every criterion passed).
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import http.client
import json
import os
import queue
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"
MOCK_MODEL = ("luna-mock", "mock-model")
TURN_EXPECT = {
    # scenario: (terminal event suffix, marker that must appear in frames)
    "text": ("succeeded", "MOCK_TEXT_OK"),
    "reasoning": ("succeeded", "MOCK_REASONING_DONE"),
    "slow": ("succeeded", "MOCK_TEXT_OK"),
    "tool-shell": ("succeeded", "luna-mock-shell-ok"),
    "subagent": ("succeeded", "MOCK_CHILD_OK"),
    "image": ("succeeded", "MOCK_IMAGE"),
    "error-401": ("failed", None),
    "context-overflow": ("failed", None),
}


def utc() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# GET /api/model and /api/model/default return each provider's `settings`,
# including `apiKey`. Known non-secret placeholders stay readable (OCF-142).
SECRET_KEYS = {"apikey", "api_key", "authtoken", "auth_token", "password", "secret", "token",
               "accesstoken", "access_token", "refreshtoken", "refresh_token", "authorization"}
PLACEHOLDERS = {"public", "luna-qa-mock-key-not-secret", ""}


def redact(value):
    if isinstance(value, dict):
        return {k: ("<redacted>" if k.lower() in SECRET_KEYS and isinstance(v, str) and v not in PLACEHOLDERS
                    else redact(v)) for k, v in value.items()}
    if isinstance(value, list):
        return [redact(v) for v in value]
    return value


def png_1x1() -> bytes:
    """A valid 1x1 RGBA PNG built from the standard library."""
    import struct
    import zlib

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)
    pixels = zlib.compress(b"\x00\x2a\x7f\xd4\xff")
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", pixels) + chunk(b"IEND", b"")


class Run:
    def __init__(self, evidence: Path):
        self.dir = evidence
        self.readback = evidence / "native-readback"
        self.readback.mkdir(parents=True)
        self.criteria: list[dict] = []
        self.lock = threading.Lock()

    def jsonl(self, name: str, entry: dict) -> None:
        with self.lock, (self.dir / name).open("a") as handle:
            handle.write(json.dumps(entry, sort_keys=True) + "\n")

    def save(self, name: str, value) -> None:
        path = self.dir / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")

    def check(self, name: str, expected: str, actual, passed: bool) -> bool:
        self.criteria.append({"criterion": name, "expected": expected, "actual": actual, "passed": bool(passed)})
        print(f"[{'PASS' if passed else 'FAIL'}] {name}: {actual}", file=sys.stderr)
        return bool(passed)

    def command(self, name: str, argv: list[str], env_names: list[str], started: float, code, log: str | None = None,
                note: str | None = None) -> None:
        self.jsonl("commands.jsonl", {"name": name, "argv": argv, "envNames": sorted(env_names), "exit": code,
                                      "durationMs": int((time.monotonic() - started) * 1000), "log": log,
                                      "note": note, "at": utc()})


def run_cmd(run: Run, name: str, argv: list[str], env: dict | None = None, env_names: list[str] | None = None,
            log: Path | None = None, check: bool = True) -> subprocess.CompletedProcess:
    started = time.monotonic()
    done = subprocess.run(argv, env=env, capture_output=True, text=True)
    if log:
        log.write_text(done.stdout + ("\n--- stderr ---\n" + done.stderr if done.stderr else ""))
    run.command(name, argv, env_names or [], started, done.returncode, str(log.relative_to(run.dir)) if log else None)
    if check and done.returncode != 0:
        raise SystemExit(f"{name} failed ({done.returncode}): {done.stderr.strip()[-2000:]}")
    return done


class Api:
    """Minimal authenticated client; records http.jsonl without credentials."""

    def __init__(self, run: Run, url: str, password: str):
        parsed = urllib.parse.urlparse(url)
        self.host, self.port = parsed.hostname, parsed.port
        self.run = run
        self.auth = "Basic " + base64.b64encode(f"opencode:{password}".encode()).decode()

    def request(self, method: str, path: str, body=None, auth: bool = True, timeout: float = 30.0, record_body: bool = True):
        data = None if body is None else json.dumps(body).encode()
        headers = {"accept": "application/json"}
        if data is not None:
            headers["content-type"] = "application/json"
        if auth:
            headers["authorization"] = self.auth
        started = time.monotonic()
        conn = http.client.HTTPConnection(self.host, self.port, timeout=timeout)
        try:
            conn.request(method, path, body=data, headers=headers)
            response = conn.getresponse()
            raw = response.read()
            status = response.status
            response_headers = {k.lower(): v for k, v in response.getheaders()}
        finally:
            conn.close()
        try:
            parsed = redact(json.loads(raw)) if raw else None
        except json.JSONDecodeError:
            parsed = None
        entry = {"at": utc(), "method": method, "path": path, "status": status,
                 "durationMs": int((time.monotonic() - started) * 1000),
                 "requestHeaderNames": sorted(headers), "authenticated": auth,
                 "requestSha256": sha(data) if data else None, "responseSha256": sha(raw),
                 "responseContentType": response_headers.get("content-type"), "responseBytes": len(raw),
                 "retryAfter": response_headers.get("retry-after")}
        if data is not None:
            entry["requestBody"] = body  # rig bodies carry no secrets
        if record_body and parsed is not None and len(raw) <= 256 * 1024:
            entry["responseBody"] = parsed
        self.run.jsonl("http.jsonl", entry)
        return status, parsed, response_headers, raw


class EventTap(threading.Thread):
    """GET /api/event -> native-frames.jsonl, plus a queue for the driver."""

    def __init__(self, api: Api, run: Run):
        super().__init__(daemon=True)
        self.api, self.out = api, run
        self.events: queue.Queue = queue.Queue()
        self.ready = threading.Event()
        self.stopped = threading.Event()
        self.count = 0
        self.error = None

    def run(self):  # noqa: D401 - thread body
        try:
            conn = http.client.HTTPConnection(self.api.host, self.api.port, timeout=None)
            conn.request("GET", "/api/event", headers={"accept": "text/event-stream", "authorization": self.api.auth})
            response = conn.getresponse()
            self.out.jsonl("http.jsonl", {"at": utc(), "method": "GET", "path": "/api/event", "status": response.status,
                                          "requestHeaderNames": ["accept", "authorization"], "authenticated": True,
                                          "stream": True, "generation": 1})
            self.ready.set()
            buffer = []
            while not self.stopped.is_set():
                line = response.fp.readline()
                if not line:
                    break
                line = line.decode("utf-8", errors="replace").rstrip("\r\n")
                if line.startswith("data:"):
                    buffer.append(line[5:].lstrip())
                elif line == "" and buffer:
                    text = "\n".join(buffer)
                    buffer = []
                    try:
                        frame = redact(json.loads(text))
                    except json.JSONDecodeError:
                        frame = {"unparsed": text[:2000]}
                    self.count += 1
                    durable = frame.get("durable") if isinstance(frame, dict) else None
                    self.out.jsonl("native-frames.jsonl", {
                        "receivedAt": utc(), "generation": 1, "localSeq": self.count,
                        "eventId": frame.get("id") if isinstance(frame, dict) else None,
                        "durableSeq": durable.get("seq") if isinstance(durable, dict) else None,
                        "type": frame.get("type") if isinstance(frame, dict) else None, "frame": frame})
                    self.events.put(frame)
        except Exception as error:  # recorded; the driver decides
            self.error = repr(error)
            self.ready.set()


def wait_url(proc: subprocess.Popen, timeout: float) -> str | None:
    holder: dict = {}

    def read():
        line = proc.stdout.readline()
        holder["line"] = line
        for _ in iter(proc.stdout.readline, b""):  # keep draining (EPIPE safety)
            pass

    threading.Thread(target=read, daemon=True).start()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and "line" not in holder and proc.poll() is None:
        time.sleep(0.05)
    line = holder.get("line") or b""
    try:
        return json.loads(line.decode()).get("url")
    except (json.JSONDecodeError, AttributeError, UnicodeDecodeError):
        return None


def stop_process(proc: subprocess.Popen, grace: float) -> dict:
    """Close the stdin lease; SIGTERM after `grace`, SIGKILL 3 s later."""
    result = {"method": None, "exit": None}
    started = time.monotonic()
    try:
        proc.stdin.close()
    except (BrokenPipeError, OSError):
        pass
    for method, wait in (("stdin-eof", grace), ("sigterm", 3.0), ("sigkill", 3.0)):
        if method == "sigterm" and proc.poll() is None:
            proc.send_signal(signal.SIGTERM)
        elif method == "sigkill" and proc.poll() is None:
            proc.kill()
        try:
            proc.wait(timeout=wait)
            result["method"] = method
            break
        except subprocess.TimeoutExpired:
            continue
    result["exit"] = proc.poll()
    result["seconds"] = round(time.monotonic() - started, 2)
    return result


USER_STORAGE = ("~/.config/opencode", "~/.local/share/opencode", "~/.local/share/opencode/opencode.db",
                "~/.local/share/opencode/opencode.db-wal", "~/.local/share/opencode/auth.json",
                "~/.cache/opencode", "~/.local/state/opencode")


def user_storage_stat() -> dict:
    """OCF-140 evidence: stat only (mtime, size); never opens a user file."""
    out = {}
    for raw in USER_STORAGE:
        path = Path(os.path.expanduser(raw))
        try:
            st = path.stat()
            out[raw] = {"mtimeNs": st.st_mtime_ns, "size": st.st_size}
        except FileNotFoundError:
            out[raw] = None
    return out


def git_state() -> dict:
    def out(*args):
        return subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, text=True).stdout.strip()
    return {"sha": out("rev-parse", "HEAD"), "branch": out("rev-parse", "--abbrev-ref", "HEAD"),
            "dirty": bool(out("status", "--porcelain"))}


def drive_turn(api: Api, tap: EventTap, run: Run, index: int, scenario: str, fixture: Path, timeout: float) -> dict:
    status, created, _, _ = api.request("POST", "/api/session", {"location": {"directory": str(fixture)},
                                                                  "title": f"LUNA T2 rig turn {index}"})
    if status != 200:
        return {"scenario": scenario, "error": f"session create {status}", "body": created}
    session = created["data"]["id"]
    text = f"[[mock:{scenario}]] LUNA T2 rig turn {index}. Follow the mock's scripted reply."
    body: dict = {"text": text}
    attachment = None
    if scenario == "image":
        image = fixture / f"pixel-{index}.png"
        image.write_bytes(png_1x1())
        attachment = {"path": str(image), "sha256": sha(image.read_bytes()), "bytes": image.stat().st_size}
        body["files"] = [{"uri": image.as_uri(), "name": image.name}]
    status, inbox, _, _ = api.request("POST", f"/api/session/{session}/prompt", body)
    outcome = {"scenario": scenario, "session": session, "promptStatus": status, "attachment": attachment,
               "inboxId": (inbox or {}).get("data", {}).get("id") if isinstance(inbox, dict) else None,
               "terminal": None, "children": [], "permissions": [], "frames": []}
    owned = {session}
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            frame = tap.events.get(timeout=0.5)
        except queue.Empty:
            continue
        data = frame.get("data") if isinstance(frame, dict) else None
        kind = frame.get("type", "") if isinstance(frame, dict) else ""
        sid = data.get("sessionID") if isinstance(data, dict) else None
        if kind == "session.created" and isinstance(data, dict) and data.get("parentID") in owned:
            owned.add(data["sessionID"])
            outcome["children"].append(data["sessionID"])
        if sid not in owned:
            continue
        outcome["frames"].append(kind)
        if kind == "permission.asked":
            # T2 rig auto-approves once inside its own disposable fixture.
            reply_status, body, _, _ = api.request("POST", f"/api/session/{sid}/permission/{data['id']}/reply",
                                                   {"decision": "once"})
            outcome["permissions"].append({"id": data["id"], "action": data.get("action"), "replyStatus": reply_status})
        if sid == session and kind in ("session.execution.succeeded", "session.execution.failed",
                                       "session.execution.interrupted"):
            outcome["terminal"] = kind.rsplit(".", 1)[1]
            outcome["terminalData"] = data
            break
    time.sleep(1.0)
    for sid in [session, *outcome["children"]]:
        _, info, _, _ = api.request("GET", f"/api/session/{sid}")
        _, messages, _, _ = api.request("GET", f"/api/session/{sid}/message?order=asc&limit=200")
        run.save(f"native-readback/turn-{index}-{sid}-session.json", info)
        run.save(f"native-readback/turn-{index}-{sid}-messages.json", messages)
    _, active, _, _ = api.request("GET", "/api/session/active")
    run.save(f"native-readback/turn-{index}-active.json", active)
    return outcome


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--evidence-root", default=str(ROOT / "target/qa/opencode"))
    parser.add_argument("--scenario", default="oce-00-t2-rig")
    parser.add_argument("--binary")
    parser.add_argument("--turn", action="append", default=[], choices=sorted(TURN_EXPECT) + ["drop", "error-429-once", "error-500-once", "tool-shell-background", "subagent-background"])
    parser.add_argument("--exclusive", action="store_true", help="deny every provider but the mock")
    parser.add_argument("--boot-timeout", type=float, default=120.0)
    parser.add_argument("--turn-timeout", type=float, default=120.0)
    parser.add_argument("--grace", type=float, default=10.0, help="seconds after stdin EOF before SIGTERM")
    parser.add_argument("--keep-work", action="store_true", help="keep the disposable profile and fixture")
    args = parser.parse_args()

    started_at = utc()
    root = Path(args.evidence_root).resolve() / args.scenario
    root.mkdir(parents=True, exist_ok=True)
    evidence = root / f"run-{dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%SZ')}-{secrets.token_hex(3)}"
    run = Run(evidence)
    (root / "latest").write_text(str(evidence) + "\n")
    print(f"evidence: {evidence}", file=sys.stderr)

    work = Path(tempfile.mkdtemp(prefix="luna-qa-opencode-t2-")).resolve()
    home = Path.home().resolve()
    if home == work or home in work.parents:
        raise SystemExit(f"QF-02: disposable work dir {work} is inside $HOME; set TMPDIR outside $HOME")
    profile, fixture = work / "profile", work / "fixture"
    fixture.mkdir()
    for argv in (["git", "init", "-q", "-b", "main", str(fixture)],
                 ["git", "-C", str(fixture), "config", "user.name", "LUNA T2 QA"],
                 ["git", "-C", str(fixture), "config", "user.email", "qa@example.invalid"]):
        subprocess.run(argv, check=True)
    (fixture / "README.md").write_text("Disposable OpenCode T2 fixture.\n")
    subprocess.run(["git", "-C", str(fixture), "add", "README.md"], check=True)
    subprocess.run(["git", "-C", str(fixture), "commit", "-q", "-m", "fixture"], check=True)

    manifest: dict = {"schema": "luna-opencode-qa-manifest/1", "scenario": args.scenario, "tier": "T2",
                      "evidenceKind": "harness-level T2 rig: real opencode v2 server + mock provider; "
                                      "no LUNA adapter, engine, document or UI is exercised",
                      "startedAt": started_at, "luna": git_state(), "app": {"used": False},
                      "work": {"root": str(work), "fixture": str(fixture), "profile": str(profile),
                               "insideHome": False, "kept": args.keep_work},
                      "requested": {"turns": args.turn, "exclusive": args.exclusive}}
    storage_before = user_storage_stat()
    run.save("manifest.json", manifest)

    env_argv =[str(SCRIPTS / "qa-opencode-env.sh"), "--offline", str(profile)]
    if args.binary:
        env_argv[1:1] = ["--binary", args.binary]
    run_cmd(run, "profile", env_argv, log=evidence / "profile-exports.txt")
    shutil.copy(profile / "luna-qa-opencode-profile.json", evidence / "profile.json")
    wrapper = profile / "bin/opencode"

    mock_log = evidence / "mock-requests.jsonl"
    mock_argv = [sys.executable, str(SCRIPTS / "qa-opencode-mock-provider.py"), "serve", "--port", "0",
                 "--log", str(mock_log), "--exit-on-stdin-eof"]
    mock_started = time.monotonic()
    mock = subprocess.Popen(mock_argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    mock_url = wait_url(mock, 10)
    run.command("mock-provider", mock_argv, [], mock_started, None if mock_url else mock.poll(), note=f"url={mock_url}")
    if not mock_url:
        raise SystemExit("mock provider did not report its URL")
    run_cmd(run, "mock-config", [sys.executable, str(SCRIPTS / "qa-opencode-mock-provider.py"), "write-config",
                                 "--profile", str(profile), "--base-url", mock_url] + (["--exclusive"] if args.exclusive else []))
    shutil.copy(profile / "config/opencode/opencode.json", evidence / "opencode-config.json")
    run_cmd(run, "binary-record", [sys.executable, str(SCRIPTS / "qa-opencode-manifest.py"), "binary",
                                   "--path", str(wrapper), "--out", str(evidence / "binary-resolution.json")],
            log=None)
    binary = json.loads((evidence / "binary-resolution.json").read_text())
    manifest["opencodeBinary"] = binary

    password = secrets.token_urlsafe(32)
    server_env = {k: v for k, v in os.environ.items() if not k.startswith("OPENCODE_")}
    server_env.update(OPENCODE_PASSWORD=password, OPENCODE_CLIENT="luna-qa-t2")
    serve_argv = [str(wrapper), "serve", "--stdio", "--port", "0"]
    stderr_log = evidence / "opencode-stderr.log"
    serve_started = time.monotonic()
    with stderr_log.open("wb") as stderr:
        server = subprocess.Popen(serve_argv, cwd=str(fixture), env=server_env, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=stderr)
    stopped = None
    url = None
    try:
        url = wait_url(server, args.boot_timeout)
        run.command("opencode-serve", serve_argv, ["OPENCODE_PASSWORD", "OPENCODE_CLIENT"], serve_started,
                    None if url else server.poll(), log="opencode-stderr.log",
                    note="ready line received" if url else "no {\"url\"} line before the boot timeout")
        run.check("stdio-ready-line", '{"url"} on stdout within the boot budget', url, bool(url))
        if not url:
            raise RuntimeError("boot")
        parsed = urllib.parse.urlparse(url)
        manifest["server"] = {"ownership": "spawned --stdio (stdin lease)", "url": url, "port": parsed.port,
                              "pid": server.pid, "credentials": "OPENCODE_PASSWORD env (value never recorded)"}
        api = Api(run, url, password)

        unauth, _, unauth_headers, _ = api.request("GET", "/api/info", auth=False)
        run.check("api-info-requires-auth", "401 without credentials", unauth, unauth == 401)
        info_status, info, info_headers = None, None, None
        boot_deadline = time.monotonic() + args.boot_timeout
        while time.monotonic() < boot_deadline:
            info_status, info, info_headers, _ = api.request("GET", "/api/info")
            if info_status != 503:
                break
            time.sleep(float(info_headers.get("retry-after") or 1))
        run.save("native-readback/api-info.json", {"status": info_status, "body": info})
        version = (info or {}).get("version") if isinstance(info, dict) else None
        run.check("api-info-ready", "200 with a 2.x version", f"{info_status} version={version}",
                  info_status == 200 and str(version or "").startswith("2."))
        legacy, legacy_body, legacy_headers, _ = api.request("GET", "/global/health", record_body=False)
        # G-0 baseline: LUNA's current Protocol::detect accepts a generation
        # only from a 2xx JSON body with a string `version` on these paths.
        health, health_body, health_headers, _ = api.request("GET", "/api/health")
        detect_hits = [p for p, s, b in (("/api/health", health, health_body), ("/global/health", legacy, legacy_body))
                       if 200 <= (s or 0) < 300 and isinstance(b, dict) and isinstance(b.get("version"), str)]
        model_status, models = None, None
        for _ in range(60):
            model_status, models, _, _ = api.request("GET", "/api/model")
            if model_status == 200 and (models or {}).get("data"):
                break
            time.sleep(1)
        run.save("native-readback/api-model.json", {"status": model_status, "body": models})
        rows = (models or {}).get("data") or []
        mock_rows = [m for m in rows if (m.get("providerID"), m.get("modelID") or m.get("id")) == MOCK_MODEL
                     or (m.get("providerID") == MOCK_MODEL[0] and m.get("id") in (MOCK_MODEL[1], "/".join(MOCK_MODEL)))]
        run.check("api-model-lists-mock", "luna-mock/mock-model present", f"{model_status}; {len(rows)} models; "
                  f"mock={[(m.get('providerID'), m.get('id'), m.get('modelID')) for m in mock_rows]}",
                  model_status == 200 and bool(mock_rows))
        if mock_rows:
            variants = [v.get("id") if isinstance(v, dict) else v for v in mock_rows[0].get("variants") or []]
            run.check("mock-variants", "variants low and high", variants, {"low", "high"} <= set(variants))
        default_status, default, _, _ = api.request("GET", "/api/model/default")
        run.save("native-readback/api-model-default.json", {"status": default_status, "body": default})
        openapi_status, openapi, _, openapi_raw = api.request("GET", "/openapi.json", record_body=False)
        operations = sorted(op.get("operationId") for ops in (openapi or {}).get("paths", {}).values()
                            for op in ops.values() if isinstance(op, dict) and op.get("operationId"))
        (evidence / "native-readback/openapi.json").write_bytes(openapi_raw)
        mcp_status, mcp, _, _ = api.request("GET", "/api/mcp")
        run.save("native-readback/api-mcp.json", {"status": mcp_status, "body": mcp})
        run.check("no-user-mcp", "isolated profile exposes no MCP servers", (mcp or {}).get("data"),
                  mcp_status == 200 and not (mcp or {}).get("data"))
        manifest["generation"] = {
            "apiInfo": {"status": info_status, "body": info},
            "legacyProbe": {"path": "/global/health", "status": legacy, "contentType": legacy_headers.get("content-type")},
            "g0Baseline": {
                "apiHealth": {"status": health, "contentType": health_headers.get("content-type")},
                "globalHealth": {"status": legacy, "contentType": legacy_headers.get("content-type")},
                "currentLunaDetectSeesVersion": bool(detect_hits),
                "note": "observation only: LUNA's pre-OC-02 detect() needs a 2xx JSON `version` from one of these paths",
            },
            "openapi": {"status": openapi_status, "sha256": sha(openapi_raw), "operations": len(operations)},
            "class": "research-baseline" if version == "2.0.22" else "untested",
            "eventVocabulary": f"v{version}" if version else None,
        }
        manifest["xdg"] = {**{k: v for k, v in (json.loads((evidence / "profile.json").read_text())).items()
                              if k in ("profile", "offline", "homeIsolated", "projectConfigDisabled")},
                           "isolated": True, "roots": {n: str(profile / d) for n, d in (
                               ("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                               ("XDG_CACHE_HOME", "cache"), ("XDG_STATE_HOME", "state"))}}
        manifest["model"] = {"requested": "/".join(MOCK_MODEL) + " (profile default)", "effectiveDefault": default}
        manifest["mcp"] = {"status": mcp_status, "servers": (mcp or {}).get("data")}
        manifest["permission"] = {"lunaMode": None, "compiledRules": None,
                                  "note": "no LUNA adapter; the rig answers asks with decision=once"}

        if args.turn:
            tap = EventTap(api, run)
            tap.start()
            tap.ready.wait(15)
            run.check("event-stream-open", "GET /api/event subscribed before the first prompt", tap.error or "open",
                      tap.error is None)
            turns = []
            for index, scenario in enumerate(args.turn, 1):
                outcome = drive_turn(api, tap, run, index, scenario, fixture, args.turn_timeout)
                turns.append(outcome)
                expect = TURN_EXPECT.get(scenario)
                if expect:
                    terminal, marker = expect
                    frames_text = (evidence / "native-frames.jsonl").read_text() if (evidence / "native-frames.jsonl").exists() else ""
                    seen = marker is None or marker in frames_text
                    run.check(f"turn-{index}-{scenario}", f"execution.{terminal}" + (f" and {marker} in frames" if marker else ""),
                              f"terminal={outcome.get('terminal')} marker={'seen' if seen else 'missing'} "
                              f"children={len(outcome.get('children', []))} permissions={len(outcome.get('permissions', []))}",
                              outcome.get("terminal") == terminal and seen)
                else:
                    run.check(f"turn-{index}-{scenario}", "a terminal execution event", outcome.get("terminal"),
                              outcome.get("terminal") is not None)
                if outcome.get("attachment"):
                    received = [image for line in mock_log.read_text().splitlines()
                                for image in json.loads(line).get("images") or []]
                    want = outcome["attachment"]["sha256"]
                    run.check(f"turn-{index}-image-bytes", "the mock received the attachment's exact bytes",
                              f"sent={want[:16]} received={[i.get('sha256', '')[:16] for i in received]}",
                              any(i.get("sha256") == want for i in received))
            manifest["turns"] = turns
            steps = []
            for line in (evidence / "native-frames.jsonl").read_text().splitlines():
                frame = json.loads(line)["frame"]
                if frame.get("type") == "session.step.started":
                    steps.append(frame.get("data", {}).get("model"))
            manifest["model"]["effectivePerStep"] = steps
            mock_models = {json.loads(l).get("model") for l in mock_log.read_text().splitlines()} if mock_log.exists() else set()
            run.check("tier-T2-mock-only", "every provider request reached the mock model only (OCF-141/143)",
                      sorted(m for m in mock_models if m), bool(mock_models) and mock_models <= {MOCK_MODEL[1]})
            tap.stopped.set()
    except RuntimeError:
        pass
    finally:
        stopped = stop_process(server, args.grace)
        run.command("opencode-shutdown", ["<close stdin>"], [], time.monotonic(), stopped["exit"],
                    note=json.dumps(stopped))
        mock_stop = stop_process(mock, 5.0)
        manifest["shutdown"] = {"opencode": stopped, "mock": mock_stop}
    run.check("clean-shutdown-on-stdin-eof", "server exits after stdin EOF, before SIGTERM",
              stopped, stopped["method"] == "stdin-eof")
    still = subprocess.run(["ps", "-p", str(server.pid)], capture_output=True).returncode == 0
    run.check("no-orphan-server", "server pid gone after shutdown", f"pid {server.pid} alive={still}", not still)
    storage_after = user_storage_stat()
    changed = sorted(k for k in storage_before if storage_before[k] != storage_after[k])
    manifest["userStorage"] = {"method": "stat only (mtime, size); no user file is opened",
                               "before": storage_before, "after": storage_after, "changed": changed}
    run.check("user-storage-untouched", "the user's OpenCode dirs keep mtime and size (OCF-140; a concurrent "
              "user OpenCode session can also change them)", changed or "unchanged", not changed)

    # Evidence integrity: redaction manifest, secret scan, sums.
    run.save("redaction.json", {
        "headers": "request header names only; Authorization values are never written",
        "serverCredential": "OPENCODE_PASSWORD generated per run, passed by env, scanned for, never written",
        "requestBodies": "recorded in full (rig prompts and permission replies carry no secrets)",
        "responseBodies": "JSON bodies up to 256 KiB recorded; larger bodies hashed only",
        "mockProvider": "logs authorizationPresent only; the config apiKey is the fixed non-secret placeholder",
        "frames": "raw /api/event frames; only string values of secret-named keys are replaced",
        "secretKeys": sorted(SECRET_KEYS),
        "placeholdersKept": sorted(p for p in PLACEHOLDERS if p),
    })
    manifest["endedAt"] = utc()
    manifest["criteria"] = run.criteria
    run.save("manifest.json", manifest)
    scan_env = dict(os.environ, LUNA_QA_T2_PASSWORD=password,
                    LUNA_QA_T2_BASIC=base64.b64encode(f"opencode:{password}".encode()).decode())
    scan = subprocess.run([sys.executable, str(SCRIPTS / "qa-opencode-manifest.py"), "scan", "--dir", str(evidence),
                           "--secret-env", "LUNA_QA_T2_PASSWORD", "--secret-env", "LUNA_QA_T2_BASIC"],
                          env=scan_env, capture_output=True, text=True)
    run.check("secret-scan", "no credential values or patterns in evidence (OCF-142)", scan.stdout.strip(),
              scan.returncode == 0)
    passed = all(c["passed"] for c in run.criteria)
    manifest["criteria"] = run.criteria
    manifest["passed"] = passed
    run.save("manifest.json", manifest)
    replay = " ".join([sys.argv[0], "--evidence-root", args.evidence_root, "--scenario", args.scenario]
                      + sum((["--turn", t] for t in args.turn), []) + (["--exclusive"] if args.exclusive else []))
    lines = [f"# OpenCode T2 rig: {args.scenario}", "",
             f"- Evidence tier: **T2** (real `opencode` {manifest.get('generation', {}).get('apiInfo', {}).get('body', {}).get('version') if isinstance(manifest.get('generation', {}).get('apiInfo', {}).get('body'), dict) else '?'} + LUNA mock provider).",
             "- Not covered: the LUNA adapter, engine, document, UI, and any real model output.",
             f"- LUNA {manifest['luna']['sha']} (dirty={manifest['luna']['dirty']}), started {started_at}, ended {manifest['endedAt']}.",
             f"- Result: **{'PASS' if passed else 'FAIL'}**",
             f"- Observation (G-0 baseline, not a criterion): {json.dumps(manifest.get('generation', {}).get('g0Baseline'))}",
             "", "| Criterion | Expected | Actual | Pass |", "| --- | --- | --- | --- |"]
    for c in run.criteria:
        actual = json.dumps(c["actual"]) if not isinstance(c["actual"], str) else c["actual"]
        lines.append(f"| {c['criterion']} | {c['expected']} | {actual.replace('|', '/')[:300]} | {'yes' if c['passed'] else 'NO'} |")
    lines += ["", "## Replay", "", f"```\n{replay}\n```", "",
              "The disposable profile and fixture live under the system temp dir and are removed unless --keep-work."]
    (evidence / "result.md").write_text("\n".join(lines) + "\n")
    if passed:
        run.save("pass.json", {"scenario": args.scenario, "tier": "T2", "passedAt": utc()})
    subprocess.run([sys.executable, str(SCRIPTS / "qa-opencode-manifest.py"), "sums", "--dir", str(evidence)],
                   check=True, capture_output=True)
    if not args.keep_work:
        shutil.rmtree(work, ignore_errors=True)
    print(json.dumps({"passed": passed, "evidence": str(evidence)}))
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
