#!/usr/bin/env python3
"""IOS-00 old-data evidence: read a Simulator app's persisted data from outside it.

  qa-data-continuity-sim.py snapshot --simulator UDID --out before.json
  qa-data-continuity-sim.py compare before.json after.json [--allow PATTERN ...]

A snapshot of com.novnski.luna.ios records the installed bundle's identity
and version, every preference with a SHA-256 of its canonical value (the
paired-Mac metadata is decoded and recorded per host and field), and every
file under Library/Application Support and Documents with its size and
SHA-256. Only hashes are written, so a snapshot holds no secret. Keychain
items cannot be read from the host: the app's own Connect after the install
is their readback, recorded separately.

`compare` lists every added, removed and changed entry and exits 1 when any
change does not match an --allow pattern (fnmatch on the entry name, such as
'paired-macs:*:lastAuthenticated'). Use it around an upgrade (old build, then
the IOS-00 build installed over it) and a downgrade (the reverse).
"""
import argparse
from datetime import datetime
import fnmatch
import hashlib
import json
from pathlib import Path
import plistlib
import subprocess
import sys

APP = "com.novnski.luna.ios"
PAIRED_MACS = "luna.paired-macs.v1"


def container(simulator, kind):
    result = subprocess.run(["xcrun", "simctl", "get_app_container", simulator, APP, kind],
                            text=True, capture_output=True)
    if result.returncode:
        raise SystemExit(f"{APP} is not installed on {simulator}")
    path = Path(result.stdout.strip()).resolve()
    if Path.home() / "Library/Developer/CoreSimulator/Devices" / simulator not in path.parents:
        raise SystemExit("Unexpected simulator container; refusing to read it")
    return path


def canonical(value):
    if isinstance(value, bytes):
        return {"$data": hashlib.sha256(value).hexdigest()}
    if isinstance(value, datetime):
        return {"$date": value.isoformat()}
    if isinstance(value, dict):
        return {str(key): canonical(item) for key, item in sorted(value.items())}
    if isinstance(value, list):
        return [canonical(item) for item in value]
    return value


def digest(value):
    return hashlib.sha256(json.dumps(canonical(value), sort_keys=True).encode()).hexdigest()


def snapshot(simulator):
    app = container(simulator, "app")
    data = container(simulator, "data")
    info = plistlib.loads((app / "Info.plist").read_bytes())
    entries = {}
    preferences_path = data / "Library/Preferences" / f"{APP}.plist"
    preferences = plistlib.loads(preferences_path.read_bytes()) if preferences_path.exists() else {}
    for key, value in preferences.items():
        entries[f"defaults:{key}"] = digest(value)
        if key == PAIRED_MACS and isinstance(value, bytes):
            for host in json.loads(value):
                for field, item in host.items():
                    entries[f"paired-macs:{host.get('id', '?')}:{field}"] = digest(item)
    for base in ("Library/Application Support", "Documents"):
        root = data / base
        if not root.exists():
            continue
        for path in sorted(root.rglob("*")):
            if path.is_file() and not path.is_symlink():
                bytes_ = path.read_bytes()
                entries[f"file:{path.relative_to(data)}"] = f"{len(bytes_)}:{hashlib.sha256(bytes_).hexdigest()}"
    return {
        "schema": 1,
        "simulator": simulator,
        "bundle": {key: info.get(key) for key in
                   ("CFBundleIdentifier", "CFBundleShortVersionString", "CFBundleVersion", "MinimumOSVersion")},
        "entries": entries,
    }


def compare(before, after, allowed):
    old, new = before["entries"], after["entries"]
    changes = []
    for name in sorted(set(old) | set(new)):
        if name not in new:
            changes.append(("removed", name))
        elif name not in old:
            changes.append(("added", name))
        elif old[name] != new[name]:
            changes.append(("changed", name))
    unexpected = [(kind, name) for kind, name in changes
                  if not any(fnmatch.fnmatch(name, pattern) for pattern in allowed)]
    return {
        "bundleBefore": before["bundle"],
        "bundleAfter": after["bundle"],
        "sameBundleIdentifier": before["bundle"]["CFBundleIdentifier"] == after["bundle"]["CFBundleIdentifier"],
        "changes": [{"kind": kind, "entry": name} for kind, name in changes],
        "unexpected": [{"kind": kind, "entry": name} for kind, name in unexpected],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    take = commands.add_parser("snapshot")
    take.add_argument("--simulator", required=True)
    take.add_argument("--out", required=True, type=Path)
    diff = commands.add_parser("compare")
    diff.add_argument("before", type=Path)
    diff.add_argument("after", type=Path)
    diff.add_argument("--allow", action="append", default=[])
    args = parser.parse_args()
    if args.command == "snapshot":
        args.out.write_text(json.dumps(snapshot(args.simulator), indent=2) + "\n")
        print(f"snapshot {args.out}")
        return
    report = compare(json.loads(args.before.read_text()), json.loads(args.after.read_text()), args.allow)
    print(json.dumps(report, indent=2))
    if report["unexpected"] or not report["sameBundleIdentifier"]:
        sys.exit(1)


if __name__ == "__main__":
    main()
