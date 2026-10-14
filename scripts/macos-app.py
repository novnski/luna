#!/usr/bin/env python3
"""Build explicit Alpha snapshots and switch safely between Alpha and LUNA Dev."""

import argparse
import hashlib
import json
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path

sys.dont_write_bytecode = True
import version

ROOT = Path(__file__).resolve().parents[1]
ALPHA = Path.home() / "Applications/LUNA Alpha.app"
ALPHA_DATA = Path.home() / ".luna-alpha"
DEV = ROOT / "target/luna-macos-dev/LUNA Dev.app"
BINARIES = ["luna", "luna-extension-host", "luna-opencode-worker"]
ALPHA_ID = "com.novnski.luna.alpha"
ALPHA_PORT = 59779


def run(*args, env=None):
    return subprocess.check_output(args, cwd=ROOT, text=True, env=env).strip()


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def apps():
    result = []
    for line in run("ps", "-axo", "pid=,comm=").splitlines():
        fields = line.strip().split(maxsplit=1)
        if len(fields) == 2 and fields[1].endswith(".app/Contents/MacOS/luna"):
            result.append((int(fields[0]), Path(fields[1]).parents[2]))
    return result


def info(app):
    if app.is_symlink() or app.resolve() != app:
        raise ValueError(f"refusing a redirected bundle: {app}")
    with (app / "Contents/Info.plist").open("rb") as stream:
        data = plistlib.load(stream)
    if app == ALPHA:
        expected = {
            "CFBundleIdentifier": ALPHA_ID,
            "CFBundleDisplayName": "LUNA Alpha",
            "LUNAUpdatePolicy": "manual",
        }
        for key, value in expected.items():
            if data.get(key) != value:
                raise ValueError(f"unexpected Alpha bundle identity/policy: {key}")
        env = data.get("LSEnvironment", {})
        for key, value in alpha_environment().items():
            if env.get(key) != value:
                raise ValueError(f"unexpected Alpha environment: {key}")
    elif app == DEV and data.get("CFBundleIdentifier") != "com.novnski.luna.dev":
        raise ValueError("unexpected LUNA Dev bundle identity")
    return data


def alpha_environment():
    return {
        "LUNA_DATA_DIR": str(ALPHA_DATA),
        "LUNA_IPC_PORT": str(ALPHA_PORT),
        "LUNA_WORKTREES_DIR": str(ALPHA_DATA / "worktrees"),
        "LUNA_AUTO_UPDATE": "0",
        "LUNA_QA_MCP_ISOLATION": "0",
        "LUNA_QA_SCOPED_MCP_SERVERS": "",
        "LUNA_EXTENSION_HOST": str(ALPHA / "Contents/MacOS/luna-extension-host"),
        "LUNA_OPENCODE_WORKER": str(ALPHA / "Contents/MacOS/luna-opencode-worker"),
    }


@contextmanager
def app_lock():
    common = Path(run("git", "rev-parse", "--git-common-dir"))
    if not common.is_absolute():
        common = ROOT / common
    lock = common.resolve() / "luna-app-operation.lock"
    try:
        lock.mkdir()
    except FileExistsError:
        raise ValueError(
            f"another LUNA app operation owns {lock}; inspect its pid"
        ) from None
    try:
        (lock / "pid").write_text(str(os.getpid()) + "\n")
        yield
    finally:
        (lock / "pid").unlink(missing_ok=True)
        lock.rmdir()


def preflight_apps():
    for _, app in apps():
        if app not in [DEV, ALPHA]:
            raise ValueError(
                f"another LUNA app is running: {app}; let that app save and quit first"
            )
        info(app)


