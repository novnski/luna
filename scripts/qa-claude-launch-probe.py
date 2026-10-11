#!/usr/bin/env python3
"""SW-00 Claude launch probe: the native `system/init` facts for LUNA's
Ask/Auto/Full launch flags on a fresh session.

Contract: the restricted (Ask/Auto) CLI delivers exactly LUNA's
ASK_AUTO_TOOL_GRANT and loads no user-scope MCP server while still loading the
dynamic `--mcp-config` entry; Full loads both (the positive control proving the
isolated config is read). Exit 1 names any drift, e.g. a newer CLI renaming a
built-in tool or loading user servers in restricted mode. Resumed delivery is
out of scope: against a closed endpoint the CLI records no user line to resume.

Uses the installed `claude` with an isolated CLAUDE_CONFIG_DIR, a dummy API key
and a closed loopback base URL, so no login, model turn or user configuration
is involved (the method of crates/harness/tests/fixtures/claude/compat-2.1.286.json).
The isolated config holds one user-scope fixture server; each launch also gets
one dynamic `--mcp-config` fixture server. Both are `/usr/bin/false`, so they
are listed with their scope and never run anything useful.

    python3 scripts/qa-claude-launch-probe.py <evidence-dir>

The flags mirror crates/harness/src/claude/{mod.rs build_command, profile.rs};
re-read those before trusting a result for a newer LUNA revision.
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PROFILE = (ROOT / "crates/harness/src/claude/profile.rs").read_text()


def const_list(name: str) -> list[str]:
    body = PROFILE.split(f"pub const {name}: &[&str] = &[", 1)[1].split("];", 1)[0]
    return [part.strip().strip('"') for part in body.split(",") if part.strip().startswith('"')]


ASK_AUTO = const_list("ASK_AUTO_TOOL_GRANT")
MODES = {
    "ask": ["--restricted", "--permission-mode", "acceptEdits", "--tools", ",".join(ASK_AUTO)],
    "auto": ["--restricted", "--permission-mode", "auto", "--tools", ",".join(ASK_AUTO)],
    "full": ["--permission-mode", "bypassPermissions", "--dangerously-skip-permissions"],
}
BASE = [
    "--print", "--input-format", "stream-json", "--output-format", "stream-json",
    "--verbose", "--include-partial-messages", "--thinking-display", "summarized",
    "--replay-user-messages", "--permission-prompt-tool", "stdio",
]
DYNAMIC = json.dumps({"mcpServers": {"luna-fixture-dynamic": {"type": "stdio", "command": "/usr/bin/false"}}})


def closed_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def launch(exe: str, home: Path, cwd: Path, mode: str) -> dict:
    argv = [exe, *BASE, *MODES[mode], "--mcp-config", DYNAMIC]
    env = {key: value for key, value in os.environ.items() if not key.startswith(("CLAUDE", "ANTHROPIC"))}
    env.update({
        "CLAUDE_CONFIG_DIR": str(home), "ANTHROPIC_API_KEY": "sk-ant-sw00-probe-not-a-key",
        "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{closed_port()}", "DISABLE_AUTOUPDATER": "1",
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "ENABLE_CLAUDEAI_MCP_SERVERS": "false",
    })
    started = time.time()
    proc = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True)
    line = {"type": "user", "message": {"role": "user", "content": "sw00 launch probe"}}
    proc.stdin.write(json.dumps(line) + "\n")
    proc.stdin.flush()
    init = None
    deadline = time.time() + 60
    while time.time() < deadline and init is None:
        raw = proc.stdout.readline()
        if not raw:
            break
        try:
            frame = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if frame.get("type") == "system" and frame.get("subtype") == "init":
            init = frame
    proc.kill()
    proc.wait()
    shown = {DYNAMIC: "<dynamic fixture>", ",".join(ASK_AUTO): "<ASK_AUTO_TOOL_GRANT>"}
    result = {"mode": mode, "argv_flags": [shown.get(flag, flag) for flag in argv[1:]],
              "seconds": round(time.time() - started, 2)}
    if not init:
        result["error"] = "no system/init frame within 60s"
        return result
    result.update({
        "claude_code_version": init.get("claude_code_version"),
        "session_id": init.get("session_id"),
        "permissionMode": init.get("permissionMode"),
        "tools": sorted(init.get("tools") or []),
        "mcp_servers": sorted(({"name": s.get("name"), "status": s.get("status")} for s in init.get("mcp_servers") or []), key=lambda s: s["name"]),
        "slash_command_count": len(init.get("slash_commands") or []),
    })
    return result


def main() -> int:
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else None
    if not out:
        print("usage: qa-claude-launch-probe.py <evidence-dir>", file=sys.stderr)
        return 2
    exe = shutil.which("claude")
    version = subprocess.run([exe, "--version"], capture_output=True, text=True).stdout.strip()
    work = Path(tempfile.mkdtemp(prefix="sw00-claude-probe-"))
    try:
        cwd = work / "project"
        cwd.mkdir()
        results = []
        for mode in MODES:
            home = work / f"home-{mode}"
            home.mkdir()
            (home / ".claude.json").write_text(json.dumps({
                "hasCompletedOnboarding": True,
                "mcpServers": {"luna-fixture-user": {"type": "stdio", "command": "/usr/bin/false"}},
            }))
            results.append(launch(exe, home, cwd, mode))
        grant = set(ASK_AUTO)
        drift = []
        for row in results:
            if "tools" not in row:
                drift.append(f"{row['mode']}: {row['error']}")
                continue
            servers = {server["name"] for server in row["mcp_servers"]}
            if "luna-fixture-dynamic" not in servers:
                drift.append(f"{row['mode']}: dynamic --mcp-config server not loaded")
            if row["mode"] == "full":
                if "luna-fixture-user" not in servers:
                    drift.append("full: user-scope server not loaded (isolated config unread?)")
                continue
            builtin = {tool for tool in row["tools"] if not tool.startswith("mcp__")}
            row["grant_missing_from_init"] = sorted(grant - builtin)
            row["init_not_in_grant"] = sorted(builtin - grant)
            if row["grant_missing_from_init"] or row["init_not_in_grant"]:
                drift.append(f"{row['mode']}: tools differ from ASK_AUTO_TOOL_GRANT")
            if "luna-fixture-user" in servers:
                drift.append(f"{row['mode']}: restricted run loaded a user-scope server")
        record = {"evidenceKind": "sw00-claude-launch-probe", "cli": version, "executable": exe.replace(str(Path.home()), "~"),
                  "ask_auto_grant": ASK_AUTO, "results": results, "drift": drift}
        out.mkdir(parents=True, exist_ok=True)
        (out / "claude-launch-probe.json").write_text(json.dumps(record, indent=2) + "\n")
        ok = not drift
        print(json.dumps({"passed": ok, "cli": version, "launches": len(results), "drift": drift}))
        return 0 if ok else 1
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
