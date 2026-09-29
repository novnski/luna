# macOS application icons

LUNA uses Porcelain (02) for light appearance and Monolith (01) for dark
appearance. The running Dock and application icon follow the resolved LUNA
appearance: Light, Dark, or System. The bundle's static Finder/launch icon is
Porcelain, which is also the legacy ICNS fallback.

The editable sources are `dist/macos/icons/Light.icon` and `Dark.icon`. Each
keeps the moon and background as aligned 1024 × 1024 SVG layers. Regenerate
the size-specific artwork and embedded ICNS files on a Mac with Xcode:

```sh
agent-python scripts/generate-macos-icons.py
```

The generator uses Apple's native renderer at each size, then centers an
824/1024 tile in the transparent legacy canvas. Packaging uses
`dist/macos/luna.iconset`; both light and dark multi-resolution ICNS files
are embedded in the macOS UI binary so direct launches and packaged apps use
the same theme behavior. No runtime Xcode or Icon Composer is required.

## End-to-end acceptance cases

Record these failure cases before implementing the appearance integration:

1. The light and dark assets are identical, undecodable, contain the old
   illustrated artwork, omit any ICNS sizes, or have incorrect tile bounds.
2. Startup disagrees with the resolved appearance; pinned Light or Dark
   follows the OS instead of the user's choice.
3. Returning to System keeps a stale pinned icon; subsequent system appearance
   notifications fail to update the native application icon.
4. Repeating the same appearance or changing a theme/accent within the same
   appearance recreates the native image unnecessarily.
5. The packaged light icon differs from the embedded light icon, or the
   refreshed app fails signing, process identity, or IPC checks.

The focused AppKit E2E fixture exercises the production appearance controller
against real `NSApplication.applicationIconImage` readbacks. It retains TIFFs
and normalized PNGs and JSON results. AppKit promotes the running icon to an
extended-range TIFF, so comparison uses premultiplied visible pixels with a
maximum 2/255 quantization tolerance rather than serialized TIFF bytes.
Its GPUI platform is headless: it opens no second LUNA GUI
and uses an isolated temporary settings profile. Run:

```sh
scripts/cargo.sh build --locked -p luna-ui --example app-icon-fixture --features appshots-fixture
target/debug/examples/app-icon-fixture /absolute/path/to/evidence
```

The final handoff requires `scripts/run-macos-dev.sh --refresh`, strict bundle
signature verification, the exact LUNA Dev process, IPC port 59777, and visual
inspection of both designs.
