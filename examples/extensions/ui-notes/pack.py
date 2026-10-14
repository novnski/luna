#!/usr/bin/env python3
"""Pack the CORE-12 example UI packages into OUT.

    python3 examples/extensions/ui-notes/pack.py OUT [VERSION]

Writes two package directories (CORE-06 manifest version 1, extension API
1.3) and prints each one's path and package hash (the SHA-256 of its exact
luna-plugin.json bytes, which pin every other file):

- OUT/notes      `core12.qa/notes`, data only: a command, a settings
                 section, a sidebar page and a task panel. Nothing runs.
- OUT/tasknotes  `core12.qa/tasknotes`, a native worker (python3 on PATH):
                 a task panel whose view and Ping action come from the
                 worker, and a task command. Native code is not sandboxed.

VERSION (default 1.0.0) sets both packages' version, so packing 1.0.1 gives
an update candidate with a different hash. Nothing here talks to LUNA;
install the results with Settings -> Extensions -> Install... or the
InstallExtension owner method.
"""
import hashlib
import json
import os
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def platform():
    system = {"darwin": "macos"}.get(sys.platform, sys.platform)
    machine = os.uname().machine.replace("arm64", "aarch64")
    return "%s-%s" % (system, machine)


def pack(out, name, manifest, files):
    dest = os.path.join(out, name)
    if os.path.exists(dest):
        if not os.path.exists(os.path.join(dest, "luna-plugin.json")):
            sys.exit("%s exists and is not a package; refusing to replace it" % dest)
        shutil.rmtree(dest)
    os.makedirs(dest)
    listed = []
    for rel, data in files:
        target = os.path.join(dest, rel)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "wb") as handle:
            handle.write(data)
        if rel.startswith("bin/"):
            os.chmod(target, 0o755)
        listed.append({"path": rel, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()})
    manifest["files"] = listed
    raw = json.dumps(manifest, indent=2).encode()
    with open(os.path.join(dest, "luna-plugin.json"), "wb") as handle:
        handle.write(raw)
    print(json.dumps({"dir": dest, "packageHash": hashlib.sha256(raw).hexdigest()}))


def read(*parts):
    with open(os.path.join(HERE, *parts), "rb") as handle:
        return handle.read()


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    out = os.path.abspath(sys.argv[1])
    version = sys.argv[2] if len(sys.argv) > 2 else "1.0.0"
    os.makedirs(out, exist_ok=True)
    base = {
        "manifestVersion": 1,
        "version": version,
        "publisher": "CORE-12 QA",
        "api": {"major": 1, "minMinor": 3},
    }
    pack(out, "notes", dict(base, **{
        "id": "core12.qa/notes",
        "displayName": "CORE-12 notes",
        "description": "Declarative UI contributions with no code.",
        "runtime": {"kind": "data"},
        "contributions": [
            {"id": "core12.qa/notes.open", "kind": "command"},
            {"id": "core12.qa/notes.settings", "kind": "settingsSection"},
            {"id": "core12.qa/notes.page", "kind": "sidebarSection"},
            {"id": "core12.qa/notes.panel", "kind": "panel"},
        ],
    }), [("luna-ui.json", read("notes", "luna-ui.json"))])
    pack(out, "tasknotes", dict(base, **{
        "id": "core12.qa/tasknotes",
        "displayName": "CORE-12 live task notes",
        "description": "A task panel whose view and action come from a native worker.",
        "runtime": {
            "kind": "native",
            "protocol": {"major": 1, "minMinor": 0},
            "entries": {platform(): "bin/worker"},
        },
        "contributions": [
            {"id": "core12.qa/tasknotes.panel", "kind": "panel"},
            {"id": "core12.qa/tasknotes.ping", "kind": "command"},
        ],
    }), [
        ("luna-ui.json", read("tasknotes", "luna-ui.json")),
        ("bin/worker", read("tasknotes", "bin", "worker")),
        ("VERSION", (version + "\n").encode()),
    ])


if __name__ == "__main__":
    main()
