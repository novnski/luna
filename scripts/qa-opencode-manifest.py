#!/usr/bin/env python3
"""OpenCode QA evidence helpers (docs/reference/qa-infrastructure.md, OC-01).

Subcommands:
  binary [--path P]                 OpenCode binary resolution record (JSON)
  write --dir D --set k=JSON ...    merge fields into D/manifest.json
  scan --dir D [--secret-env N ...] OCF-142 secret scan; exit 1 on findings
  sums --dir D                      write D/SHA256SUMS (shasum -a 256 format)
  verify-sums --dir D               check D/SHA256SUMS

Standard library only. Never prints secret values: the scan reports the rule
or environment variable *name*, the file and the line number.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

PROFILE_MARKER = "# luna-qa-opencode-profile"
PROFILE_TARGET = "# luna-qa-opencode-target: "
NATIVE_MAGIC = (
    b"\xcf\xfa\xed\xfe",  # Mach-O 64 LE
    b"\xce\xfa\xed\xfe",  # Mach-O 32 LE
    b"\xca\xfe\xba\xbe",  # Mach-O universal
    b"\x7fELF",
)
ENV_MUTATION = re.compile(
    r"^\s*(?:export\s+)?((?:XDG_[A-Z_]+|OPENCODE_[A-Z_]+|TMPDIR|HOME))=", re.M
)
EXEC_LINE = re.compile(r"^\s*exec\s+(.+?)\s*$", re.M)
INSTALL_DIRS = (
    "~/.opencode/bin",
    "~/.local/bin",
    "~/.bun/bin",
    "~/.npm-global/bin",
    "/opt/homebrew/bin",
    "/usr/local/bin",
)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


# ---------------------------------------------------------------- binary


def parse_exec_target(script: Path, text: str) -> str | None:
    """The exec target of a simple POSIX wrapper, or None when not static."""
    for raw in EXEC_LINE.findall(text):
        token = raw.split('"$@"')[0].strip()
        token = token.split(" $@")[0].strip()
        dirname = '"$(dirname "$0")"'
        for prefix in ('"$(dirname "$0")/', "$(dirname \"$0\")/", "$(dirname $0)/"):
            if token.startswith(prefix):
                rest = token[len(prefix):].rstrip('"')
                return str(script.parent / rest)
        if token.startswith(dirname + "/"):
            return str(script.parent / token[len(dirname) + 1:].strip('"'))
        token = token.strip('"').strip("'")
        if token.startswith("/"):
            return token
    return None


def wrapper_chain(start: Path, limit: int = 8) -> dict:
    """Follow symlinks and static `exec` wrappers down to a native binary."""
    hops: list[dict] = []
    current = start
    for _ in range(limit):
        hop: dict = {"path": str(current)}
        try:
            st = current.lstat()
        except FileNotFoundError:
            hop["kind"] = "missing"
            hops.append(hop)
            return {"hops": hops, "final": None, "error": f"missing: {current}"}
        if stat.S_ISLNK(st.st_mode):
            target = os.readlink(current)
            resolved = (current.parent / target).resolve() if not os.path.isabs(target) else Path(target)
            hop.update(kind="symlink", target=str(resolved))
            hops.append(hop)
            current = resolved
            continue
        if not stat.S_ISREG(st.st_mode):
            hop["kind"] = "not-a-file"
            hops.append(hop)
            return {"hops": hops, "final": None, "error": f"not a regular file: {current}"}
        hop["sha256"] = sha256_file(current)
        hop["executable"] = os.access(current, os.X_OK)
        with current.open("rb") as handle:
            head = handle.read(4)
        if head in NATIVE_MAGIC:
            hop["kind"] = "native"
            hops.append(hop)
            return {"hops": hops, "final": str(current), "error": None}
        if head[:2] == b"#!":
            text = current.read_text(errors="replace")
            mutated = sorted(set(ENV_MUTATION.findall(text)))
            hop["mutatesEnv"] = mutated
            if PROFILE_MARKER in text:
                hop["kind"] = "luna-qa-profile-wrapper"
                pinned = next(
                    (line[len(PROFILE_TARGET):].strip() for line in text.splitlines() if line.startswith(PROFILE_TARGET)),
                    None,
                )
                target = pinned
            else:
                hop["kind"] = "script"
                target = parse_exec_target(current, text)
            hop["execTarget"] = target
            hops.append(hop)
            if not target:
                return {"hops": hops, "final": None, "error": f"wrapper without a static exec target: {current}"}
            current = Path(target)
            continue
        hop["kind"] = "unknown-format"
        hops.append(hop)
        return {"hops": hops, "final": None, "error": f"unrecognized executable format: {current}"}
    return {"hops": hops, "final": None, "error": "wrapper chain too deep"}


def isolated_version(native: str, timeout: float = 30.0) -> dict:
    """`--version` of a native binary inside a throwaway isolated profile."""
    root = Path(tempfile.mkdtemp(prefix="luna-qa-opencode-version-"))
    env = {k: v for k, v in os.environ.items() if not k.startswith("OPENCODE_")}
    for name in ("config", "data", "cache", "state", "home", "tmp"):
        (root / name).mkdir()
    env.update(
        XDG_CONFIG_HOME=str(root / "config"),
        XDG_DATA_HOME=str(root / "data"),
        XDG_CACHE_HOME=str(root / "cache"),
        XDG_STATE_HOME=str(root / "state"),
        OPENCODE_TEST_HOME=str(root / "home"),
        TMPDIR=str(root / "tmp"),
        OPENCODE_DISABLE_AUTOUPDATE="1",
        OPENCODE_DISABLE_MODELS_FETCH="1",
    )
    try:
        done = subprocess.run(
            [native, "--version"], env=env, cwd=str(root), stdin=subprocess.DEVNULL,
            capture_output=True, text=True, timeout=timeout,
        )
        text = done.stdout.strip().splitlines()
        version = text[-1].strip() if text else ""
        match = re.search(r"(\d+)\.(\d+)\.(\d+)", version)
        return {
            "exit": done.returncode,
            "version": version,
            "major": int(match.group(1)) if match else None,
            "isolated": True,
        }
    except subprocess.TimeoutExpired:
        return {"exit": None, "version": "", "major": None, "isolated": True, "error": "timeout"}
    finally:
        shutil.rmtree(root, ignore_errors=True)


def login_shell_lookup(flags: list[str]) -> str | None:
    shell = os.environ.get("SHELL") or "/bin/zsh"
    try:
        done = subprocess.run(
            [shell, *flags, "command -v opencode"], stdin=subprocess.DEVNULL,
            capture_output=True, text=True, timeout=10,
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    lines = [line.strip() for line in done.stdout.splitlines() if line.strip().startswith("/")]
    return lines[-1] if done.returncode == 0 and lines else None


def candidates() -> list[tuple[str, str]]:
    found: list[tuple[str, str]] = []
    override = os.environ.get("OPENCODE_EXECUTABLE")
    if override:
        found.append(("env:OPENCODE_EXECUTABLE", override))
    for directory in os.environ.get("PATH", "").split(os.pathsep):
        if directory:
            found.append(("process-path", os.path.join(directory, "opencode")))
    for source, flags in (("login-shell -l -i -c", ["-l", "-i", "-c"]), ("login-shell -l -c", ["-l", "-c"])):
        hit = login_shell_lookup(flags)
        if hit:
            found.append((source, hit))
    for directory in INSTALL_DIRS:
        found.append(("install-path", os.path.join(os.path.expanduser(directory), "opencode")))
    seen: set[str] = set()
    unique = []
    for source, path in found:
        if path not in seen and os.path.isfile(path):
            seen.add(path)
            unique.append((source, path))
    return unique


def launch_context() -> str:
    if os.environ.get("SSH_CONNECTION"):
        return "remote"
    if os.environ.get("TERM_PROGRAM") or (os.environ.get("TERM") and sys.stdin.isatty()):
        return "terminal"
    if "application" in os.environ.get("XPC_SERVICE_NAME", ""):
        return "launchd-gui"
    return "headless"


def binary_record(path: str | None) -> dict:
    record: dict = {"generatedAt": now(), "launchContext": launch_context(),
                    "processPath": os.environ.get("PATH", ""), "considered": []}
    pool = [("explicit", path)] if path else candidates()
    chosen = None
    for source, candidate in pool:
        chain = wrapper_chain(Path(candidate))
        entry = {"source": source, "path": candidate, "chain": chain["hops"], "error": chain["error"]}
        if chain["final"]:
            entry["final"] = chain["final"]
            entry["finalSha256"] = chain["hops"][-1].get("sha256")
            entry["version"] = isolated_version(chain["final"])
        record["considered"].append(entry)
        # PD-15: OpenCode 2 only. Keep looking past v1 or broken candidates.
        if chosen is None and entry.get("version", {}).get("major") == 2:
            chosen = entry
            if path:
                break
    if chosen:
        record.update(
            resolved=chosen["path"], resolverSource=chosen["source"], wrapperChain=chosen["chain"],
            final=chosen["final"], finalSha256=chosen["finalSha256"], version=chosen["version"],
            envMutatingHops=[hop["path"] for hop in chosen["chain"] if hop.get("mutatesEnv")],
        )
    else:
        record.update(resolved=None, error="no OpenCode 2 binary found (PD-15: v1 is not supported)")
    return record


# ---------------------------------------------------------------- manifest


def set_dotted(target: dict, dotted: str, value) -> None:
    keys = dotted.split(".")
    for key in keys[:-1]:
        target = target.setdefault(key, {})
        if not isinstance(target, dict):
            raise SystemExit(f"cannot set {dotted}: {key} is not an object")
    target[keys[-1]] = value


def cmd_write(args) -> int:
    path = Path(args.dir) / "manifest.json"
    manifest = json.loads(path.read_text()) if path.exists() else {}
    for item in args.set or []:
        key, _, raw = item.partition("=")
        set_dotted(manifest, key, json.loads(raw))
    for item in args.file or []:
        key, _, source = item.partition("=")
        set_dotted(manifest, key, json.loads(Path(source).read_text()))
    manifest.setdefault("schema", "luna-opencode-qa-manifest/1")
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    tmp.replace(path)
    return 0


# ---------------------------------------------------------------- scan

PATTERNS = {
    "authorization-header-value": re.compile(r"(?i)authorization['\"]?\s*[:=]\s*['\"]?(basic|bearer)\s+[A-Za-z0-9+/=._-]{8,}"),
    "basic-credential-url": re.compile(r"https?://[^/\s:@]+:[^/\s@]{6,}@"),
    "opencode-password-assignment": re.compile(r"OPENCODE_(?:SERVER_)?PASSWORD\s*[=:]\s*['\"]?[A-Za-z0-9+/=_-]{8,}"),
    "openai-style-key": re.compile(r"\bsk-(?:proj-|ant-)?[A-Za-z0-9_-]{20,}"),
    "github-token": re.compile(r"\bgh[pousr]_[A-Za-z0-9]{30,}"),
    "private-key-block": re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    # /api/model returns provider settings including apiKey; only redacted
    # values and known non-secret placeholders may appear in evidence.
    "secret-named-field": re.compile(
        r"(?i)[\"'](?:api_?key|auth_?token|access_?token|refresh_?token|password|secret)[\"']\s*:\s*[\"']"
        r"(?!<redacted>|public[\"']|luna-qa-mock-key-not-secret[\"']|[\"'])"),
}
SKIP_SCAN = {"SHA256SUMS", "secret-scan.json"}


def cmd_scan(args) -> int:
    root = Path(args.dir)
    secrets = {}
    for name in args.secret_env or []:
        value = os.environ.get(name)
        if value and len(value) >= 6:
            secrets[f"env:{name}"] = value
    for item in args.secret_file or []:
        value = Path(item).read_text().strip()
        if len(value) >= 6:
            secrets[f"file:{Path(item).name}"] = value
    findings = []
    scanned = 0
    for path in sorted(p for p in root.rglob("*") if p.is_file() and not p.is_symlink()):
        if path.name in SKIP_SCAN or path.stat().st_size > 64 << 20:
            continue
        scanned += 1
        text = path.read_bytes().decode("utf-8", errors="replace")
        rel = str(path.relative_to(root))
        for number, line in enumerate(text.splitlines(), 1):
            for rule, pattern in PATTERNS.items():
                if pattern.search(line):
                    findings.append({"file": rel, "line": number, "rule": rule})
            for label, value in secrets.items():
                if value in line:
                    findings.append({"file": rel, "line": number, "rule": f"known-secret {label}"})
    report = {"scannedAt": now(), "filesScanned": scanned, "knownSecrets": sorted(secrets),
              "rules": sorted(PATTERNS), "findings": findings, "passed": not findings}
    (root / "secret-scan.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"passed": report["passed"], "findings": len(findings), "filesScanned": scanned}))
    return 0 if report["passed"] else 1


# ---------------------------------------------------------------- sums


def cmd_sums(args) -> int:
    root = Path(args.dir)
    lines = []
    for path in sorted(p for p in root.rglob("*") if p.is_file() and not p.is_symlink()):
        rel = path.relative_to(root).as_posix()
        if rel == "SHA256SUMS":
            continue
        lines.append(f"{sha256_file(path)}  {rel}")
    (root / "SHA256SUMS").write_text("\n".join(lines) + "\n")
    print(f"{len(lines)} files hashed into {root / 'SHA256SUMS'}")
    return 0


def cmd_verify_sums(args) -> int:
    root = Path(args.dir)
    bad = 0
    for line in (root / "SHA256SUMS").read_text().splitlines():
        digest, _, rel = line.partition("  ")
        target = root / rel
        if not target.is_file() or sha256_file(target) != digest:
            print(f"MISMATCH {rel}")
            bad += 1
    print(json.dumps({"verified": bad == 0, "mismatches": bad}))
    return 0 if bad == 0 else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    binary = sub.add_parser("binary")
    binary.add_argument("--path")
    binary.add_argument("--out")
    write = sub.add_parser("write")
    write.add_argument("--dir", required=True)
    write.add_argument("--set", action="append")
    write.add_argument("--file", action="append")
    scan = sub.add_parser("scan")
    scan.add_argument("--dir", required=True)
    scan.add_argument("--secret-env", action="append")
    scan.add_argument("--secret-file", action="append")
    sums = sub.add_parser("sums")
    sums.add_argument("--dir", required=True)
    verify = sub.add_parser("verify-sums")
    verify.add_argument("--dir", required=True)
    args = parser.parse_args()
    if args.command == "binary":
        record = binary_record(args.path)
        text = json.dumps(record, indent=2) + "\n"
        if args.out:
            Path(args.out).write_text(text)
        print(text, end="")
        return 0 if record.get("resolved") else 1
    return {"write": cmd_write, "scan": cmd_scan, "sums": cmd_sums, "verify-sums": cmd_verify_sums}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
