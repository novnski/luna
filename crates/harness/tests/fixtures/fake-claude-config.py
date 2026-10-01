#!/usr/bin/env python3
"""Deterministic Claude Code 2.1.286 peer for effective-configuration tests (E23).

Emulates only the configuration semantics recorded in claude/compat-2.1.286.json:
- --restricted ignores user/project/local settings files and the custom commands they
  bring; --setting-sources cannot re-enable them; --settings (the flag layer) applies.
- --restricted removes code-running tools and WebFetch unless --tools NAMES them; the
  "default" preset does not count. Unknown names are dropped silently.
- Settings precedence: --settings > settings.local.json > settings.json > user.
- initialize alone would run SessionStart hooks unless disableAllHooks is set. The fake
  never executes hook commands; it records hooks_ran in its log.
- initialize replays the real response (claude/initialize-2.1.286.json); a user line
  replays the real system/init (claude/system-init-2.1.286.json) and then a success result.

Every invocation appends one JSON line to <provider home>/fake-claude-log.jsonl.
A `.fake-init-error` file in the cwd makes initialize answer with an error.
"""

import json
import os
import sys
from pathlib import Path

FIXTURES = Path(__file__).resolve().parent / "claude"
VALUE_FLAGS = {
    "--permission-mode", "--tools", "--settings", "--setting-sources", "--model", "--effort",
    "--append-system-prompt", "--system-prompt", "--mcp-config", "--thinking-display",
    "--input-format", "--output-format", "--permission-prompt-tool",
}


def parse(argv):
    flags, i = {}, 0
    while i < len(argv):
        arg = argv[i]
        if arg in VALUE_FLAGS and i + 1 < len(argv):
            flags.setdefault(arg, []).append(argv[i + 1])
            i += 2
            continue
        if "=" in arg and arg.startswith("--"):
            key, value = arg.split("=", 1)
            flags.setdefault(key, []).append(value)
        else:
            flags.setdefault(arg, []).append(True)
        i += 1
    return flags


def read_json(path):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError):
        return {}


def flag_settings(flags):
    merged = {}
    for value in flags.get("--settings", []):
        merged.update(json.loads(value) if value.lstrip().startswith("{") else read_json(value))
    return merged


def custom_commands(root, origin):
    out = []
    folder = Path(root) / "commands"
    for path in sorted(folder.glob("*.md")) if folder.is_dir() else []:
        description = ""
        for line in path.read_text().splitlines():
            if line.startswith("description:"):
                description = line.split(":", 1)[1].strip()
                break
        out.append({"name": path.stem, "description": f"{description} ({origin})", "argumentHint": ""})
    return out


def main():
    argv = sys.argv[1:]
    flags = parse(argv)
    cwd = Path(os.getcwd())
    home = Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude")
    restricted = "--restricted" in flags
    # Recorded: with no --permission-mode, 2.1.286 reports "auto" for this account shape.
    mode = (flags.get("--permission-mode") or ["auto"])[-1]
    flag_layer = flag_settings(flags)
    layers = [] if restricted else [
        read_json(home / "settings.json"),
        read_json(cwd / ".claude" / "settings.json"),
        read_json(cwd / ".claude" / "settings.local.json"),
    ]
    settings = {}
    for layer in layers + [flag_layer]:
        settings.update(layer)
    hooks_ran = not settings.get("disableAllHooks", False) and any(
        layer.get("hooks", {}).get("SessionStart") for layer in layers + [flag_layer]
    )

    init_fixture = json.loads((FIXTURES / "initialize-2.1.286.json").read_text())
    system_fixture = json.loads((FIXTURES / "system-init-2.1.286.json").read_text())
    restricted_tools = system_fixture["ask-restricted"]["tools"]
    unrestricted_tools = system_fixture["unrestricted-acceptEdits"]["tools"]
    builtin = [c for c in init_fixture["response"]["response"]["commands"] if c.get("builtin")]
    commands = list(builtin)
    if not restricted:
        commands += custom_commands(home, "user") + custom_commands(cwd / ".claude", "project")

    tools_arg = (flags.get("--tools") or [None])[-1]
    known = set(restricted_tools) | set(unrestricted_tools)
    if tools_arg is None or tools_arg.strip().lower() == "default":
        tools = list(restricted_tools if restricted else unrestricted_tools)
    else:
        names = [n for n in tools_arg.replace(" ", ",").split(",") if n]
        tools = [n for n in names if n in known]
    output_style = settings.get("outputStyle", "default")

    log_entry = {
        "exe": sys.argv[0], "argv": argv, "cwd": str(cwd), "config_home": str(home),
        "restricted": restricted, "permission_mode": mode, "flag_settings": flag_layer,
        "output_style": output_style, "commands": [c["name"] for c in commands], "tools": tools,
        "hooks_ran": hooks_ran,
    }

    def log(kind, **extra):
        home.mkdir(parents=True, exist_ok=True)
        with open(home / "fake-claude-log.jsonl", "a") as out:
            out.write(json.dumps(dict(log_entry, kind=kind, **extra)) + "\n")

    def emit(frame):
        sys.stdout.write(json.dumps(frame) + "\n")
        sys.stdout.flush()

    initialized = False
    for line in sys.stdin:
        try:
            frame = json.loads(line)
        except ValueError:
            continue
        request = frame.get("request") or {}
        if frame.get("type") == "control_request" and request.get("subtype") == "initialize":
            rid = frame.get("request_id")
            if (cwd / ".fake-init-error").exists():
                log("initialize-error")
                emit({"type": "control_response",
                      "response": {"subtype": "error", "request_id": rid, "error": "fixture initialize failure"}})
                continue
            response = json.loads(json.dumps(init_fixture))
            body = response["response"]["response"]
            response["response"]["request_id"] = rid
            body["commands"] = commands
            body["output_style"] = output_style
            body["current_permission_mode"] = mode
            body["pid"] = os.getpid()
            initialized = True
            log("initialize")
            emit(response)
            continue
        if frame.get("type") == "user":
            system = json.loads(json.dumps(system_fixture["ask-restricted"]))
            system.update({
                "cwd": str(cwd), "tools": tools, "permissionMode": mode, "output_style": output_style,
                "slash_commands": [c["name"] for c in commands], "session_id": "sess-effective-config",
            })
            log("run", initialized_first=initialized)
            emit(system)
            emit({"type": "result", "subtype": "success", "result": "ok", "errors": [],
                  "usage": {"input_tokens": 1, "output_tokens": 1}, "session_id": "sess-effective-config"})
            return 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
