#!/usr/bin/env python3
"""Verify installed Alpha remains frozen through real Dev refresh and channel switching."""

import hashlib
import json
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]
ALPHA = Path.home() / "Applications/LUNA Alpha.app"
DEV = ROOT / "target/luna-macos-dev/LUNA Dev.app"


def fingerprint(app):
    result = {}
    for path in sorted(app.rglob("*")):
        if path.is_file() and path.name != ".DS_Store":
            with path.open("rb") as stream:
                result[str(path.relative_to(app))] = hashlib.file_digest(
                    stream, "sha256"
                ).hexdigest()
    return result


def main():
    evidence = (
        ROOT
        / "target/qa/macos-app-workflow"
        / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    )
    evidence.mkdir(parents=True)
    cases = []
    before = fingerprint(ALPHA)
    if not before:
        raise SystemExit(
            "Install the first Alpha snapshot before this native verification"
        )

    def command(name, arguments, expected=0, env=None):
        result = subprocess.run(
            arguments, cwd=ROOT, env=env, capture_output=True, text=True, check=False
        )
        (evidence / f"{name}.log").write_text(result.stdout + result.stderr)
        if result.returncode != expected:
            raise AssertionError(
                f"{name}: expected exit {expected}, got {result.returncode}; see {evidence / (name + '.log')}"
            )
        cases.append(
            {"name": name, "exitCode": result.returncode, "expected": expected}
        )
        return result.stdout

    def identity(channel, app, expected_id, expected_port, expected_root):
        data = plistlib.loads((app / "Contents/Info.plist").read_bytes())
        assert data["CFBundleIdentifier"] == expected_id
        assert data["LSEnvironment"]["LUNA_IPC_PORT"] == str(expected_port)
        assert data["LSEnvironment"]["LUNA_DATA_DIR"] == str(expected_root)
        environment = dict(os.environ, **data["LSEnvironment"])
        command(
            channel + "-signature",
            ["codesign", "--verify", "--deep", "--strict", str(app)],
        )
        for binary in ["luna", "luna-extension-host", "luna-opencode-worker"]:
            assert os.access(app / "Contents/MacOS" / binary, os.X_OK)
        output = json.loads(
            command(
                channel + "-rpc",
                [str(app / "Contents/MacOS/luna"), "inspect", "--json", "status"],
                env=environment,
            )
        )
        assert "error" not in output and output["data"]["ready"]
        machine = output["data"]["machineId"]
        assert (expected_root / "device-id").read_text().strip() == machine
        version = command(
            channel + "-version",
            [str(app / "Contents/MacOS/luna"), "--version"],
            env=environment,
        )
        base = data["CFBundleShortVersionString"].split("-")[0]
        assert version.strip() == f"luna {base}"
        return machine, environment

    tool = ["python3", "scripts/macos-app.py"]
    try:
        command("open-alpha", [*tool, "alpha"])
        alpha_id, alpha_env = identity(
            "alpha", ALPHA, "com.novnski.luna.alpha", 59779, Path.home() / ".luna-alpha"
        )
        assert alpha_env["LUNA_WORKTREES_DIR"] == str(
            Path.home() / ".luna-alpha/worktrees"
        )
        command("repeat-install-refused", [*tool, "install-alpha"], 1)
        command(
            "alpha-updater-refused",
            [str(ALPHA / "Contents/MacOS/luna"), "update"],
            1,
            alpha_env,
        )
        assert (
            "explicit update-alpha"
            in (evidence / "alpha-updater-refused.log").read_text()
        )
        with tempfile.TemporaryDirectory(
            prefix="renamed-fixture-", dir=evidence
        ) as temporary:
            renamed = Path(temporary) / "LUNA.app"
            shutil.copytree(ALPHA, renamed)
            command(
                "renamed-alpha-updater-refused",
                [str(renamed / "Contents/MacOS/luna"), "update"],
                1,
                alpha_env,
            )
            assert (
                "explicit update-alpha"
                in (evidence / "renamed-alpha-updater-refused.log").read_text()
            )
        command("development-refresh", [*tool, "dev"])
        dev_id, _ = identity(
            "dev",
            DEV,
            "com.novnski.luna.dev",
            59777,
            ROOT / "target/luna-macos-dev/data",
        )
        assert alpha_id != dev_id
        assert fingerprint(ALPHA) == before, "Dev refresh changed the Alpha bundle"
        command("return-to-alpha", [*tool, "alpha"])
        after_id, _ = identity(
            "alpha-after",
            ALPHA,
            "com.novnski.luna.alpha",
            59779,
            Path.home() / ".luna-alpha",
        )
        assert after_id == alpha_id
        assert fingerprint(ALPHA) == before, "channel switch replaced Alpha"
        running = json.loads(command("final-status", [*tool, "status"]))["running"]
        assert len(running) == 1 and running[0]["bundle"] == str(ALPHA)
        (evidence / "alpha-sha256.json").write_text(json.dumps(before, indent=2) + "\n")
        result = {
            "status": "passed",
            "cases": cases,
            "alphaMachineId": alpha_id,
            "devMachineId": dev_id,
            "alphaBundleUnchanged": True,
            "onlyRunningApp": str(ALPHA),
            "command": "python3 scripts/verify-macos-app-workflow.py",
        }
    except BaseException as error:
        result = {"status": "failed", "cases": cases, "error": str(error)}
        (evidence / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        raise
    (evidence / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(
        json.dumps(
            {
                "status": result["status"],
                "cases": len(cases),
                "evidence": str(evidence),
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
