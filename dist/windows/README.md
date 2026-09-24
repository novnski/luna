# Windows icon

`luna.ico` contains 16, 24, 32, 48, 64, 128, and 256 pixel PNG frames.
Each frame preserves the artwork's transparency.

`apps/luna/build.rs` compiles `luna.rc` into the Windows executable for
both debug and release builds. Resource ID 1 is required by GPUI's Windows
icon loader. No installer or adjacent image file is needed at runtime.
