#!/usr/bin/env python3
"""Exercise version CLI transactions in a disposable Git workspace with real Cargo metadata."""

import json
import shutil
import subprocess
import tempfile
from datetime import datetime, timezone
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parents[1]


def main():
    evidence = (
        ROOT
        / "target/qa/version-workflow"
        / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    )
    evidence.mkdir(parents=True)
    cases = []
    try:
        with tempfile.TemporaryDirectory(prefix="fixture-", dir=evidence) as temporary:
            root = Path(temporary)
            (root / "scripts").mkdir()
            for name in ["version.py", "cargo.sh"]:
                shutil.copy2(ROOT / "scripts" / name, root / "scripts" / name)
            (root / "Cargo.toml").write_text(
                '[workspace]\nresolver = "2"\nmembers = ["app", "sdk"]\n[workspace.package]\nversion = "0.0.1"\n'
            )
            for name, declaration in [
                ("app", "version.workspace = true"),
                ("sdk", 'version = "0.1.0"'),
            ]:
                (root / name / "src").mkdir(parents=True)
                (root / name / "Cargo.toml").write_text(
                    f'[package]\nname = "fixture-{name}"\n{declaration}\nedition = "2024"\n'
                )
                (root / name / "src/lib.rs").write_text(
                    "// No compile is required for the metadata boundary.\n"
                )
            ios = root / "apps/ios/Luna.xcodeproj/project.pbxproj"
            ios.parent.mkdir(parents=True)
            ios.write_text("MARKETING_VERSION = 0.0.1;\nMARKETING_VERSION = 0.0.1;\n")
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            subprocess.run(
                [str(root / "scripts/cargo.sh"), "generate-lockfile"],
                cwd=root,
                check=True,
                capture_output=True,
            )

            def command(name, args, expected=0):
                result = subprocess.run(
                    ["python3", "scripts/version.py", *args],
                    cwd=root,
                    capture_output=True,
                    text=True,
                    check=False,
                )
                (evidence / f"{name}.log").write_text(result.stdout + result.stderr)
                if result.returncode != expected:
                    raise AssertionError(
                        f"{name}: expected exit {expected}, got {result.returncode}"
                    )
                cases.append(
                    {"name": name, "exitCode": result.returncode, "expected": expected}
                )
                return (
                    json.loads(result.stdout)
                    if expected == 0 and args[0] != "show"
                    else None
                )

            owned = [root / "Cargo.toml", root / "Cargo.lock", ios]
            before = [p.read_bytes() for p in owned]
            assert (
                command("initial", ["check", "--metadata", "--tag", "v0.0.1"])[
                    "version"
                ]
                == "0.0.1"
            )
            assert command("dry-run", ["bump", "minor", "--dry-run"])["to"] == "0.1.0"
            assert [p.read_bytes() for p in owned] == before
            for kind, expected in [
                ("patch", "0.0.2"),
                ("minor", "0.1.0"),
                ("major", "1.0.0"),
            ]:
                assert command(kind, ["bump", kind])["to"] == expected
                assert (
                    command(kind + "-metadata", ["check", "--metadata"])["version"]
                    == expected
                )
                lock = tomllib.loads((root / "Cargo.lock").read_text())
                assert (
                    next(p for p in lock["package"] if p["name"] == "fixture-sdk")[
                        "version"
                    ]
                    == "0.1.0"
                )
            command("wrong-tag", ["check", "--tag", "v9.9.9"], 1)
            ios.write_text("MARKETING_VERSION = 99.0.0;\n")
            before = [p.read_bytes() for p in owned]
            command("inconsistent-ios", ["bump", "patch"], 1)
            assert [p.read_bytes() for p in owned] == before
            ios.write_text("MARKETING_VERSION = 1.0.0;\n")
            manifest = root / "Cargo.toml"
            manifest.write_text(
                manifest.read_text().replace('version = "1.0.0"', 'version = "01.0.0"')
            )
            command("invalid-version", ["check"], 1)
        result = {
            "status": "passed",
            "cases": cases,
            "fixtureRemoved": True,
            "command": "python3 scripts/verify-version-workflow.py",
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