def quit_apps(except_app=None):
    preflight_apps()
    for pid, app in apps():
        if app == except_app:
            continue
        exe = str(app / "Contents/MacOS/luna")
        subprocess.run(
            [
                "osascript",
                "-l",
                "JavaScript",
                "-e",
                """ObjC.import("AppKit"); function run(argv) {
            const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(Number(argv[0]));
            if (!app.isNil() && ObjC.unwrap(app.executableURL.path) === argv[1]) app.terminate;
        }""",
                str(pid),
                exe,
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            if not any(p == pid for p, _ in apps()):
                break
            time.sleep(1)
        else:
            raise ValueError(
                f"{app.name} declined to quit, possibly because of unsaved work; no app was replaced"
            )


def port_owner(port):
    result = subprocess.run(
        ["lsof", "-nP", f"-iTCP:{port}", "-sTCP:LISTEN", "-t"],
        capture_output=True,
        text=True,
        check=False,
    )
    return [int(pid) for pid in result.stdout.split()]


@contextmanager
def staging_area():
    staging = Path(tempfile.mkdtemp(prefix=".luna-alpha-stage-", dir=ALPHA.parent))
    try:
        yield staging
    finally:
        if (staging / "previous.app").exists():
            print(
                f"Previous Alpha bundle retained for recovery at {staging / 'previous.app'}",
                file=sys.stderr,
            )
        else:
            shutil.rmtree(staging)


def launch(app):
    data = info(app)
    run("codesign", "--verify", "--deep", "--strict", str(app))
    env = data.get("LSEnvironment", {})
    port = int(env["LUNA_IPC_PORT"])
    preflight_apps()
    existing = [pid for pid, candidate in apps() if candidate == app]
    if existing:
        owners = port_owner(port)
        if len(existing) != 1 or (owners and owners != existing):
            raise ValueError("running app does not own its expected IPC listener")
    else:
        if port_owner(port):
            raise ValueError(f"IPC port {port} is occupied; no app was launched")
        args = ["open", "-g", "-n"]
        for key, value in env.items():
            args += ["--env", f"{key}={value}"]
        args.append(str(app))
        run(*args)
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        pids = [pid for pid, candidate in apps() if candidate == app]
        if len(pids) == 1 and port_owner(port) == pids:
            # A PID alone (or an accepting socket) is not a ready engine.
            # The ordinary CLI proves the listener owns this data root too.
            try:
                probe = subprocess.run(
                    [str(app / "Contents/MacOS/luna"), "inspect", "--json", "status"],
                    env=dict(os.environ, **env),
                    capture_output=True,
                    text=True,
                    timeout=5,
                    check=False,
                )
                if probe.returncode == 0 and json.loads(probe.stdout)["data"]["ready"]:
                    return pids[0]
            except (subprocess.TimeoutExpired, ValueError, KeyError):
                pass
        time.sleep(1)
    raise ValueError(
        f"{app.name} did not establish its process, IPC listener, and ready engine"
    )


def build_alpha(update):
    if ALPHA.parent.is_symlink() or ALPHA_DATA.is_symlink():
        raise ValueError(
            "Alpha app/data directories must not redirect through symlinks"
        )
    if ALPHA.exists() and not update:
        raise ValueError(
            "LUNA Alpha is already installed; opening it never rebuilds it. Use update-alpha only when explicitly requested"
        )
    if not ALPHA.exists() and update:
        raise ValueError("LUNA Alpha is not installed; use install-alpha first")
    old_info = info(ALPHA) if ALPHA.exists() else None
    if old_info:
        run("codesign", "--verify", "--deep", "--strict", str(ALPHA))
    if run("git", "branch", "--show-current") != "main" or run(
        "git", "status", "--porcelain"
    ):
        raise ValueError(
            "Alpha builds require committed, clean main; finish and verify development first"
        )
    base = version.check(metadata=True)["version"]
    revision = run("git", "rev-parse", "HEAD")
    previous = (
        json.loads((ALPHA / "Contents/Resources/luna-build.json").read_text())
        if old_info
        else None
    )
    if previous and tuple(map(int, base.split("."))) < tuple(
        map(int, previous["baseVersion"].split("."))
    ):
        raise ValueError(
            "refusing to downgrade the daily app and its saved-data version"
        )
    number = (
        previous["alphaNumber"] + 1
        if previous and previous["baseVersion"] == base
        else 1
    )
    build_number = int(old_info["CFBundleVersion"]) + 1 if old_info else 1
    label = f"{base}-alpha.{number}"
    out = (
        ROOT
        / "target/alpha-builds"
        / (datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + f"-{os.getpid()}")
    )
    out.mkdir(parents=True)
    start = time.monotonic()
    environment = dict(os.environ, CARGO_INCREMENTAL="0")
    environment.pop("CARGO_TARGET_DIR", None)
    with (out / "build.log").open("w") as log:
        result = subprocess.run(
            [
                str(ROOT / "scripts/cargo.sh"),
                "build",
                "--locked",
                "--release",
                "-p",
                "luna",
                "-p",
                "luna-extension-host",
                "-p",
                "luna-engine",
                "--target-dir",
                str(ROOT / "target"),
            ],
            cwd=ROOT,
            env=environment,
            stdout=log,
            stderr=subprocess.STDOUT,
            check=False,
        )
    if result.returncode:
        raise ValueError(
            f"Alpha build failed (exit {result.returncode}); current apps are intact. Log: {out / 'build.log'}"
        )
    if run("git", "rev-parse", "HEAD") != revision or run(
        "git", "status", "--porcelain"
    ):
        raise ValueError(
            "source changed during the Alpha build; current apps are intact"
        )
    ALPHA.parent.mkdir(parents=True, exist_ok=True)
    with staging_area() as staging:
        candidate = staging / "LUNA Alpha.app"
        contents = candidate / "Contents"
        (contents / "MacOS").mkdir(parents=True)
        (contents / "Resources/licenses/fonts").mkdir(parents=True)
        for name in BINARIES:
            shutil.copy2(ROOT / "target/release" / name, contents / "MacOS" / name)
        for license_file in (ROOT / "crates/ui/assets/fonts/licenses").iterdir():
            if license_file.is_file():
                shutil.copy2(
                    license_file,
                    contents / "Resources/licenses/fonts" / license_file.name,
                )
        run(
            "iconutil",
            "-c",
            "icns",
            str(ROOT / "dist/macos/luna.iconset"),
            "-o",
            str(contents / "Resources/luna.icns"),
        )
        data = plistlib.loads(
            (ROOT / "dist/macos/Info.plist")
            .read_bytes()
            .replace(b"__VERSION__", base.encode())
        )
        data.update(
            {
                "CFBundleName": "LUNA Alpha",
                "CFBundleDisplayName": "LUNA Alpha",
                "CFBundleIdentifier": ALPHA_ID,
                "CFBundleShortVersionString": label,
                "CFBundleVersion": str(build_number),
                "LUNARevision": revision,
                "LUNADistribution": "local-alpha",
                "LUNAUpdatePolicy": "manual",
                "LSEnvironment": alpha_environment(),
            }
        )
        manifest = {
            "schemaVersion": 1,
            "version": label,
            "baseVersion": base,
            "alphaNumber": number,
            "buildNumber": build_number,
            "revision": revision,
            "builtAt": datetime.now(timezone.utc).isoformat(),
            "profile": "release",
            "bundleIdentifier": ALPHA_ID,
            "dataRoot": str(ALPHA_DATA),
            "ipcPort": ALPHA_PORT,
            "updatePolicy": "manual",
            "buildSeconds": round(time.monotonic() - start, 2),
        }
        (contents / "Info.plist").write_bytes(plistlib.dumps(data))
        (contents / "Resources/luna-build.json").write_text(
            json.dumps(manifest, indent=2) + "\n"
        )
        identity = os.environ.get("LUNA_ALPHA_CODESIGN_IDENTITY")
        if not identity:
            identities = run("security", "find-identity", "-v", "-p", "codesigning")
            import re

            identity = next(
                iter(re.findall(r'"(Apple Development:[^"]+)"', identities)), "-"
            )
        for name in BINARIES[1:]:
            run(
                "codesign",
                "--force",
                "--sign",
                identity,
                "--identifier",
                f"{ALPHA_ID}.{name}",
                str(contents / "MacOS" / name),
            )
        run(
            "codesign",
            "--force",
            "--sign",
            identity,
            "--identifier",
            ALPHA_ID,
            str(candidate),
        )
        run("codesign", "--verify", "--deep", "--strict", str(candidate))
        # Signatures are now final; keep exact installed executable hashes.
        manifest["binaries"] = {
            name: sha(contents / "MacOS" / name) for name in BINARIES
        }
        (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        if run("git", "rev-parse", "HEAD") != revision or run(
            "git", "status", "--porcelain"
        ):
            raise ValueError(
                "source changed during Alpha staging; installed apps are intact"
            )
        preflight_apps()
        quit_apps()
        if port_owner(ALPHA_PORT):
            raise ValueError(
                f"Alpha IPC {ALPHA_PORT} is occupied; installed apps are intact"
            )
        if old_info:
            backup_env = dict(os.environ, **alpha_environment())
            with (out / "data-backup.log").open("w") as log:
                subprocess.run(
                    [str(ALPHA / "Contents/MacOS/luna"), "backup", "now"],
                    env=backup_env,
                    stdout=log,
                    stderr=subprocess.STDOUT,
                    check=True,
                )
        previous_app = staging / "previous.app"
        if ALPHA.exists():
            ALPHA.rename(previous_app)
        try:
            candidate.rename(ALPHA)
            pid = launch(ALPHA)
        except BaseException:
            # Never remove a running app, and never force a quit for rollback.
            quit_apps()
            if ALPHA.exists():
                ALPHA.rename(staging / "failed.app")
            if previous_app.exists():
                previous_app.rename(ALPHA)
            raise
        manifest["pid"] = pid
        manifest["signature"] = (
            "ad-hoc" if identity == "-" else "Apple Development or supplied identity"
        )
        (out / "result.json").write_text(json.dumps(manifest, indent=2) + "\n")
        if previous_app.exists():
            shutil.rmtree(previous_app)
    print(
        json.dumps(
            {
                "installed": str(ALPHA),
                "version": label,
                "revision": revision,
                "pid": pid,
                "buildSeconds": manifest["buildSeconds"],
                "evidence": str(out),
            },
            indent=2,
        )
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "command",
        choices=["status", "alpha", "dev", "install-alpha", "update-alpha", "quit"],
    )
    args = parser.parse_args()
    try:
        if sys.platform != "darwin":
            raise ValueError("this workflow requires macOS")
        if args.command == "status":
            print(
                json.dumps(
                    {
                        "running": [
                            {"pid": pid, "bundle": str(app)} for pid, app in apps()
                        ],
                        "alpha": str(ALPHA),
                        "alphaData": str(ALPHA_DATA),
                        "dev": str(DEV),
                        "alphaBuild": json.loads(
                            (ALPHA / "Contents/Resources/luna-build.json").read_text()
                        )
                        if ALPHA.exists()
                        else None,
                    },
                    indent=2,
                )
            )
            return
        with app_lock():
            preflight_apps()
            if args.command in ["install-alpha", "update-alpha"]:
                build_alpha(args.command == "update-alpha")
            elif args.command == "quit":
                quit_apps()
            elif args.command == "alpha":
                if not ALPHA.exists():
                    raise ValueError("LUNA Alpha is not installed; use install-alpha")
                info(ALPHA)
                quit_apps(except_app=ALPHA)
                print(json.dumps({"bundle": str(ALPHA), "pid": launch(ALPHA)}))
            elif args.command == "dev":
                quit_apps(except_app=DEV)
        if args.command == "dev":
            # run-macos-dev owns the app-operation lock for its full refresh.
            # A concurrent operation in this handoff fails closed on that lock.
            environment = {
                key: value
                for key, value in os.environ.items()
                if not key.startswith("LUNA_DEV_")
            }
            result = subprocess.run(
                [str(ROOT / "scripts/run-macos-dev.sh"), "--refresh"],
                cwd=ROOT,
                env=environment,
                check=False,
            )
            if result.returncode == 0:
                with app_lock():
                    print(
                        json.dumps(
                            {"bundle": str(DEV), "pid": launch(DEV), "ready": True}
                        )
                    )
            raise SystemExit(result.returncode)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"LUNA app workflow: {error}\n")


if __name__ == "__main__":
    main()
