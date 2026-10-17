#!/usr/bin/env python3
"""Bounded cargo-mutants campaigns under LUNA's existing build admission guard."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
VERSION = "27.1.0"
BASE = ROOT / "target" / "mutants"
TOOL = BASE / "tool" / "bin" / "cargo-mutants"


def positive(value):
    number = int(value)
    if number <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return number


def guarded_call(command, env, output=None):
    # Let build-budget terminate and reap its process group before scratch
    # cleanup. subprocess.call's KeyboardInterrupt path can kill the guard early.
    child = subprocess.Popen(command, env=env, stdout=output, start_new_session=True,
                             stderr=subprocess.STDOUT if output else None)
    previous = {}
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        previous[sig] = signal.signal(sig, lambda signum, _frame: child.send_signal(signum))
    try:
        result = child.wait()
        return result if result >= 0 else 128 - result
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--install", action="store_true", help=f"install cargo-mutants {VERSION} locally")
    parser.add_argument("--list", action="store_true", help="list candidates as JSON without compiling")
    parser.add_argument("-p", "--package", action="append", help="package to mutate (default: luna-proto)")
    parser.add_argument("-f", "--file", action="append", help="source glob relative to the workspace")
    parser.add_argument("--in-diff", type=Path, help="only lines changed by this unified diff")
    parser.add_argument("--test-package", action="append", help="package owning the behavior's tests")
    parser.add_argument("--test-target", help="integration test target in the test package")
    parser.add_argument("--timeout", type=positive, default=60, help="seconds per test phase (default: 60)")
    parser.add_argument("--build-timeout", type=positive, default=300, help="seconds per build phase (default: 300)")
    args = parser.parse_args()
    os.chdir(ROOT)
    if (ROOT / "target").is_symlink() or BASE.resolve() != BASE:
        parser.error("target/mutants must be inside this checkout, without symlinks")
    for name in ("CARGO_BUILD_BUILD_DIR", "CARGO_BUILD_TARGET_DIR"):
        if os.environ.get(name):
            parser.error(f"unset {name}; builds must stay inside the checkout target/")
    cargo = shutil.which("cargo")
    if not cargo:
        parser.error("Cargo is not on PATH")
    env = dict(os.environ, CARGO=cargo, CARGO_TARGET_DIR=str(ROOT / "target"),
               CARGO_INCREMENTAL="0", CARGO_TERM_COLOR="never")
    guard = [sys.executable, str(ROOT / "scripts/build-budget.py"), "--"]
    if args.install:
        # Installation is also guarded and keeps its disposable build graph in
        # target/. Never install globally or replace the user's Cargo toolchain.
        return guarded_call(guard + [cargo, "install", "cargo-mutants", "--version", f"={VERSION}",
                                      "--locked", "--root", str(BASE / "tool"),
                                      "--target-dir", str(BASE / "tool-build")], env=env)
    if not TOOL.is_file():
        parser.error("run scripts/cargo.sh mutants --install first")
    version = subprocess.check_output([str(TOOL), "mutants", "--version"], text=True).strip()
    if version != f"cargo-mutants {VERSION}":
        parser.error(f"expected cargo-mutants {VERSION}, found {version}; rerun --install")
    if args.in_diff:
        args.in_diff = args.in_diff.resolve(strict=True)
    runs = BASE / "runs"
    runs.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ-")
    run = Path(tempfile.mkdtemp(prefix=stamp, dir=runs))
    command = [str(TOOL), "mutants", "--dir", str(ROOT), "--output", str(run),
               "--jobs", "1", "--no-shuffle", "--baseline", "run",
               "--timeout", str(args.timeout), "--build-timeout", str(args.build_timeout)]
    for package in args.package or ["luna-proto"]:
        command += ["--package", package]
    # With no scope supplied, use a tiny real contract to make first use bounded.
    files = args.file or ([] if args.package or args.in_diff
                          else ["crates/proto/src/context_control.rs"])
    for file in files:
        command += ["--file", file]
    if args.in_diff:
        command += ["--in-diff", str(args.in_diff)]
    for package in args.test_package or []:
        command += ["--test-package", package]
    if args.test_target:
        command += ["--cargo-test-arg=--test", f"--cargo-test-arg={args.test_target}"]
    if args.list:
        command += ["--list", "--json"]
    record = {"version": version, "command": command,
              "invocation": [str(ROOT / "scripts/cargo.sh"), "mutants", *sys.argv[1:]],
              "revision": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
              "working_tree": subprocess.check_output(["git", "status", "--porcelain"], text=True),
              "started_at": datetime.now(timezone.utc).isoformat(), "exit_code": None}
    receipt = run / "run.json"
    receipt.write_text(json.dumps(record, indent=2) + "\n")
    print(f"Mutation evidence: {run}", flush=True)
    started = time.monotonic()
    # tempfile honors TMPDIR. All copied sources (and test temp files) are now
    # counted by build-budget.py. Cargo reuses the checkout's target/ci cache.
    with tempfile.TemporaryDirectory(prefix="scratch-", dir=BASE) as scratch:
        env.update(TMPDIR=scratch, TMP=scratch, TEMP=scratch)
        with (run / "console.log").open("w") as log:
            result = guarded_call(guard + command, env=env, output=log)
    if result == 0 and not args.list:
        outcomes = run / "mutants.out" / "outcomes.json"
        if not outcomes.is_file() or json.loads(outcomes.read_text())["total_mutants"] == 0:
            record["error"] = "No mutants tested; check package, file, and diff selection"
            print(record["error"], file=sys.stderr)
            result = 1
    record.update(exit_code=result, elapsed_seconds=round(time.monotonic() - started, 2))
    receipt.write_text(json.dumps(record, indent=2) + "\n")
    print(f"Exit {result}; {receipt}", flush=True)
    return result


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, subprocess.SubprocessError) as error:
        print(f"Mutation testing: {error}", file=sys.stderr)
        sys.exit(1)
