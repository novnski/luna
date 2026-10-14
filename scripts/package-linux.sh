#!/usr/bin/env bash
# Linux packaging: build the release binary and produce
#   target/package/luna-<version>-linux-<arch>.tar.gz
# containing the binary, the .desktop entry, and the icon, plus an install.sh
# that drops them into ~/.local (XDG) paths.
#
# Usage: scripts/package-linux.sh
# Env:   PROFILE=debug for a fast unoptimized package (CI smoke); default release.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
PROFILE="${PROFILE:-release}"
ARCH="$(uname -m)"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
OUT_DIR="$ROOT/target/package"
STAGE="$OUT_DIR/luna-$VERSION-linux-$ARCH"
TARBALL="$STAGE.tar.gz"

cd "$ROOT"
BUILD=(build --locked -p luna -p luna-extension-host -p luna-engine)
if [[ "$PROFILE" == "release" ]]; then
  BUILD+=(--release)
  BIN="$ROOT/target/release/luna"
else
  BIN="$ROOT/target/debug/luna"
fi
if [[ "${GITHUB_ACTIONS:-}" == true ]]; then
  cargo "${BUILD[@]}"
else
  "$ROOT/scripts/cargo.sh" "${BUILD[@]}"
fi

rm -rf "$STAGE" "$TARBALL"
mkdir -p "$STAGE"
install -m 755 "$BIN" "$STAGE/luna"
for binary in luna-extension-host luna-opencode-worker; do
  install -m 755 "$(dirname "$BIN")/$binary" "$STAGE/$binary"
done
install -m 644 "$ROOT/dist/luna.desktop" "$STAGE/luna.desktop"
install -m 644 "$ROOT/dist/luna.png" "$STAGE/luna.png"
mkdir -p "$STAGE/licenses/fonts"
cp "$ROOT/crates/ui/assets/fonts/licenses/"* "$STAGE/licenses/fonts/"

cat >"$STAGE/install.sh" <<'INSTALL'
#!/usr/bin/env bash
# Install LUNA into ~/.local (no root needed).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
install -Dm755 "$HERE/luna" "$HOME/.local/bin/luna"
for binary in luna-extension-host luna-opencode-worker; do
  install -Dm755 "$HERE/$binary" "$HOME/.local/bin/$binary"
done
install -Dm644 "$HERE/luna.desktop" "$HOME/.local/share/applications/luna.desktop"
install -Dm644 "$HERE/luna.png" "$HOME/.local/share/icons/hicolor/1024x1024/apps/luna.png"
command -v update-desktop-database >/dev/null 2>&1 \
  && update-desktop-database "$HOME/.local/share/applications" || true
echo "Installed. Make sure ~/.local/bin is on your PATH."
INSTALL
chmod 755 "$STAGE/install.sh"

tar -czf "$TARBALL" -C "$OUT_DIR" "$(basename "$STAGE")"
rm -rf "$STAGE"
echo "packaged: $TARBALL"
tar -tzf "$TARBALL"
