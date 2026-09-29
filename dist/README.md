# Packaging

## Linux (implemented)

```sh
scripts/package-linux.sh            # release build (thin LTO, stripped)
PROFILE=debug scripts/package-linux.sh   # fast smoke package
```

Produces `target/package/luna-<version>-linux-<arch>.tar.gz` containing:

- `luna` — the binary (headed by default; `luna headless` runs the engine alone)
- `luna.desktop` — XDG desktop entry
- `luna.png` — the preserved upstream 1024×1024 app artwork
- `install.sh` — installs into `~/.local/{bin,share/applications,share/icons}`

The release profile in the root `Cargo.toml` sets `lto = "thin"` and
`strip = "symbols"` for distribution builds.

## macOS

```sh
scripts/package-macos.sh    # → target/package/luna-<version>-macos-<arch>.dmg
```

Builds the release binary, assembles `LUNA.app` (Info.plist + icns), ad-hoc
signs it (set `CODESIGN_IDENTITY` for a Developer ID signature), and wraps it in
a dmg. It also produces an app tarball for in-app updates. The GitHub Release
workflow signs the update manifest with the project's own Ed25519 key; Apple
signing and notarization are optional and do not host or authorize updates.
See [GitHub release updates](../docs/reference/github-release-updates.md) for
release setup and operation. The manual packaging steps, for reference
(run on a macOS host — gpui needs Metal; no cross-build from Linux):

1. Build the universal (or per-arch) binary:
   ```sh
   cargo build --release -p luna --target aarch64-apple-darwin
   cargo build --release -p luna --target x86_64-apple-darwin
   lipo -create -output luna \
     target/aarch64-apple-darwin/release/luna \
     target/x86_64-apple-darwin/release/luna
   ```
2. Assemble the bundle:
   ```sh
   mkdir -p LUNA.app/Contents/{MacOS,Resources}
   cp luna LUNA.app/Contents/MacOS/luna
   sed "s/__VERSION__/$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')/" \
     dist/macos/Info.plist > LUNA.app/Contents/Info.plist
   ```
3. Icon: generate `luna.icns` from `dist/macos/luna.iconset`. Each frame uses
   size-specific Porcelain light artwork, centered on an 824/1024
   (80.47%) legacy-ICNS tile within its transparent canvas. This keeps the
   flattened icon's apparent size aligned with native macOS icons on systems
   that don't apply the newer Icon Composer mask and scale automatically.
   Place it at
   `LUNA.app/Contents/Resources/luna.icns`:
   ```sh
   iconutil -c icns dist/macos/luna.iconset \
     -o LUNA.app/Contents/Resources/luna.icns
   ```
   The running app switches to Monolith for dark appearance, including the
   resolved System setting. Both multi-resolution variants are embedded in
   the binary. See [app icons](../docs/reference/app-icons.md) for editable
   Icon Composer sources, regeneration, and native readback QA.
4. Optionally sign + notarize to reduce macOS first-launch warnings:
   ```sh
   codesign --deep --force --options runtime --sign "Developer ID Application: …" LUNA.app
   xcrun notarytool submit LUNA.zip --keychain-profile … --wait
   xcrun stapler staple LUNA.app
   ```
5. Ship as a `.dmg` (`hdiutil create -volname LUNA -srcfolder LUNA.app -ov -format UDZO LUNA.dmg`).
