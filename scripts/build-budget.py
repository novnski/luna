#!/usr/bin/env python3
"""Cooperative, repository-wide build admission and growth guard (no cleanup)."""
import fcntl
import json
import math
import os
from pathlib import Path
import shutil
import signal
import stat
import subprocess
import sys
import time

# Keep the build entrypoint from creating source-tree __pycache__ artifacts.
sys.dont_write_bytecode = True
import prune_incremental

GIB = 1024**3
ROOT = Path(__file__).resolve().parent.parent


def limit(name, default, maximum):
    value = float(os.environ.get(name, default))
    if not math.isfinite(value) or not 0 < value <= maximum:
        raise ValueError(f"{name} must be > 0 and <= {maximum} GiB")
    return int(value * GIB)


def git(*args):
    return subprocess.check_output(["git", "-C", str(ROOT), *args])


def worktrees():
    return [Path(os.fsdecode(field[9:])).resolve()
            for field in git("worktree", "list", "--porcelain", "-z").split(b"\0")
            if field.startswith(b"worktree ")]


def cache_root():
    binary = os.environ.get("LUNA_MBX_BIN") or shutil.which("mbx")
    if not binary:
        binary = str(Path.home() / ".local/bin/mbx")
    if not Path(binary).is_file():
        # Missing-tool fallback still accounts for the standard existing store.
        return Path.home() / ("Library/Caches/mbx" if sys.platform == "darwin"
                              else ".cache/mbx")
    result = subprocess.run([binary, "cache", "stats", "--json"],
                            capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise ValueError(f"cannot account for mbx store: {result.stderr.strip()}")
    return Path(json.loads(result.stdout)["store"]).resolve().parent


def size(path):
    """Count logical OR allocated bytes, whichever is larger; no clone discount."""
    total = 0
    pending = [path]
    seen = set()
    while pending:
        current = pending.pop()
        try:
            info = current.lstat()
        except FileNotFoundError:
            continue  # Cargo removes temporary files while being sampled.
        key = (info.st_dev, info.st_ino)
        if key in seen:
            continue
        seen.add(key)
        total += max(info.st_size, info.st_blocks * 512)
        if stat.S_ISDIR(info.st_mode):
            try:
                pending.extend(current.iterdir())
            except FileNotFoundError:
                continue
    return total


def snapshot(cache):
    paths = sorted({(tree / "target").resolve() for tree in worktrees()} | {cache})
    # Descendants and duplicate canonical roots must not be counted twice.
    roots = [p for p in paths if not any(parent in paths for parent in p.parents)]
    rows = [{"path": str(p), "bytes": size(p)} for p in roots]
    return {"accounting": "max(logical, allocated) per file; APFS clones charged separately",
            "paths": rows, "total_bytes": sum(row["bytes"] for row in rows),
            "free_bytes": shutil.disk_usage(ROOT).free}


def report(data, ceiling, reserve):
    data.update(operating_ceiling_bytes=ceiling, growth_reservation_bytes=reserve,
                absolute_policy_max_bytes=200 * GIB)
    print(json.dumps(data, indent=2), flush=True)


def stop(child):
    # Only the launched build's process group; never another app or build.
    try:
        os.killpg(child.pid, signal.SIGTERM)
        child.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(child.pid, signal.SIGKILL)
        child.wait()
    except ProcessLookupError:
        child.wait()


def main():
    ceiling = limit("LUNA_BUILD_BUDGET_GIB", "170", 170)
    reserve = limit("LUNA_BUILD_RESERVE_GIB", "25", 25)
    cache = cache_root()
    if sys.argv[1:] == ["--status"]:
        report(snapshot(cache), ceiling, reserve)
        return 0
    if len(sys.argv) < 3 or sys.argv[1] != "--":
        raise ValueError("usage: build-budget.py --status | -- command [args...]")
    common = Path(os.fsdecode(git("rev-parse", "--path-format=absolute", "--git-common-dir")).strip())
    # This inode is permanent. Unlinking a lock file can create two lock owners.
    with (common / "luna-build-budget.lock").open("a+") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            lock.seek(0)
            print(f"Build budget busy: {lock.read().strip()}; retry after it finishes", file=sys.stderr)
            return 75
        lock.seek(0)
        lock.truncate()
        lock.write(json.dumps({"pid": os.getpid(), "checkout": str(ROOT), "started": time.time()}))
        lock.flush()
        # This is the only safe point for automatic cleanup: the shared build
        # lock is held and no new Cargo subprocess has started. Scratch target
        # directories used by QA are deliberately excluded.
        if Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve() == ROOT / "target":
            prune_incremental.prune(apply=True, quiet=True)
        before = snapshot(cache)
        report(before, ceiling, reserve)
        if before["total_bytes"] + reserve > ceiling or before["free_bytes"] < reserve + 10 * GIB:
            print("Build blocked: insufficient aggregate reservation or disk headroom; coordinate cleanup", file=sys.stderr)
            return 75
        child = None

        def interrupted(signum, _frame):
            raise InterruptedError(signum)

        for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(sig, interrupted)
        try:
            child = subprocess.Popen(sys.argv[2:], start_new_session=True)
            while True:
                try:
                    result = child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    result = None
                current = snapshot(cache)
                growth = current["total_bytes"] - before["total_bytes"]
                if (current["total_bytes"] >= ceiling or growth >= reserve
                        or current["free_bytes"] < 10 * GIB):
                    print(f"Build stopped: total={current['total_bytes']/GIB:.2f} GiB, growth={growth/GIB:.2f} GiB", file=sys.stderr)
                    if result is None:
                        stop(child)
                    report(current, ceiling, reserve)
                    return 75
                if result is not None:
                    report(current, ceiling, reserve)
                    return result if result >= 0 else 128 - result
        except InterruptedError as error:
            if child is not None:
                stop(child)
            return 128 + error.args[0]
        finally:
            if child is not None and child.poll() is None:
                stop(child)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        print(f"Build budget: {error}", file=sys.stderr)
        sys.exit(2)
