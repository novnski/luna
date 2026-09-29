#!/usr/bin/env python3
"""Regenerate the selected native icons. Run with agent-python on macOS."""

import json
from pathlib import Path
import shutil
import subprocess
import tempfile

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
DEVELOPER = Path(subprocess.check_output(["xcode-select", "-p"], text=True).strip())
TOOL = DEVELOPER.parent / "Applications/Icon Composer.app/Contents/Executables/ictool"
FRAMES = [(16, 1), (16, 2), (32, 1), (32, 2), (128, 1), (128, 2), (256, 1), (256, 2), (512, 1), (512, 2)]

with tempfile.TemporaryDirectory(prefix="luna-icon-render-") as temporary:
    stage = Path(temporary)
    receipts = []
    for variant in ("Light", "Dark"):
        iconset = stage / f"{variant.lower()}.iconset"
        iconset.mkdir()
        for points, scale in FRAMES:
            size = points * scale
            tile_size = round(size * 824 / 1024)
            native = stage / f"{variant}-{size}.png"
            subprocess.run([
                str(TOOL), str(ROOT / f"dist/macos/icons/{variant}.icon"),
                "--export-image", "--output-file", str(native), "--platform", "macOS",
                "--rendition", "Default", "--width", str(tile_size), "--height", str(tile_size),
                "--scale", "1", "--design-generation", "26",
            ], check=True, capture_output=True)
            tile = Image.open(native).convert("RGBA")
            if tile.size != (tile_size, tile_size) or tile.getbbox() is None:
                raise RuntimeError(f"invalid native export: {native}")
            canvas = Image.new("RGBA", (size, size))
            offset = (size - tile_size) // 2
            canvas.paste(tile, (offset, offset))
            suffix = "@2x" if scale == 2 else ""
            filename = f"icon_{points}x{points}{suffix}.png"
            canvas.save(iconset / filename)
            receipts.append({"variant": variant, "filename": filename, "size": size, "tile": tile_size})
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(stage / f"{variant.lower()}.icns")], check=True)

    # Publish only after both variants fully render and encode successfully.
    for variant in ("Light", "Dark"):
        iconset = ROOT / ("dist/macos/luna.iconset" if variant == "Light" else "dist/macos/luna-dark.iconset")
        iconset.mkdir(parents=True, exist_ok=True)
        for source in (stage / f"{variant.lower()}.iconset").glob("*.png"):
            shutil.copy2(source, iconset / source.name)
        embedded = ROOT / "crates/ui/assets/app-icons"
        embedded.mkdir(parents=True, exist_ok=True)
        shutil.copy2(stage / f"{variant.lower()}.icns", embedded / f"{variant.lower()}.icns")
    print(json.dumps({"ok": True, "frames": receipts}, indent=2))
