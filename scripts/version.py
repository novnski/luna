#!/usr/bin/env python3
"""Check or bump LUNA's product version without resolving new dependencies."""

import argparse
import fcntl
import json
import os
import re
import subprocess
import tempfile
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parents[1]
CORE = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
IOS = Path("apps/ios/Luna.xcodeproj/project.pbxproj")


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def product():
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    version = manifest["workspace"]["package"]["version"]
    if not re.fullmatch(CORE, version):
        raise ValueError("workspace version must be numeric MAJOR.MINOR.PATCH")
    names = set()
    for member in manifest["workspace"]["members"]:
        package = tomllib.loads((ROOT / member / "Cargo.toml").read_text())["package"]
        if package["version"] == {"workspace": True}:
            names.add(package["name"])
    return version, names


def check(tag=None, metadata=False):
    version, names = product()
    locked = tomllib.loads((ROOT / "Cargo.lock").read_text())["package"]
    owned = {
        p["name"]: p["version"]
        for p in locked
        if p["name"] in names and "source" not in p
    }
    if set(owned) != names or any(v != version for v in owned.values()):
        raise ValueError("Cargo.lock workspace versions disagree with Cargo.toml")
    marketing = re.findall(r"MARKETING_VERSION = ([^;]+);", (ROOT / IOS).read_text())
    if not marketing or any(v != version for v in marketing):
        raise ValueError("iOS marketing versions disagree with the workspace version")
    if tag and tag != f"v{version}":
        raise ValueError(f"tag {tag!r} does not match v{version}")
    if metadata:
        data = json.loads(
            run(
                str(ROOT / "scripts/cargo.sh"),
                "metadata",
                "--locked",
                "--no-deps",
                "--format-version",
                "1",
            )
        )
        members = set(data["workspace_members"])
        for package in data["packages"]:
            if (
                package["id"] in members
                and package["name"] in names
                and package["version"] != version
            ):
                raise ValueError("Cargo metadata disagrees with the product version")
    return {
        "version": version,
        "workspacePackages": len(names),
        "iosConfigurations": len(marketing),
    }


def atomic_write(path, text):
    with tempfile.NamedTemporaryFile(
        "w", dir=path.parent, prefix=".version-", delete=False
    ) as f:
        temporary = Path(f.name)
        f.write(text)
    try:
        temporary.chmod(path.stat().st_mode & 0o777)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def bump(kind, dry_run):
    old, names = product()
    check()
    major, minor, patch = map(int, old.split("."))
    new = {
        "patch": f"{major}.{minor}.{patch + 1}",
        "minor": f"{major}.{minor + 1}.0",
        "major": f"{major + 1}.0.0",
    }[kind]
    originals = {
        p: (ROOT / p).read_text() for p in [Path("Cargo.toml"), Path("Cargo.lock"), IOS]
    }
    updates = {}
    updates[Path("Cargo.toml")] = re.sub(
        r'(\[workspace\.package\]\s*\nversion = ")' + re.escape(old) + '"',
        lambda m: m[1] + new + '"',
        originals[Path("Cargo.toml")],
        count=1,
    )
    blocks = originals[Path("Cargo.lock")].split("[[package]]")
    for i, block in enumerate(blocks[1:], 1):
        package = tomllib.loads(block)
        if package["name"] in names and "source" not in package:
            blocks[i] = re.sub(
                r'^version = "' + re.escape(old) + r'"$',
                f'version = "{new}"',
                block,
                count=1,
                flags=re.MULTILINE,
            )
    updates[Path("Cargo.lock")] = "[[package]]".join(blocks)
    updates[IOS] = re.sub(
        r"MARKETING_VERSION = [^;]+;", f"MARKETING_VERSION = {new};", originals[IOS]
    )
    for path, text in updates.items():
        if text == originals[path]:
            raise ValueError(f"version replacement did not update {path}")
    summary = {
        "from": old,
        "to": new,
        "bump": kind,
        "dryRun": dry_run,
        "files": [str(p) for p in updates],
    }
    if not dry_run:
        common = Path(run("git", "rev-parse", "--git-common-dir"))
        if not common.is_absolute():
            common = ROOT / common
        with (
            (common / "luna-version.lock").open("a+") as lock,
            (common / "luna-build-budget.lock").open("a+") as build_lock,
        ):
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(build_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            for path, text in originals.items():
                if (ROOT / path).read_text() != text:
                    raise ValueError(
                        "version files changed concurrently; retry after reviewing"
                    )
            try:
                for path, text in updates.items():
                    atomic_write(ROOT / path, text)
                check(metadata=True)
            except BaseException:
                for path, text in originals.items():
                    atomic_write(ROOT / path, text)
                raise
    return summary


def release_plan():
    result = check(metadata=True)
    if run("git", "branch", "--show-current") != "main" or run(
        "git", "status", "--porcelain"
    ):
        raise ValueError("release planning requires committed, clean main")
    revision = run("git", "rev-parse", "HEAD")
    remote = run("git", "ls-remote", "origin", "refs/heads/main").split()[0]
    if remote != revision:
        raise ValueError("push and verify main before planning a public release")
    tag = "v" + result["version"]
    if run("git", "tag", "--list", tag):
        raise ValueError("this version already has a tag; bump before a new release")
    if run("git", "ls-remote", "origin", f"refs/tags/{tag}"):
        raise ValueError("this version already has a remote tag; never replace it")
    result.update(
        {
            "revision": revision,
            "tag": tag,
            "githubPrerelease": result["version"].startswith("0."),
            "publishes": False,
            "next": f"Only with explicit release authorization: git tag -a {tag} -m 'LUNA {result['version']}' && git push origin {tag}",
        }
    )
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("show")
    validate = sub.add_parser("check")
    validate.add_argument("--tag")
    validate.add_argument("--metadata", action="store_true")
    change = sub.add_parser("bump")
    change.add_argument("kind", choices=["patch", "minor", "major"])
    change.add_argument("--dry-run", action="store_true")
    sub.add_parser("release-plan")
    args = parser.parse_args()
    try:
        if args.command == "show":
            print(product()[0])
            return
        result = {
            "check": lambda: check(args.tag, args.metadata),
            "bump": lambda: bump(args.kind, args.dry_run),
            "release-plan": release_plan,
        }[args.command]()
        print(json.dumps(result, indent=2))
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Version workflow: {error}\n")


if __name__ == "__main__":
    main()
