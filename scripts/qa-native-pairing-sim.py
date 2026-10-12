#!/usr/bin/env python3
"""Coordinate prepared XCTest products with the real engine gateway E2E.

Run this, then use XcodeBuildMCP test_sim with the prepared products, the same
simulator and -parallel-testing-enabled NO. No UI automation is performed here.
The runner's data container may move on reinstall; follow its logical identity.
Raw bootstrap input remains private and is removed after the run.
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
RUNNER = "com.novnski.luna.ios.uitests.xctrunner"
APP = "com.novnski.luna.ios"
FILES = ("rm02-pairing.json", "rm02-revoke-ready.json", "rm02-revoked.json", "rm02-native-result.json")


def container(simulator, bundle):
    result = subprocess.run(["xcrun", "simctl", "get_app_container", simulator, bundle, "data"],
                            text=True, capture_output=True)
    if result.returncode:
        return None
    path = Path(result.stdout.strip()).resolve()
    expected = Path.home() / "Library/Developer/CoreSimulator/Devices" / simulator
    if expected not in path.parents:
        raise RuntimeError("Unexpected simulator container; refusing fixture writes")
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--simulator", required=True)
    parser.add_argument("--run-dir", required=True, type=Path)
    args = parser.parse_args()
    run = args.run_dir.resolve()
    run.mkdir(parents=True, mode=0o700)
    documents = container(args.simulator, RUNNER)
    if documents is None:
        raise RuntimeError("Install the prepared XCTest runner before starting")
    documents /= "Documents"
    documents.mkdir(exist_ok=True)
    for name in FILES:
        (documents / name).unlink(missing_ok=True)
    link = run / "runner-documents"
    link.symlink_to(documents, target_is_directory=True)
    log = run / "host-e2e-private.log"
    environment = {**os.environ, "CARGO_INCREMENTAL": "0", "LUNA_REMOTE_TEST_IP": "192.168.18.2",
                   "LUNA_NATIVE_PAIRING_FIXTURE": str(link / FILES[0])}
    command = [str(ROOT / "scripts/cargo.sh"), "test", "--locked", "-p", "luna-engine",
               "--test", "remote_access", "gateway::enrollment_preflights_identity_before_spending_a_code_and_commits_only_after_storage",
               "--", "--exact"]
    ready = False
    snapshots = []
    seen = set()
    started = time.monotonic()
    with log.open("w") as output:
        process = subprocess.Popen(command, cwd=ROOT, env=environment, stdout=output, stderr=subprocess.STDOUT)
        while process.poll() is None:
            current = container(args.simulator, RUNNER)
            if current is not None:
                current /= "Documents"
                if current != documents:
                    documents = current
                    replacement = run / "runner-documents-next"
                    replacement.symlink_to(documents, target_is_directory=True)
                    replacement.replace(link)
            if not ready and (link / FILES[0]).exists():
                ready = True
                print("Native fixtures ready. Run the prepared RemotePairingUITests through XcodeBuildMCP.", flush=True)
            app = container(args.simulator, APP)
            if app:
                preferences = app / "Library/Preferences" / (APP + ".plist")
                try:
                    raw = plistlib.loads(preferences.read_bytes()).get("luna.paired-macs.v1")
                    if isinstance(raw, bytes):
                        metadata = json.loads(raw)
                        def keys(value):
                            if isinstance(value, dict):
                                return set(value) | set().union(*(keys(v) for v in value.values()))
                            if isinstance(value, list):
                                return set().union(*(keys(v) for v in value))
                            return set()
                        forbidden = keys(metadata) & {"sessionToken", "session_token", "credential", "authorization", "bearer"}
                        state = tuple(sorted(row.get("name", "") for row in metadata))
                        if state not in seen:
                            seen.add(state)
                            snapshots.append({"savedHosts": list(state), "secretFieldsInPreferences": sorted(forbidden)})
                except (OSError, ValueError, plistlib.InvalidFileException):
                    pass
            time.sleep(.5)
        code = process.wait()
    result = None
    if (link / FILES[-1]).exists():
        result = json.loads((link / FILES[-1]).read_text())
    summary = {"platform": "iOS Simulator", "simulator": args.simulator, "hostExitCode": code,
               "durationSeconds": round(time.monotonic() - started, 2), "nativeObservations": result,
               "preferences": snapshots}
    (run / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    for name in FILES:
        (link / name).unlink(missing_ok=True)
    link.unlink()
    print(f"Native host E2E exit={code}; public summary={run / 'summary.json'}", flush=True)
    if code or not result or any(row["secretFieldsInPreferences"] for row in snapshots):
        raise SystemExit(code or 1)


if __name__ == "__main__":
    main()
