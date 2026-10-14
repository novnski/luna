#!/usr/bin/env python3
"""Create and Ed25519-sign the exact GitHub Release update manifest bytes."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

REPOSITORY = "novnski/luna"
PLATFORMS = (
    "linux-x86_64.tar.gz",
    "linux-aarch64.tar.gz",
    "macos-arm64.dmg",
    "macos-arm64-app.tar.gz",
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--assets", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--private-key", required=True, type=Path)
    args = parser.parse_args()

    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", args.version):
        parser.error("version must be a three-part numeric release version")
    if not args.private_key.is_file():
        parser.error("Ed25519 private key file is required")
    if not args.assets.is_dir():
        parser.error("assets directory does not exist")

    public_key = Path(__file__).resolve().parents[1] / "dist/update-signing-public.hex"
    pinned_key = public_key.read_text(encoding="ascii").strip()
    public_der = subprocess.run(
        ["openssl", "pkey", "-in", str(args.private_key), "-pubout", "-outform", "DER"],
        check=True,
        capture_output=True,
    ).stdout
    if (
        len(public_der) != 44
        or public_der[:12].hex() != "302a300506032b6570032100"
        or public_der[12:].hex() != pinned_key
    ):
        parser.error("release signing key does not match the pinned Ed25519 public key")

    expected = {f"luna-{args.version}-{platform}" for platform in PLATFORMS}
    actual = {path.name for path in args.assets.iterdir() if path.is_file()}
    if actual != expected:
        parser.error(
            f"release assets mismatch: missing={sorted(expected - actual)}, "
            f"unexpected={sorted(actual - expected)}"
        )

    files = {}
    for name in sorted(expected):
        asset = args.assets / name
        if asset.is_symlink() or asset.stat().st_size == 0:
            parser.error(f"release asset must be a nonempty regular file: {name}")
        with asset.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        files[name] = {
            "sha256": digest,
            "url": f"https://github.com/{REPOSITORY}/releases/download/v{args.version}/{name}",
        }

    args.output.mkdir(parents=True, exist_ok=True)
    manifest = args.output / "manifest.json"
    signature = args.output / "manifest.sig"
    manifest.write_bytes(
        (json.dumps({"version": args.version, "files": files}, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
    )
    try:
        subprocess.run(
            ["openssl", "pkeyutl", "-sign", "-rawin", "-inkey", str(args.private_key), "-in", str(manifest), "-out", str(signature)],
            check=True,
        )
        if signature.stat().st_size != 64:
            raise ValueError("Ed25519 signature is not 64 bytes")
    except Exception:
        manifest.unlink(missing_ok=True)
        signature.unlink(missing_ok=True)
        raise

    print(f"Signed {manifest} ({len(files)} assets) and {signature}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
