#!/usr/bin/env bash
# Rebuildable local candidate only. Never installs, notarizes, uploads, or uses a release feed.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

for tool in cargo git iconutil plutil codesign ditto shasum jq; do
  command -v "$tool" >/dev/null || { echo "missing tool: $tool" >&2; exit 1; }
done
if ! git diff --quiet HEAD --; then
  echo "commit tracked source changes before packaging so the revision identifies the binary" >&2
  exit 1
fi
git ls-files --error-unmatch scripts/package-luna-local.sh >/dev/null || {
  echo "commit the package script before packaging" >&2
  exit 1
}

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
revision="$(git rev-parse HEAD)"
short_revision="$(git rev-parse --short=12 HEAD)"
arch="$(uname -m)"
case "$arch" in arm64|x86_64) ;; *) echo "unsupported macOS architecture: $arch" >&2; exit 1 ;; esac
case "$version" in ''|*[!0-9.]*) echo "invalid workspace version: $version" >&2; exit 1 ;; esac

out="$root/target/package/l32-local"
candidate="$out/luna-v${version}+${short_revision}-macos-${arch}"
mkdir -p "$out"
if [[ -e "$candidate" ]]; then
  echo "candidate already exists: $candidate" >&2
  exit 1
fi

# Ignore caller-provided target and signing/notary settings for this local candidate.
env -u CARGO_TARGET_DIR CARGO_INCREMENTAL=0 cargo build --locked --release -p luna
stage="$(mktemp -d "$out/.stage.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
app="$stage/LUNA.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources/licenses/fonts"
install -m 755 "$root/target/release/luna" "$app/Contents/MacOS/luna"
cp "$root/crates/ui/assets/fonts/licenses/"* "$app/Contents/Resources/licenses/fonts/"
iconutil -c icns "$root/dist/macos/luna.iconset" -o "$app/Contents/Resources/luna.icns"
sed "s/__VERSION__/$version/g" "$root/dist/macos/Info.plist" > "$app/Contents/Info.plist"
plutil -insert LUNAUpstreamBase -string "$version" "$app/Contents/Info.plist"
plutil -insert LUNARevision -string "$revision" "$app/Contents/Info.plist"
plutil -insert LUNADistribution -string "local-ad-hoc" "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

codesign --force --sign - --identifier com.novnski.luna "$app"
codesign --verify --deep --strict "$app"
ditto -c -k --keepParent "$app" "$stage/LUNA.app.zip"
binary_sha="$(shasum -a 256 "$app/Contents/MacOS/luna" | awk '{print $1}')"
zip_sha="$(shasum -a 256 "$stage/LUNA.app.zip" | awk '{print $1}')"

cat > "$stage/manifest.json" <<EOF
{
  "schemaVersion": 1,
  "audience": "local-ad-hoc",
  "upstreamBaseVersion": "$version",
  "lunaRevision": "$revision",
  "displayVersion": "$version+luna.$short_revision",
  "architecture": "$arch",
  "bundleIdentifier": "com.novnski.luna",
  "defaultDataRoot": "~/.luna",
  "defaultIpcPort": 59778,
  "updates": "disabled-no-owned-feed",
  "signature": "ad-hoc",
  "binarySha256": "$binary_sha",
  "zipSha256": "$zip_sha"
}
EOF
jq -e . "$stage/manifest.json" >/dev/null

if [[ -e "$candidate" ]]; then
  echo "candidate appeared during packaging: $candidate" >&2
  exit 1
fi
mv "$stage" "$candidate"
trap - EXIT
echo "$candidate"
