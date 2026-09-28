#!/usr/bin/env python3
"""Trim old duplicate Cargo incremental caches in this checkout's debug target.

The build guard calls this while holding its repository-wide lock, before it
starts Cargo. This never touches Cargo dependencies, app bundles, or user data.
"""

import argparse
from collections import defaultdict
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parent.parent
INCREMENTAL = ROOT / "target/debug/incremental"
TRIGGER_BYTES = 12 * 1024**3
MIN_AGE_SECONDS = 24 * 60 * 60
KEEP_PER_CRATE = 2
NAME = re.compile(r"([A-Za-z0-9_]+)-[0-9a-z]{12,20}\Z")


def usage(path):
    """Use the same conservative accounting as build-budget.py."""
    total = 0
    latest = 0.0
    pending = [path]
    while pending:
        current = pending.pop()
        info = current.lstat()
        total += max(info.st_size, info.st_blocks * 512)
        latest = max(latest, info.st_mtime)
        if stat.S_ISDIR(info.st_mode):
            pending.extend(current.iterdir())
    return total, latest


def candidates(now):
    for path in (ROOT / "target", ROOT / "target/debug", INCREMENTAL):
        if path.is_symlink():
            raise ValueError(f"refusing symlink in incremental target: {path}")
    if not INCREMENTAL.exists():
        return 0, []
    if not INCREMENTAL.is_dir():
        raise ValueError(f"incremental target is not a directory: {INCREMENTAL}")

    groups = defaultdict(list)
    total = 0
    for path in INCREMENTAL.iterdir():
        if path.is_symlink() or not path.is_dir():
            continue
        match = NAME.fullmatch(path.name)
        if not match or match.group(1) == "build_script_build":
            continue  # Build-script directories from different crates share a name.
        size, latest = usage(path)
        total += size
        info = path.lstat()
        groups[match.group(1)].append((latest, path, size, info.st_dev, info.st_ino))

    selected = []
    if total > TRIGGER_BYTES:
        for entries in groups.values():
            entries.sort(key=lambda entry: entry[0], reverse=True)
            selected.extend(entry for entry in entries[KEEP_PER_CRATE:]
                            if now - entry[0] >= MIN_AGE_SECONDS)
    return total, sorted(selected, key=lambda entry: entry[0])


def prune(*, apply, quiet):
    total, selected = candidates(time.time())
    selected_bytes = sum(entry[2] for entry in selected)
    action = "removing" if apply else "would remove"
    print(f"Incremental cache: {total / 1024**3:.2f} GiB accounted; "
          f"{action} {len(selected)} old duplicate directories "
          f"({selected_bytes / 1024**3:.2f} GiB accounted)", flush=True)
    if not quiet:
        for _, path, size, _, _ in selected:
            print(f"  {size / 1024**2:.0f} MiB {path}")
    if apply:
        for _, path, _, dev, ino in selected:
            info = path.lstat()
            if not stat.S_ISDIR(info.st_mode) or (info.st_dev, info.st_ino) != (dev, ino):
                raise ValueError(f"incremental cache changed during cleanup: {path}")
            shutil.rmtree(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apply", action="store_true", help="remove eligible caches")
    parser.add_argument("--quiet", action="store_true", help="show totals only")
    args = parser.parse_args()
    if not args.apply:
        prune(apply=False, quiet=args.quiet)
        return 0

    common = Path(os.fsdecode(subprocess.check_output(
        ["git", "-C", str(ROOT), "rev-parse", "--path-format=absolute", "--git-common-dir"])).strip())
    with (common / "luna-build-budget.lock").open("a+") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print("Incremental cleanup: build lock is busy", file=sys.stderr)
            return 75
        lock.seek(0)
        lock.truncate()
        lock.write(json.dumps({"pid": os.getpid(), "checkout": str(ROOT),
                               "started": time.time(), "action": "incremental cleanup"}))
        lock.flush()
        prune(apply=True, quiet=args.quiet)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"Incremental cleanup: {error}", file=sys.stderr)
        sys.exit(2)
