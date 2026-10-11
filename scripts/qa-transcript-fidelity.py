#!/usr/bin/env python3
"""SW-05 live transcript fidelity: a `qa-opencode-live.mjs` run of Codex or
Claude, checked against the provider's own transcript file.

    python3 scripts/qa-transcript-fidelity.py <driver-run-dir> codex|claude-code <native.jsonl>

Cases (docs/pending-tasks/luna-only-workflows/sw-05/failures.md):
N1/N2 every native root tool call appears once, in native order (Claude by
tool_use id; Codex code-mode by the inner shell command or MCP tool name);
N3 every LUNA text part is exactly one native message, in order; N4 helper
notices follow their create call; N5 no unresolved chip or unfinished entry;
N6 part ids unique per entry; N7 (Claude) no subagent call in the root and
each Agent chip owns a finished subagent; N8 the post-restart snapshot equals
the pre-restart transcript. Writes transcript-fidelity.json; exit 1 on any
failure. Needs `snapshot` after `verify-restart` for N8.
"""

import glob
import json
import re
import sys
from pathlib import Path


def native_items(path: Path, provider: str):
    items, subagent_ids = [], set()
    for line in path.read_text().splitlines():
        record = json.loads(line)
        if provider == "claude-code":
            message = record.get("message") or {}
            if record.get("type") != "assistant" or not isinstance(message.get("content"), list):
                continue
            for block in message["content"]:
                if block.get("type") == "tool_use":
                    if record.get("isSidechain"):
                        subagent_ids.add(block["id"])
                    else:
                        items.append(("tool", block["id"]))
                elif block.get("type") == "text" and not record.get("isSidechain") and block["text"].strip():
                    items.append(("text", block["text"].strip()))
        else:
            payload = record.get("payload") or {}
            if payload.get("type") == "message" and payload.get("role") == "assistant":
                text = "".join(c.get("text", "") for c in payload.get("content", []) if isinstance(c, dict)).strip()
                if text:
                    items.append(("text", text))
            elif payload.get("type") in ("function_call", "custom_tool_call"):
                source = payload.get("arguments") or payload.get("input") or ""
                for match in re.finditer(r'tools\.(exec_command|mcp__\w+?__\w+)\(\{(?:cmd:"((?:[^"\\]|\\.)*)")?', source):
                    name, cmd = match.group(1), match.group(2)
                    items.append(("tool", cmd.replace('\\"', '"') if name == "exec_command" else name.split("__")[-1]))
    # Background Agent subagents keep their own files beside the session.
    for child in sorted((path.parent / path.stem / "subagents").glob("*.jsonl")):
        for line in child.read_text().splitlines():
            message = json.loads(line).get("message") or {}
            if isinstance(message.get("content"), list):
                subagent_ids.update(b["id"] for b in message["content"] if b.get("type") == "tool_use")
    return items, subagent_ids


def luna_items(transcript, provider: str):
    items = []
    for entry in transcript:
        if entry["role"] != "assistant":
            continue
        for part in entry["parts"]:
            if part.get("kind") == "text" and (part.get("text") or "").strip():
                items.append(("text", part["text"].strip()))
            elif part.get("kind") == "tool":
                call = part.get("call") or {}
                if provider == "claude-code":
                    items.append(("tool", part["id"]))
                elif call.get("kind") == "exec":
                    command = re.sub(r"^/bin/zsh -lc '(.*)'$", r"\1", call.get("command", ""))
                    items.append(("tool", command))
                elif call.get("kind") == "mcp":
                    items.append(("tool", call.get("tool")))
    return items


def in_order(needles, haystack):
    position = 0
    for needle in needles:
        try:
            position = haystack.index(needle, position) + 1
        except ValueError:
            return False, needle
    return True, None


def main() -> int:
    run, provider, native_path = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
    transcript = json.loads((run / "state/after.json").read_text())["chats"]["root"]["transcript"]
    native, subagent_ids = native_items(native_path, provider)
    luna = luna_items(transcript, provider)
    checks = {}

    native_tools = [value for kind, value in native if kind == "tool"]
    luna_tools = [value for kind, value in luna if kind == "tool"]
    if provider == "claude-code":
        checks["N1"] = sorted(native_tools) == sorted(luna_tools) and len(set(luna_tools)) == len(luna_tools)
        checks["N2"] = native_tools == luna_tools
    else:
        # Code mode folds a polled session (write_stdin) and pure-JS helpers
        # into no extra chip, so every LUNA chip must be a distinct native call.
        ok, missing = in_order(luna_tools, native_tools)
        checks["N1"] = len(set(luna_tools)) == len(luna_tools) and set(luna_tools) <= set(native_tools)
        checks["N2"] = ok
    native_texts = [value for kind, value in native if kind == "text"]
    luna_texts = [value for kind, value in luna if kind == "text"]
    checks["N3"] = all(text in native_texts for text in luna_texts) and in_order(luna_texts, native_texts)[0]
    order = [entry["id"] for entry in transcript]
    created = [i for i, entry in enumerate(transcript) if entry["id"].startswith("child-created")]
    outcome = [i for i, entry in enumerate(transcript) if entry["id"].startswith("child-outcome")]
    create_call = [i for i, entry in enumerate(transcript)
                   if any((p.get("call") or {}).get("tool") == "create_thread" for p in entry["parts"])]
    checks["N4"] = bool(created) and bool(create_call) and created[0] > create_call[0] and (not outcome or outcome[0] > created[0])
    tools = [p for e in transcript for p in e["parts"] if p.get("kind") == "tool"]
    checks["N5"] = all(p.get("resolved") for p in tools) and all(e["status"] == "complete" for e in transcript)
    checks["N6"] = all(len({p["id"] for p in e["parts"]}) == len(e["parts"]) for e in transcript)
    if provider == "claude-code":
        agents = [p for p in tools if str((p.get("call") or {}).get("name", "")).startswith("Agent")]
        checks["N7"] = bool(subagent_ids) and not (set(luna_tools) & subagent_ids) and all(
            p.get("subagentRef") and p.get("subagentStatus") == "done" for p in agents) and bool(agents)
    snapshots = sorted(glob.glob(str(run / "state/snapshot-*.json")))
    checks["N8"] = bool(snapshots) and json.loads(Path(snapshots[-1]).read_text())["chats"]["root"]["transcript"] == transcript
    report = {"provider": provider, "run": run.name, "entries": len(order), "nativeItems": len(native),
              "lunaItems": len(luna), "checks": checks, "passed": all(checks.values())}
    (run / "transcript-fidelity.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
