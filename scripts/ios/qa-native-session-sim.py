#!/usr/bin/env python3
"""IOS-00 P01 / RM-03 P12 / IOS-02 P03: coordinate a native test with the real engine gateway.

Run this, then run the prepared native test it names on the same Simulator
(test-without-building, -parallel-testing-enabled NO):
- `--campaign ios00` (default): `LunaTests/RemoteSessionTests`. The engine
  campaign `remote_access::gateway::mobile_session_...` pairs a disposable
  profile, writes a private fixture into the Luna app's Documents (hosted unit
  tests run inside the app), revokes on cue and verifies the public result.
- `--campaign rm03`: `LunaTests/RemoteTasksRuntimeTests`. The engine campaign
  `remote_access::gateway::mobile_runtime_follows_...` writes a `luna://pair`
  code minted by CreateRemotePairing, renames and restarts the Mac on cue and
  verifies the public result.
- `--campaign ios02`: `LunaUITests/AdaptiveShellUITests` (IOS-02 P03). The
  native-only engine campaign `remote_access::gateway::ios02_native_shell_...`
  (ignored in CI) seeds two projects and five tasks, writes a `luna://pair`
  code into the UI test runner's Documents, deletes the open task, stops and
  restarts the Mac on cue and verifies the public result. The summary also
  lists the app's `LunaRemote` files left after the run (Forget removes the
  QA Mac's cache and drafts).

No UI automation happens here. The bearer in the fixture never reaches stdout
or the summary; the fixture and cue files are removed afterwards. The app's
data container may move on reinstall; this follows its logical identity.
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
APP = "com.novnski.luna.ios"
RUNNER = "com.novnski.luna.ios.uitests.xctrunner"
# Per campaign: fixture first, public result last; every file is private and removed afterwards.
CAMPAIGNS = {
    "ios00": {
        "files": ("ios00-session.json", "ios00-revoke-ready.json", "ios00-revoked.json",
                  "ios00-native-result.json"),
        "test": "gateway::mobile_session_reads_typed_identity_and_every_cancellation_reaches_the_engine",
        "fixture_env": "LUNA_NATIVE_SESSION_FIXTURE",
        "native": "LunaTests/RemoteSessionTests",
    },
    "rm03": {
        "files": ("rm03-runtime.json", "rm03-native-live.json", "rm03-host-renamed.json",
                  "rm03-native-suspended.json", "rm03-host-changed.json", "rm03-native-restarted.json",
                  "rm03-host-restarted.json", "rm03-native-result.json"),
        "test": "gateway::mobile_runtime_follows_mac_tasks_across_suspend_network_restart_and_cursor_repair",
        "fixture_env": "LUNA_NATIVE_RUNTIME_FIXTURE",
        "native": "LunaTests/RemoteTasksRuntimeTests",
    },
    "ios02": {
        "files": ("ios02-shell.json", "ios02-native-delete.json", "ios02-host-deleted.json",
                  "ios02-native-stop.json", "ios02-host-stopped.json", "ios02-native-restart.json",
                  "ios02-host-restarted.json", "ios02-native-result.json"),
        "test": "gateway::ios02_native_shell_serves_the_phone_and_deletes_stops_and_restarts_on_cue",
        "fixture_env": "LUNA_NATIVE_SHELL_FIXTURE",
        "native": "LunaUITests/AdaptiveShellUITests",
        # UI tests read their fixture from the runner's own Documents.
        "container": RUNNER,
        "ignored": True,
    },
}
SECRET_KEYS = {"sessionToken", "session_token", "credential", "authorization", "bearer", "ticket"}


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


def keys(value):
    if isinstance(value, dict):
        return set(value) | set().union(*(keys(v) for v in value.values()))
    if isinstance(value, list):
        return set().union(*(keys(v) for v in value))
    return set()


def preference_keys(app):
    """Every key the app's preferences hold, including inside its JSON blobs."""
    try:
        preferences = plistlib.loads((app / "Library/Preferences" / (APP + ".plist")).read_bytes())
    except (OSError, plistlib.InvalidFileException):
        return set()
    found = set(preferences)
    for value in preferences.values():
        if isinstance(value, bytes):
            try:
                found |= keys(json.loads(value))
            except ValueError:
                pass
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--simulator", required=True, help="UDID of an owned, disposable Simulator")
    parser.add_argument("--run-dir", required=True, type=Path, help="new directory for this run's evidence")
    parser.add_argument("--host-ip", required=True,
                        help="this Mac's LAN address for the gateway (not loopback, not a VPN route)")
    parser.add_argument("--campaign", choices=sorted(CAMPAIGNS), default="ios00")
    args = parser.parse_args()
    campaign = CAMPAIGNS[args.campaign]
    FILES, TEST = campaign["files"], campaign["test"]
    bundle = campaign.get("container", APP)
    run = args.run_dir.resolve()
    run.mkdir(parents=True, mode=0o700)
    if container(args.simulator, APP) is None:
        raise RuntimeError("Install the prepared Luna app on this Simulator before starting")
    data = container(args.simulator, bundle)
    if data is None:
        raise RuntimeError("Install the prepared XCTest runner on this Simulator before starting")
    documents = data / "Documents"
    documents.mkdir(exist_ok=True)
    for name in FILES:
        (documents / name).unlink(missing_ok=True)
    link = run / "app-documents"
    link.symlink_to(documents, target_is_directory=True)
    log = run / "host-e2e-private.log"
    environment = {**os.environ, "CARGO_INCREMENTAL": "0", "LUNA_REMOTE_TEST_IP": args.host_ip,
                   campaign["fixture_env"]: str(link / FILES[0])}
    command = [str(ROOT / "scripts/cargo.sh"), "test", "--locked", "-p", "luna-engine",
               "--test", "remote_access", TEST, "--", "--exact"]
    if campaign.get("ignored"):
        command.append("--include-ignored")
    ready = False
    secrets = set()
    started = time.monotonic()
    with log.open("w") as output:
        process = subprocess.Popen(command, cwd=ROOT, env=environment, stdout=output, stderr=subprocess.STDOUT)
        while process.poll() is None:
            app = container(args.simulator, APP)
            if app is not None:
                secrets |= preference_keys(app) & SECRET_KEYS
            current = container(args.simulator, bundle)
            if current is not None:
                if current / "Documents" != documents:
                    documents = current / "Documents"
                    replacement = run / "app-documents-next"
                    replacement.symlink_to(documents, target_is_directory=True)
                    replacement.replace(link)
            if not ready and (link / FILES[0]).exists():
                ready = True
                print(f"Native fixture ready. Run the prepared {campaign['native']} now.", flush=True)
            time.sleep(.5)
        code = process.wait()
    result = None
    if (link / FILES[-1]).exists():
        result = json.loads((link / FILES[-1]).read_text())
    summary = {"platform": "iOS Simulator", "simulator": args.simulator, "campaign": args.campaign,
               "hostTest": TEST,
               "hostExitCode": code, "durationSeconds": round(time.monotonic() - started, 2),
               "nativeObservations": result, "secretKeysInPreferences": sorted(secrets)}
    if args.campaign == "ios02":
        app = container(args.simulator, APP)
        remote = app / "Library/Application Support/LunaRemote" if app else None
        summary["lunaRemoteFiles"] = sorted(p.name for p in remote.iterdir()) if remote and remote.is_dir() else []
    (run / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    for name in FILES:
        (link / name).unlink(missing_ok=True)
    link.unlink()
    print(f"Native session E2E host exit={code}; public summary={run / 'summary.json'}", flush=True)
    if code or not result or secrets:
        raise SystemExit(code or 1)


if __name__ == "__main__":
    main()
