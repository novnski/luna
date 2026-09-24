#!/bin/sh
set -eu

# Local, explicitly invoked packaging only. No release feed or upload.
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
case "$(uname -s)" in Darwin) platform=darwin ;; Linux) platform=linux ;; *) echo 'Unsupported platform' >&2; exit 1 ;; esac
case "$(uname -m)" in arm64|aarch64) arch=arm64 ;; x86_64|amd64) arch=x64 ;; *) echo 'Unsupported architecture' >&2; exit 1 ;; esac
CARGO_INCREMENTAL=0 cargo build --locked --release -p luna-headless
out=${LUNA_HEADLESS_ARTIFACT_DIR:-target/headless-artifacts}
mkdir -p "$out"
name="luna-headless-v${version}-${platform}-${arch}"
cp target/release/luna-headless "$out/$name"
if command -v shasum >/dev/null 2>&1; then
  hash=$(shasum -a 256 "$out/$name" | awk '{print $1}')
else
  hash=$(sha256sum "$out/$name" | awk '{print $1}')
fi
printf '%s  %s\n' "$hash" "$name" > "$out/$name.sha256"
printf '%s\n' "$out/$name"
