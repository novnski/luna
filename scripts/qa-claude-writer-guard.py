#!/usr/bin/env python3
"""No-model live E2E of Claude's session listing and LUNA's writer guard.

Build the example separately with scripts/cargo.sh --locked. This controller
never builds or launches a GUI. All provider output is summarized, never saved
raw; credentials are inherited normally and never read, copied or modified.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import threading
import time
import uuid


SESSION_ENV = (
    "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_BRIDGE_SESSION_ID", "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_SESSION_ATTENDED", "CLAUDE_JOB_DIR",
    "CLAUDE_CODE_SESSION_KIND", "CLAUDE_CODE_SESSION_NAME", "CLAUDECODE",
)
INIT_ID = "luna-writer-guard-initialize"
PROBE_LAUNCHER = """import json, os, subprocess, sys
try:
    result = subprocess.run(sys.argv[1:], capture_output=True, timeout=20)
    if result.returncode != 0:
        sys.exit(1)
    report = json.loads(result.stdout)
    report['probeParentPid'] = os.getpid()
    print(json.dumps(report))
except (Exception, KeyboardInterrupt):
    sys.exit(1)
"""


def interrupted(_signum, _frame):
    raise KeyboardInterrupt


class OutputSummary:
    def __init__(self):
        self.stdout_bytes = 0
        self.stderr_bytes = 0
        self.stdout_hash = hashlib.sha256()
        self.stderr_hash = hashlib.sha256()
        self.initialized = threading.Event()
        self.init_failed = threading.Event()
        self.frames = 0

    def stdout(self, pipe):
        for line in iter(pipe.readline, b""):
            self.stdout_bytes += len(line)
            self.stdout_hash.update(line)
            try:
                frame = json.loads(line)
                self.frames += 1
                response = frame.get("response", {})
                if (frame.get("type") == "control_response"
                        and response.get("request_id") == INIT_ID):
                    if response.get("subtype") == "success":
                        self.initialized.set()
                    else:
                        self.init_failed.set()
            except (ValueError, AttributeError):
                pass

    def stderr(self, pipe):
        for block in iter(lambda: pipe.read(8192), b""):
            self.stderr_bytes += len(block)
            self.stderr_hash.update(block)

    def record(self):
        return {
            "stdoutBytes": self.stdout_bytes, "stderrBytes": self.stderr_bytes,
            "stdoutSha256": self.stdout_hash.hexdigest(),
            "stderrSha256": self.stderr_hash.hexdigest(),
            "frames": self.frames, "initialized": self.initialized.is_set(),
        }


def stop_owned(holder):
    """Popen retains ownership until wait; never signal a discovered PID."""
    if holder.stdin:
        try:
            holder.stdin.close()
        except BrokenPipeError:
            pass
    try:
        return {"method": "stdin-eof", "exit": holder.wait(timeout=5)}
    except subprocess.TimeoutExpired:
        holder.terminate()
    try:
        return {"method": "owned-child-terminate", "exit": holder.wait(timeout=3)}
    except subprocess.TimeoutExpired:
        holder.kill()
        return {"method": "owned-child-kill", "exit": holder.wait(timeout=3)}


def main():
    signal.signal(signal.SIGTERM, interrupted)
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path, help="new evidence directory")
    parser.add_argument("--claude", default=shutil.which("claude"))
    parser.add_argument("--probe", type=Path,
                        default=root / "target/debug/examples/claude_writer_guard_probe")
    parser.add_argument("--cwd", type=Path, default=root)
    parser.add_argument("--config-home", type=Path, help="optional existing isolated provider home")
    parser.add_argument("--hold-seconds", type=int, default=0,
                        help="0..60 seconds after success for app Settings readback")
    args = parser.parse_args()
    if not args.claude or not 0 <= args.hold_seconds <= 60:
        parser.error("installed Claude executable and hold-seconds in 0..60 required")
    real = Path(args.claude).resolve(strict=True)
    probe = args.probe.resolve(strict=True)
    cwd = args.cwd.resolve(strict=True)
    config = args.config_home.resolve(strict=True) if args.config_home else None
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = os.environ.copy()
    for name in SESSION_ENV:
        env.pop(name, None)
    env.update({"DISABLE_AUTOUPDATER": "1", "DISABLE_TELEMETRY": "1",
                "DISABLE_ERROR_REPORTING": "1", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"})
    if config:
        env["CLAUDE_CONFIG_DIR"] = str(config)
    session = str(uuid.uuid4())
    marker = out / "unexpected-turn-launch.json"
    wrapper = out / "guard-cli.py"
    wrapper.write_text(
        f"#!{sys.executable}\nimport json, os, sys\n"
        f"if sys.argv[1:] == ['agents', '--json']:\n"
        f"    os.execv({str(real)!r}, [{str(real)!r}, 'agents', '--json'])\n"
        f"with open({str(marker)!r}, 'w') as log:\n"
        f"    json.dump({{'blockedBeforeProviderLaunch': True, 'argumentCount': len(sys.argv)-1}}, log)\n"
        f"sys.exit(70)\n"
    )
    wrapper.chmod(0o700)
    command = [str(real), "--print", "--session-id", session,
               "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
               "--tools", "", "--permission-mode", "dontAsk", "--setting-sources", "",
               "--settings", '{"disableAllHooks":true}', "--strict-mcp-config",
               "--mcp-config", '{"mcpServers":{}}', "--no-chrome", "--no-session-persistence"]
    summary = {"sessionId": session, "controllerPid": os.getpid(), "pass": False,
               "claudeExecutable": str(real), "configuredHome": config is not None,
               "holderArgs": command[1:], "sentUserMessages": 0}
    holder = None
    readers = []
    observed = OutputSummary()
    try:
        version = subprocess.run([str(real), "--version"], env=env, cwd=cwd,
                                 capture_output=True, timeout=5, check=True)
        summary["version"] = version.stdout.decode(errors="replace").strip()[:120]
        holder = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                  start_new_session=True)
        summary["holderPid"] = holder.pid
        for target, pipe in [(observed.stdout, holder.stdout), (observed.stderr, holder.stderr)]:
            thread = threading.Thread(target=target, args=(pipe,), daemon=True)
            thread.start()
            readers.append(thread)
        holder.stdin.write((json.dumps({"type": "control_request", "request_id": INIT_ID,
                                       "request": {"subtype": "initialize"}}) + "\n").encode())
        holder.stdin.flush()
        deadline = time.monotonic() + 20
        while not observed.initialized.wait(0.1):
            if holder.poll() is not None or observed.init_failed.is_set() or time.monotonic() >= deadline:
                raise RuntimeError("holder did not acknowledge initialize within 20 seconds")
        own = []
        for attempt in range(3):
            listing = subprocess.run([str(real), "agents", "--json"], env=env, cwd=cwd,
                                     capture_output=True, timeout=5, check=True)
            rows = json.loads(listing.stdout)
            own = [row for row in rows if row.get("pid") == holder.pid and row.get("sessionId") == session]
            if own:
                break
            if attempt < 2:
                time.sleep(1)
        if len(own) != 1 or holder.poll() is not None:
            raise RuntimeError("the exact live holder PID and session UUID were not listed")
        summary["listedHolder"] = {key: own[0].get(key) for key in ("pid", "sessionId", "kind", "status")}
        # A separate bounded launcher owns the Rust probe: the holder's parent
        # is this controller, while the Rust probe has a different parent.
        result = subprocess.run([sys.executable, "-c", PROBE_LAUNCHER, str(probe), str(wrapper),
                                 str(cwd), session, str(config) if config else "-"],
                                env=env, cwd=cwd, capture_output=True, timeout=23)
        if result.returncode != 0:
            raise RuntimeError("public harness guard probe failed; raw diagnostics omitted")
        report = json.loads(result.stdout)
        if (not report.get("rejectedBeforeAcceptance") or report.get("probeParentPid") == os.getpid()
                or report.get("probePid") == os.getpid()
                or report.get("probePid") == holder.pid or marker.exists()):
            raise RuntimeError("guard did not refuse safely with distinct process ownership")
        summary["probe"] = report
        summary["pass"] = True
        print(json.dumps({"guardRefused": True, "holderPid": holder.pid, "sessionId": session,
                          "holdSeconds": args.hold_seconds}), flush=True)
        # Optional bounded pause lets the lead inspect Settings in its current
        # app, while this controller retains ownership of the idle holder.
        if args.hold_seconds:
            time.sleep(args.hold_seconds)
    except (Exception, KeyboardInterrupt) as error:
        summary["pass"] = False
        # Only our own fixed errors are retained; provider text may be private.
        summary["failure"] = str(error) if isinstance(error, RuntimeError) else type(error).__name__
    finally:
        if holder:
            summary["cleanup"] = stop_owned(holder)
            if summary["cleanup"]["method"] != "stdin-eof" or summary["cleanup"]["exit"] != 0:
                summary["pass"] = False
            for reader in readers:
                reader.join(timeout=1)
        summary["holderOutput"] = observed.record()
        summary["unexpectedTurnLaunch"] = marker.exists()
        evidence = out / "evidence.json"
        evidence.write_text(json.dumps(summary, indent=2) + "\n")
        digest = hashlib.sha256(evidence.read_bytes()).hexdigest()
        (out / "SHA256SUMS").write_text(f"{digest}  evidence.json\n")
    print(json.dumps({"pass": summary["pass"], "evidence": str(evidence)}))
    return 0 if summary["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
